//! The feed that Brigade's server keeps on an embedded driver, call for call (work
//! item 362; ADR 0138, condition 9). Brigade is the team-messaging server whose ordered
//! log is one recorded Cavalry publication: `server/crates/brigade-log/src/feed.rs` of
//! `github.com/appshapes/brigade`. This module is that file's use of Cavalry and
//! nothing else of it, so that a change here that would break the server breaks a test
//! of `just test` first. `docs/explanation/what-brigades-server-relies-on.md` lists
//! the calls and says what to do when one must change.
//!
//! The imports below are the server's own: the client and the driver from `cavalry`,
//! and the catalog's reader from `cavalry-log`, because the client lists no recordings.
//!
//! The feed, as the server keeps it:
//!
//! - One exclusive publication on a `cavalry:udp` control channel in manual control
//!   mode, recorded. Every run of the server is a new recording, and the feed is still
//!   one line of positions: each run's publication is created where the last recording
//!   stopped, by `init-term-id=0`, `term-id` and `term-offset`.
//! - An append offers its records, waits until the recorder has written them, asks for
//!   a sync and returns when the sync has. What the server then answers is on the disk.
//! - A start reads the earlier recordings back in order, each through a replay onto
//!   `cavalry:ipc`.
//! - A roll stops the recording and begins the next where it stopped, with no restart;
//!   a release purges the stopped recordings that a snapshot covers, but for the newest.
//! - A client that the driver closed is replaced by a new one, and the feed goes on
//!   where its recording stopped.
//!
//! [`Shape`] is the one thing the server changes between the Cavalry it is pinned to
//! and the one it moves to.

// Each test program that takes this module uses a part of it.
#![allow(dead_code)]

pub mod process;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry::{
    Cavalry, Driver, Flags, Image, NULL_LENGTH, Offer, Publication, Recording, ReplayStopReason,
    Subscription,
};
use cavalry_log::catalog::CatalogReader;
use cavalry_log::layout::State;

/// The stream the feed is published on.
pub const FEED_STREAM: i32 = 10;

/// The stream a start replays the feed's recordings on.
pub const REPLAY_STREAM: i32 = 11;

/// The channel a start replays on: inside the one driver, with no socket.
pub const REPLAY_CHANNEL: &str = "cavalry:ipc";

/// How long an append, a connection or a quiet replay is waited for.
pub const PATIENCE: Duration = Duration::from_secs(15);

/// How the feed is shaped by the Cavalry the server is built against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    /// What the feed's channel adds after its control mode.
    pub channel_keys: &'static str,
    /// The term length the position keys count in, in bytes.
    pub term_length: u64,
    /// Whether the server reads its own feed so that the feed takes appends.
    pub own_subscription: bool,
}

/// The shape of the server at its pin, 620a2e29: a publication with no connected
/// receiver refuses every offer and a recording does not count as one, so the server
/// subscribes to its own feed, added as a destination; and a channel names no term
/// length, so a log is three terms of 16 MiB.
pub const PINNED: Shape = Shape {
    channel_keys: "",
    term_length: 16 << 20,
    own_subscription: true,
};

/// The shape the server moves to: the recording connects the publication (`ssc=true`,
/// ADR 0098) and the channel names terms of 1 MiB (`term-length`, ADR 0099), so the
/// server reads no feed of its own.
pub const MOVED_TO: Shape = Shape {
    channel_keys: "|ssc=true|term-length=1m",
    term_length: 1 << 20,
    own_subscription: false,
};

/// What the feed is opened with.
#[derive(Debug, Clone)]
pub struct FeedOptions {
    /// The driver directory.
    pub driver_dir: PathBuf,
    /// The log directory: the recordings and their catalog.
    pub log_dir: PathBuf,
    /// The UDP address of the feed, `127.0.0.1:<port>`.
    pub control: String,
    /// The UDP address the server reads its own feed on, while the shape needs it.
    pub own_endpoint: String,
    /// How many bytes a recording holds before a release rolls the feed.
    pub roll_bytes: u64,
    /// The shape.
    pub shape: Shape,
}

/// One recording of the feed, from the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recorded {
    /// Its id, the catalog's slot.
    pub id: i64,
    /// Where it starts.
    pub start: i64,
    /// Where it stopped; `None` while it is live.
    pub stop: Option<i64>,
}

/// The feed's publication, its recording and what keeps it connected. All of it
/// belongs to one client and goes when that client does.
struct Feed {
    publication: Publication,
    recording: Recording,
    own: Option<Subscription>,
}

/// The log as a Cavalry feed.
pub struct FeedLog {
    // Dropped in this order: the feed before its client, the client before its driver.
    feed: Option<Feed>,
    client: Option<Cavalry>,
    driver: Option<Driver>,
    options: FeedOptions,
    /// Where this run's recording began: the end of everything the earlier recordings
    /// hold.
    begin: i64,
    /// The position after the last record appended.
    end: i64,
    max_record_bytes: usize,
    /// What the operator should be told, until the caller asks.
    notes: Vec<String>,
}

fn failed(doing: &str, error: impl std::fmt::Display) -> String {
    format!("{doing}: {error}")
}

/// The recordings of the feed that the catalog holds as valid, in the order of their
/// positions.
pub fn recordings(log_dir: &Path) -> Result<Vec<Recorded>, String> {
    let doing = "reading the feed's catalog";
    let catalog = CatalogReader::open(log_dir).map_err(|error| failed(doing, error))?;
    let slots = catalog.slots().map_err(|error| failed(doing, error))?;
    let mut found = Vec::new();
    for id in 0..i64::try_from(slots).unwrap_or(i64::MAX) {
        if let Some(descriptor) = catalog.get(id).map_err(|error| failed(doing, error))?
            && descriptor.state() == State::Valid
            && descriptor.stream_id() == FEED_STREAM
        {
            let stop = (!descriptor.is_live()).then_some(descriptor.stop_position());
            found.push(Recorded {
                id,
                start: descriptor.start_position(),
                stop,
            });
        }
    }
    found.sort_by_key(|recorded| (recorded.start, recorded.id));
    Ok(found)
}

/// Reads the server's own subscription to its feed, and throws what it reads away. A
/// receiver that never reads holds its sender at the window.
fn drain(feed: &mut Feed) {
    if let Some(own) = &mut feed.own {
        while matches!(own.poll(|_, _| {}, 64), Ok(read) if read > 0) {}
    }
}

/// Adds the feed's publication where its last recording stopped, records it, and
/// connects it. Returns the feed and the position it begins at.
fn add_feed(client: &mut Cavalry, options: &FeedOptions) -> Result<(Feed, i64), String> {
    let doing = "adding the feed";
    // A recording that is still live is an earlier client's, which the driver has not
    // timed out yet. It is stopped first: read before that, the stop is too low, and
    // the new publication would repeat positions the old recording holds.
    for recorded in recordings(&options.log_dir)? {
        if recorded.stop.is_none() {
            client
                .stop_recording(recorded.id)
                .map_err(|error| failed("stopping an earlier recording of the feed", error))?;
        }
    }
    let begin = recordings(&options.log_dir)?
        .iter()
        .filter_map(|recorded| recorded.stop)
        .max()
        .unwrap_or(0);
    let term = i64::try_from(options.shape.term_length).unwrap_or(i64::MAX);
    let channel = format!(
        "cavalry:udp?control={}|control-mode=manual{}|init-term-id=0|term-id={}|term-offset={}",
        options.control,
        options.shape.channel_keys,
        begin / term,
        begin % term
    );
    let publication = client
        .add_exclusive_publication(&channel, FEED_STREAM)
        .map_err(|error| failed(doing, error))?;
    let recording = client
        .start_recording(&channel, FEED_STREAM)
        .map_err(|error| failed(doing, error))?;
    let own = if options.shape.own_subscription {
        let endpoint = format!("cavalry:udp?endpoint={}", options.own_endpoint);
        let own = client
            .add_subscription(&endpoint, FEED_STREAM)
            .map_err(|error| failed(doing, error))?;
        client
            .add_destination(publication.registration_id(), &endpoint)
            .map_err(|error| failed(doing, error))?;
        Some(own)
    } else {
        None
    };
    let mut feed = Feed {
        publication,
        recording,
        own,
    };
    let deadline = Instant::now() + PATIENCE;
    while !feed.publication.is_connected() {
        if Instant::now() > deadline {
            return Err(format!("the feed on {} did not connect", options.control));
        }
        drain(&mut feed);
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok((feed, begin))
}

impl FeedLog {
    /// Starts the driver, connects to it and adds the feed where its last recording
    /// stopped.
    pub fn open(options: &FeedOptions) -> Result<Self, String> {
        let driver = Driver::start(&options.driver_dir, &options.log_dir)
            .map_err(|error| failed("starting the Cavalry driver", error))?;
        let mut client = Cavalry::connect_to(&options.driver_dir)
            .map_err(|error| failed("connecting to the Cavalry driver", error))?;
        let (feed, begin) = add_feed(&mut client, options)?;
        let max_record_bytes = feed.publication.max_message_length();
        Ok(Self {
            feed: Some(feed),
            client: Some(client),
            driver: Some(driver),
            options: options.clone(),
            begin,
            end: begin,
            max_record_bytes,
            notes: Vec::new(),
        })
    }

    /// Where this run's recording began.
    pub fn begin(&self) -> i64 {
        self.begin
    }

    /// The position after the last record appended.
    pub fn end(&self) -> i64 {
        self.end
    }

    /// The longest record the feed takes.
    pub fn max_record_bytes(&self) -> usize {
        self.max_record_bytes
    }

    /// The feed's channel as this run's publication was added.
    pub fn channel(&self) -> Option<&str> {
        self.feed.as_ref().map(|feed| feed.publication.channel())
    }

    /// The id of the recording this run writes.
    pub fn recording_id(&self) -> Option<i64> {
        self.feed.as_ref().map(|feed| feed.recording.recording_id())
    }

    /// The session of the feed's publication, and the session its recording records.
    /// They are one session when the recording is of this run's publication.
    pub fn sessions(&self) -> Option<(i32, i32)> {
        self.feed
            .as_ref()
            .map(|feed| (feed.publication.session_id(), feed.recording.session_id()))
    }

    /// What the operator should be told, since the last call.
    pub fn notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// A new client of the driver this process still runs, and the feed again where it
    /// stopped.
    ///
    /// A process that was held still for longer than Cavalry's liveness timeouts, as a
    /// virtual machine is while it is moved, comes back with its client closed. The
    /// driver goes on; everything the client held is gone with it. Returns where the
    /// feed stopped.
    fn reconnect(&mut self) -> Result<i64, String> {
        self.feed = None;
        self.client = None;
        let driver_dir = self
            .driver
            .as_ref()
            .map(|driver| driver.dir().to_path_buf());
        let driver_dir = driver_dir.ok_or_else(|| "the Cavalry driver is gone".to_owned())?;
        let mut client = Cavalry::connect_to(&driver_dir)
            .map_err(|error| failed("connecting to the Cavalry driver again", error))?;
        let (feed, stopped_at) = add_feed(&mut client, &self.options)?;
        self.feed = Some(feed);
        self.client = Some(client);
        Ok(stopped_at)
    }

    /// Takes the feed's client away as the driver's timeout of it does, and starts
    /// again: nothing of the feed is stopped or closed by its own handle, and the
    /// client is then closed, which has the driver drop everything the client
    /// registered, as the driver does for a client it times out. Then a new client
    /// connects and adds the feed again, as the server's reconnect does. Returns the
    /// feed's recordings as the catalog held them before the client went, with the
    /// feed's own still live, and where the feed stopped.
    ///
    /// Once the client has gone, its publication is at its end of stream, and the
    /// recorder stops the recording there by itself. So the new start finds the
    /// earlier recording live or already stopped, as the two threads fall, and stops
    /// it in the first case. The feed begins at the same position in both.
    ///
    /// The server does not call this. It is the state its reconnect meets after a
    /// stall, made without the stall: `tests/feed_held_still.rs` makes it with one.
    pub fn start_again_after_the_client_went(&mut self) -> Result<(Vec<Recorded>, i64), String> {
        if let Some(feed) = self.feed.take() {
            std::mem::forget(feed);
        }
        let left = recordings(&self.options.log_dir)?;
        if let Some(client) = self.client.take() {
            client
                .close()
                .map_err(|error| failed("closing the earlier client", error))?;
        }
        let stopped_at = self.reconnect()?;
        self.begin = stopped_at;
        self.end = stopped_at;
        Ok((left, stopped_at))
    }

    /// Starts again beside the feed's client, which stays: nothing is stopped and
    /// nothing is closed, the earlier client and what it held are forgotten and never
    /// dropped, so no close reaches the driver and the earlier client's conductor
    /// keeps it alive there for as long as this process runs. Then a new client
    /// connects and adds the feed again. Returns where the feed stopped.
    ///
    /// The server does not call this either. It is the state its reconnect meets when
    /// its client closed on its own side and the driver has not timed it out yet,
    /// which lasts up to the client liveness timeout of 10 s.
    pub fn start_again_beside_the_earlier_client(&mut self) -> Result<i64, String> {
        if let Some(feed) = self.feed.take() {
            std::mem::forget(feed);
        }
        if let Some(client) = self.client.take() {
            std::mem::forget(client);
        }
        let stopped_at = self.reconnect()?;
        self.begin = stopped_at;
        self.end = stopped_at;
        Ok(stopped_at)
    }

    /// Ends the feed's recording and begins the next one where it stopped, with no
    /// restart: stop the recording, close the publication, and add the feed again at
    /// the stop position. The order matters. A stop cuts the recording at what the
    /// recorder has written, and every append waited for the recorder, so that is
    /// everything published.
    fn roll(&mut self) -> Result<(), String> {
        let doing = "rolling the feed's recording";
        let (Some(feed), Some(client)) = (self.feed.take(), &mut self.client) else {
            return Err("the feed is closed".to_owned());
        };
        feed.recording
            .stop(client)
            .map_err(|error| failed(doing, error))?;
        let deadline = Instant::now() + PATIENCE;
        while feed.recording.stop_position().is_none() {
            if Instant::now() > deadline {
                return Err("the feed's recording did not report its stop in time".to_owned());
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        if let Some(own) = feed.own {
            // The destination belongs to the send endpoint, which outlives the
            // publication for a moment: taken off here, it is added again with the new
            // publication like the first time.
            let endpoint = format!("cavalry:udp?endpoint={}", self.options.own_endpoint);
            let _ = client.remove_destination(feed.publication.registration_id(), &endpoint);
            let _ = own.close();
        }
        feed.publication
            .close()
            .map_err(|error| failed(doing, error))?;
        let (feed, begin) = add_feed(client, &self.options)?;
        self.feed = Some(feed);
        if begin != self.end {
            return Err(format!(
                "the feed's recording stopped at {begin} and its last record ends at {}",
                self.end
            ));
        }
        self.begin = begin;
        Ok(())
    }

    /// Offers the records from `offered.len()` on and waits until they are synced.
    /// `offered` takes the position after each record that was offered, so that after
    /// a reconnect the caller can tell which of them the feed holds.
    fn offer_and_sync(
        &mut self,
        records: &[Vec<u8>],
        offered: &mut Vec<i64>,
    ) -> Result<(), String> {
        let (Some(feed), Some(client)) = (&mut self.feed, &mut self.client) else {
            return Err("the feed is closed".to_owned());
        };
        let deadline = Instant::now() + PATIENCE;
        for record in &records[offered.len()..] {
            loop {
                match feed.publication.offer(record) {
                    Ok(after) => {
                        offered.push(after);
                        break;
                    }
                    // Not connected for the moment a recording takes to go live,
                    // back-pressured while the recorder has a window to write, or the
                    // end of a term: each passes.
                    Err(Offer::NotConnected | Offer::BackPressured | Offer::AdminAction)
                        if Instant::now() < deadline =>
                    {
                        drain(feed);
                        std::thread::sleep(Duration::from_micros(200));
                    }
                    Err(refusal) => {
                        return Err(failed("the feed refused a record", refusal.reason()));
                    }
                }
            }
            drain(feed);
        }
        let end = offered.last().copied().unwrap_or(self.end);
        // The recorder is a thread of the driver's: once it has written up to the last
        // record, one sync covers the batch.
        while feed.recording.position() < end {
            if client.is_closed() {
                return Err("closed".to_owned());
            }
            if Instant::now() > deadline {
                return Err("the feed's recorder did not reach the records in time".to_owned());
            }
            drain(feed);
            std::thread::sleep(Duration::from_micros(50));
        }
        feed.recording
            .sync(client)
            .map_err(|error| failed("syncing the feed's recording", error))?;
        if feed.recording.synced_position() < end {
            return Err("the feed's recording was synced short of the records".to_owned());
        }
        drain(feed);
        self.end = end;
        Ok(())
    }

    /// Appends the records and returns the position after each, once all of them are
    /// synced: what the server answers after this is on the disk.
    pub fn append(&mut self, records: &[Vec<u8>]) -> Result<Vec<i64>, String> {
        let mut offered = Vec::with_capacity(records.len());
        let mut attempts = 0;
        // Everything up to here was synced and answered.
        let answered = self.end;
        loop {
            match self.offer_and_sync(records, &mut offered) {
                Ok(()) => return Ok(offered),
                Err(error) => {
                    let closed = self.client.as_ref().is_none_or(Cavalry::is_closed);
                    attempts += 1;
                    if !closed || attempts > 3 {
                        return Err(error);
                    }
                    // The client was closed under the append. The feed stopped where
                    // its recording did: the records offered at or before that are in
                    // it, and the rest are offered again.
                    let stopped_at = self.reconnect()?;
                    if stopped_at < answered {
                        // The catalog holds no stop for the recording, as when its own
                        // stop could not be written: a feed that went on from here
                        // would use positions of answered records.
                        return Err(format!(
                            "the feed stopped at position {stopped_at}, before records that were \
                             answered up to {answered}"
                        ));
                    }
                    offered.retain(|after| *after <= stopped_at);
                    self.end = stopped_at;
                    self.notes.push(format!(
                        "the Cavalry client was closed under an append; connected again, and the \
                         feed goes on at position {stopped_at}"
                    ));
                }
            }
        }
    }

    /// Plays one recording from `from` to its stop into `each`, and returns the
    /// position it ended at.
    fn play(
        client: &mut Cavalry,
        subscription: &mut Subscription,
        recorded: &Recorded,
        from: i64,
        stop: i64,
        each: &mut dyn FnMut(&[u8], i64),
    ) -> Result<i64, String> {
        let doing = "reading the feed back";
        let replay = client
            .start_replay(
                recorded.id,
                from,
                NULL_LENGTH,
                REPLAY_STREAM,
                REPLAY_CHANNEL,
            )
            .map_err(|error| failed(doing, error))?;
        let session = replay.session_id();
        let (mut record, mut whole) = (Vec::new(), false);
        let mut arithmetic = None;
        let mut heard = Instant::now();
        // The position after the last frame this reader was handed.
        let mut delivered = from;
        loop {
            // The images first, so that the replay's position arithmetic is in hand
            // before its frames.
            subscription
                .poll(|_, _| {}, 0)
                .map_err(|error| failed(doing, error))?;
            let image = subscription
                .images()
                .iter()
                .find(|image| image.session_id() == session);
            if arithmetic.is_none() {
                arithmetic = image.map(Image::positions);
            }
            let (reached, in_sight) = (image.map(Image::position), image.is_some());
            let mut read = 0;
            if let Some(positions) = &arithmetic {
                read = subscription
                    .poll(
                        |body, header| {
                            if header.session_id() != session {
                                return;
                            }
                            // From the image's arithmetic and not a sum of lengths: a
                            // padding frame at the end of a term moves the position
                            // and reaches no handler.
                            let offset = u32::try_from(header.term_offset()).unwrap_or(0);
                            let after = positions.position(header.term_id(), offset)
                                + i64::try_from(header.aligned_length()).unwrap_or(0);
                            delivered = after;
                            // The frames of one record are consecutive: the first says
                            // so, and the last. A record whose last frame never came,
                            // as a kill in the middle of an offer leaves, is dropped
                            // when the next one begins.
                            if header.flags().contains(Flags::BEGIN) {
                                record.clear();
                                whole = true;
                            }
                            if !whole {
                                return;
                            }
                            record.extend_from_slice(body);
                            if header.flags().contains(Flags::END) {
                                whole = false;
                                each(&record, after);
                            }
                        },
                        64,
                    )
                    .map_err(|error| failed(doing, error))?;
            }
            if read > 0 {
                heard = Instant::now();
                continue;
            }
            if let Some((end, reason)) = replay.ended() {
                // As the server has it, and with its gap. `Replay::ended` asks a reader
                // to keep the image's position from each poll; this one keeps the
                // frames' alone, and reads the image's in this pass only. A recording
                // that ends in padding is so read to its end only while the image is
                // still in sight: the page's "What does not hold".
                //
                // The replay is read to its end when the last frame handed over ends
                // there, or the image stands there: the image's own position counts
                // the padding that reaches no handler. Nothing else is the end. An
                // image that is not in sight yet has delivered nothing, and one that
                // has gone may have gone unread: the driver lets go of a replay's
                // image a few seconds after the replay stops, whether or not its
                // subscriber read it.
                if delivered >= end || reached.is_some_and(|reached| reached >= end) {
                    return match reason {
                        Some(ReplayStopReason::Ended) | None if end >= stop => Ok(end),
                        _ => Err(format!(
                            "the replay of recording {} ended at position {end}, before the \
                             recording's stop at {stop}",
                            recorded.id
                        )),
                    };
                }
                if arithmetic.is_some() && !in_sight {
                    return Err(format!(
                        "the replay of recording {} was let go of at position {delivered}, \
                         before it was read to its end at {end}",
                        recorded.id
                    ));
                }
            }
            if heard.elapsed() > PATIENCE {
                return Err(format!(
                    "the replay of recording {} stopped arriving",
                    recorded.id
                ));
            }
            std::thread::sleep(Duration::from_micros(200));
        }
    }

    /// Reads the feed from `from` to where this run began, each record with the
    /// position after it, through a replay of each earlier recording in order.
    /// Returns the position this run began at.
    pub fn read_from(
        &mut self,
        from: i64,
        each: &mut dyn FnMut(&[u8], i64),
    ) -> Result<i64, String> {
        let mut position = from;
        if position > self.begin {
            return Err(format!(
                "position {position} is past the feed's end, {}",
                self.begin
            ));
        }
        let Some(client) = &mut self.client else {
            return Err("the feed is closed".to_owned());
        };
        if position < self.begin {
            let doing = "reading the feed back";
            let mut subscription = client
                .add_subscription(REPLAY_CHANNEL, REPLAY_STREAM)
                .map_err(|error| failed(doing, error))?;
            for recorded in recordings(&self.options.log_dir)? {
                let Some(stop) = recorded
                    .stop
                    .filter(|stop| *stop > position && *stop > recorded.start)
                else {
                    continue;
                };
                if recorded.start > position {
                    return Err(format!(
                        "the feed no longer holds position {position}: its earliest recording \
                         after it begins at {}",
                        recorded.start
                    ));
                }
                position = Self::play(client, &mut subscription, &recorded, position, stop, each)?;
                if let Some(feed) = &mut self.feed {
                    drain(feed);
                }
            }
            let _ = subscription.close();
        }
        if position != self.begin {
            return Err(format!(
                "the feed's recordings end at {position} and the feed begins at {}",
                self.begin
            ));
        }
        Ok(self.begin)
    }

    /// Lets go of what a snapshot at `before` covers: rolls the feed when this run's
    /// recording holds the roll's bytes, then purges every stopped recording that
    /// stops at or before `before`, but for the newest.
    pub fn release(&mut self, before: i64) -> Result<(), String> {
        let held = u64::try_from(self.end - self.begin).unwrap_or(0);
        if held >= self.options.roll_bytes && held > 0 {
            self.roll()?;
            self.notes.push(format!(
                "the feed's recording was rolled at position {}",
                self.begin
            ));
        }
        let mut stopped: Vec<(i64, i64)> = recordings(&self.options.log_dir)?
            .iter()
            .filter_map(|recorded| recorded.stop.map(|stop| (stop, recorded.id)))
            .collect();
        stopped.sort_unstable();
        // The newest stopped recording is kept whatever the snapshots cover: where the
        // feed begins at the next start is read from it.
        stopped.pop();
        let Some(client) = &mut self.client else {
            return Err("the feed is closed".to_owned());
        };
        let mut gone = 0;
        for (stop, id) in stopped {
            if stop > before {
                continue;
            }
            // A purge that is refused, as it is while a replay still reads the
            // recording, is sent again after the next snapshot.
            match client.purge_recording(id) {
                Ok(()) => gone += 1,
                Err(error) => self.notes.push(format!(
                    "recording {id} of the feed was not let go of: {error}"
                )),
            }
        }
        if gone > 0 {
            self.notes.push(format!(
                "let go of {gone} recordings of the feed, which the snapshots cover up to \
                 position {before}"
            ));
        }
        Ok(())
    }
}

impl Drop for FeedLog {
    fn drop(&mut self) {
        // Every append was synced before it returned, so a stop cuts the recording at
        // what was answered.
        if let (Some(feed), Some(client)) = (self.feed.take(), &mut self.client) {
            let _ = feed.recording.stop(client);
            if let Some(own) = feed.own {
                let _ = own.close();
            }
            let _ = feed.publication.close();
        }
        if let Some(client) = self.client.take() {
            let _ = client.close();
        }
        if let Some(driver) = self.driver.take() {
            let _ = driver.close();
        }
    }
}

/// Record `number` of a test's feed: `length` bytes that say which record it is, so
/// that a record read back at the wrong place, or twice, is seen.
pub fn record(number: u64, length: usize) -> Vec<u8> {
    let mut state = number.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xC0FF_EE00_0000_0362;
    let mut bytes = Vec::with_capacity(length);
    for _ in 0..length {
        // xorshift64: every byte depends on the record's number.
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.push(state.to_le_bytes()[0]);
    }
    bytes
}
