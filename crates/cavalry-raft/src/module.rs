//! The consensus module: one member's [`Consensus`] run over a driver, as a client of
//! it (ADR 0063). It gives the core its [`Io`] through the commands every program
//! uses: the consensus messages on a publication to each peer and a subscription of
//! its own, the leader's log as a publication with every other member as a
//! destination, created at the term's base log position, and a recording of it that
//! begins there, a follower's log as a subscription with a recording armed on it, and
//! the record of terms in the `terms` file of the member's log directory. What the
//! core counts it stores in counters it adds to the driver's counters file, where
//! `cavalry doctor` reads them.
//!
//! One thread drives a module: [`ConsensusModule::do_work`] once per duty cycle. A
//! cycle reads the clock once, allocates nothing and sends no command; the commands, a
//! round trip each, come at the member's start and where its election adds or leaves
//! the log.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cavalry_client::{
    ArmedRecording, Cavalry, ClientError, Counter, Fault, Flags, Offer, Publication, Recording,
    Subscription,
};
use cavalry_log::terms::{NO_RECORDING, TermsError, TermsFile};
use cavalry_proto::cluster::{
    CONSENSUS_STREAM_ID, EMPTY_MEMBER, Endpoint, LOG_STREAM_ID, MAX_MEMBERS, MembersError, Message,
    Stream, parse_members,
};
use cavalry_proto::udp::UDP_TERM_LENGTH;
use cavalry_shm::counters::CounterId;
use cavalry_shm::counters::generated::{
    CLUSTER_APPEND_POSITION, CLUSTER_COMMIT_POSITION, CLUSTER_ELECTION_STATE,
    CLUSTER_LEADERSHIP_TERM_ID, CLUSTER_LOG_RECORDING_ID, CLUSTER_MESSAGES_REJECTED, CLUSTER_ROLE,
    COUNTERS,
};

use crate::consensus::{
    Clock, Consensus, Counts, ElectionState, Io, Log, MembersRefused, Refused, Role,
};

/// The longest payload [`ConsensusModule::offer`] takes: what one frame of the log
/// carries after the frame header and the session message header, at the UDP
/// transport's MTU length of 1,408 bytes. An entry is one frame in this version, so
/// that no entry lies across the commit position.
pub const MAX_PAYLOAD_LENGTH: usize = 1408 - 32 - 32;

/// How many consensus messages one duty cycle reads.
const CONSENSUS_POLL_LIMIT: usize = 16;

/// How many fragments of the log one duty cycle of a follower's module reads past.
const LOG_POLL_LIMIT: usize = 64;

/// How long a member leaving the log waits for its recording's stop position.
const STOP_TIMEOUT: Duration = Duration::from_secs(1);
/// No sync asked for yet: no position a recording has written.
const NO_SYNC_ASKED: i64 = -1;

const DOC: &str = "docs/reference/cluster.md#roles-and-the-election";
const DOC_MEMBERS: &str = "docs/reference/cluster.md#members-and-streams";

/// Why a consensus module did not start, or stopped.
#[derive(Debug)]
pub enum RaftError {
    /// The members string was refused.
    Members(MembersError),
    /// The member id is not one of the string's.
    Member(MembersRefused),
    /// The member's log directory holds a term or a vote: a member of this version
    /// does not start again on its log (`spec/cluster.md`, "The appointed leader").
    Restart {
        /// The highest term in the record, −1 for none.
        log_leadership_term_id: i64,
        /// The candidate term stored, −1 for none.
        candidate_term: i64,
    },
    /// The `terms` file could not be read or written.
    Terms(TermsError),
    /// The driver refused a command, or the client is closed.
    Client(ClientError),
    /// A poll of the consensus stream or of the log stopped: an image that failed a
    /// check, or a client that is closed.
    Poll(Fault),
    /// A step this version does not take.
    Unsupported {
        /// What was asked.
        what: &'static str,
    },
    /// The payload offered is longer than [`MAX_PAYLOAD_LENGTH`].
    PayloadTooLong {
        /// Its length.
        length: usize,
    },
    /// The offer was refused by the member's state; offer again as the reason says.
    Offer(Refused),
    /// A member leaving the log stopped the recording it held, and 1 s later had not
    /// read where the recording stopped. It holds the recording still, in init, and the
    /// next duty cycle stops it again and waits for its stop.
    StopTimeout {
        /// The recording held.
        recording_id: i64,
    },
}

impl RaftError {
    /// The reason's name: `members`, `member`, `restart`, `terms`, `client`, `poll`,
    /// `unsupported`, `payload-too-long`, `offer` or `stop-timeout`.
    pub const fn reason(&self) -> &'static str {
        match self {
            RaftError::Members(_) => "members",
            RaftError::Member(_) => "member",
            RaftError::Restart { .. } => "restart",
            RaftError::Terms(_) => "terms",
            RaftError::Client(_) => "client",
            RaftError::Poll(_) => "poll",
            RaftError::Unsupported { .. } => "unsupported",
            RaftError::PayloadTooLong { .. } => "payload-too-long",
            RaftError::Offer(_) => "offer",
            RaftError::StopTimeout { .. } => "stop-timeout",
        }
    }
}

impl core::fmt::Display for RaftError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let reason = self.reason();
        match self {
            RaftError::Members(error) => {
                write!(f, "member refused ({reason}): {error}; see {DOC_MEMBERS}")
            }
            RaftError::Member(error) => write!(f, "member refused ({reason}): {error}"),
            RaftError::Restart {
                log_leadership_term_id,
                candidate_term,
            } => write!(
                f,
                "member refused ({reason}): the log directory holds term \
                 {log_leadership_term_id} and candidate term {candidate_term}, and a member \
                 of this version does not start again on its log: that needs the election \
                 and the catch-up, which are not built yet. Start the cluster again as a new \
                 cluster, every member on a new log directory; a new log directory on one \
                 member alone joins nothing; see {DOC}"
            ),
            RaftError::Terms(error) => write!(f, "member stopped ({reason}): {error}"),
            RaftError::Client(error) => write!(f, "member stopped ({reason}): {error}"),
            RaftError::Poll(fault) => write!(f, "member stopped ({reason}): {fault}"),
            RaftError::Unsupported { what } => write!(
                f,
                "member stopped ({reason}): {what} is not in this version; start the cluster \
                 as a new cluster, every member on a new log directory; see {DOC}"
            ),
            RaftError::PayloadTooLong { length } => write!(
                f,
                "offer refused ({reason}): the payload is {length} bytes and an entry carries \
                 at most {MAX_PAYLOAD_LENGTH}; send it in parts of that length; see {DOC}"
            ),
            RaftError::Offer(refused) => write!(f, "{refused}"),
            RaftError::StopTimeout { recording_id } => write!(
                f,
                "member stopped ({reason}): the member stopped recording {recording_id} as \
                 it left the log, and 1 s later the driver had not said where it stopped, so \
                 the member does not know where its log ends; it stays in init holding the \
                 recording. Call do_work again, which stops it again and waits; if this \
                 repeats, run `cavalry doctor`: the driver or this process is starved of CPU; \
                 see {DOC}"
            ),
        }
    }
}

impl std::error::Error for RaftError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RaftError::Members(error) => Some(error),
            RaftError::Member(error) => Some(error),
            RaftError::Terms(error) => Some(error),
            RaftError::Client(error) => Some(error),
            RaftError::Poll(fault) => Some(fault),
            RaftError::Offer(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ClientError> for RaftError {
    fn from(error: ClientError) -> Self {
        RaftError::Client(error)
    }
}

impl From<TermsError> for RaftError {
    fn from(error: TermsError) -> Self {
        RaftError::Terms(error)
    }
}

/// The two clocks of a duty cycle: a monotonic one since the module started, and the
/// wall clock's milliseconds, the cluster's time.
fn read_clock(started: Instant) -> Clock {
    Clock {
        now_ns: u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        time_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
            }),
    }
}

/// `cavalry:udp?endpoint=<host>:<port>` for an endpoint of the members string.
fn endpoint_channel(endpoint: &Endpoint<'_>) -> String {
    format!("cavalry:udp?endpoint={}", host_port(endpoint))
}

/// `<host>:<port>`, an IPv6 literal in its square brackets again.
fn host_port(endpoint: &Endpoint<'_>) -> String {
    let host = String::from_utf8_lossy(endpoint.host);
    if host.contains(':') {
        format!("[{host}]:{}", endpoint.port)
    } else {
        format!("{host}:{}", endpoint.port)
    }
}

/// Another member as this one reaches it.
struct Peer {
    id: i32,
    /// The publication on the consensus stream to the peer's `consensus` endpoint.
    consensus: Publication,
    /// The peer's `log` endpoint as a destination of the leader's log.
    log_destination: String,
}

/// A recording of the log this member made, with the position its image joined at.
struct Held {
    recording: Recording,
    start: i64,
}

/// The image of the log on a follower's subscription, as the module noted it.
#[derive(Clone, Copy)]
enum LogImage {
    /// No image has joined the subscription yet.
    Awaited,
    /// The first image that joined the subscription, noted after the poll that applied
    /// it and before a frame of it was read.
    Joined {
        /// The id the driver gave the image, which tells it from a later image of the
        /// same session.
        correlation_id: i64,
        session_id: i32,
        join_position: i64,
    },
    /// The image noted is no longer available. No other image of the subscription is
    /// noted or read until the member leaves the log, so that the core sees the loss.
    Gone,
}

/// The core's [`Io`] over the client.
struct ClientIo {
    client: Cavalry,
    terms: TermsFile,
    peers: Vec<Peer>,
    /// The leader's log channel: the control channel on this member's `log` endpoint.
    log_control_channel: String,
    /// A follower's log channel: this member's `log` endpoint.
    log_endpoint_channel: String,
    scratch: Box<[u8; 1408]>,
    // The leader's log.
    log_publication: Option<Publication>,
    /// The term `log_publication` was created for.
    log_term: Option<i64>,
    destinations_added: usize,
    log_recording: Option<Recording>,
    // A follower's log.
    log_subscription: Option<Subscription>,
    armed: Option<ArmedRecording>,
    /// The image on the log subscription and where it joined.
    image: LogImage,
    held: Option<Held>,
    /// The end of the contiguous span the recordings this member has stopped hold.
    stopped_at: i64,
    /// The position its recording of the log had written when this member last asked
    /// the recorder to sync it ([`ask_sync`]).
    sync_asked_at: i64,
}

/// Asks the driver to sync `recording`, this member's recording of the log, when the
/// recorder has written past what it has synced, without waiting for the answer
/// (`spec/client-protocol.md`, "Commands", sync recording; ADR 0060, ADR 0069). The
/// member's append position is the recording's synced position, so without the
/// request an entry counts toward a commit at the recorder's next sync of its own,
/// up to a second later. It asks once for each position written that it sees, never
/// once per duty cycle: while the sync runs the recorder writes nothing, the next
/// block it takes holds everything that arrived meanwhile, and one request then
/// covers that block, so the syncs group the entries by themselves. A request the
/// driver refuses, its recorder's queue full, is not asked again until the position
/// written moves; the recorder's own sync covers what it left within a second.
fn ask_sync(client: &mut Cavalry, recording: &Recording, asked_at: &mut i64) {
    let written = recording.position();
    if written > recording.synced_position()
        && written != *asked_at
        && recording.request_sync(client).is_ok()
    {
        *asked_at = written;
    }
}

impl ClientIo {
    /// Takes the images the driver announced or withdrew on the log subscription, notes
    /// a new image's join position before a frame of it is read, hands over an armed
    /// recording the driver has started, and reads past what has arrived of the image
    /// noted, so that the stream is not held back by a subscriber that stands still:
    /// the module acts on no entry in this version, and the recording is the member's
    /// log.
    ///
    /// An image is never read in the poll that applies it. The client's conductor runs
    /// on a thread of its own and hands an image over whenever it has read the driver's
    /// `available image`, between any two polls, and every poll applies what was handed
    /// over before it reads. A poll that read every image would read up to
    /// `LOG_POLL_LIMIT` frames of an image it had just applied, and the position
    /// noted on the next cycle would be past the image's join: a join position mismatch
    /// counted for a follower whose log had none, and a recording never counted, since
    /// it would not begin where it was noted to. So the poll with a fragment limit of 0
    /// applies and reads nothing, the image is noted after it, and the read is a bounded
    /// poll of the noted session alone, which reads no image of another session that
    /// the conductor hands over meanwhile. An image of the same session that the
    /// conductor hands over in place of the noted one, which that read may read, is
    /// never noted: the noted image is gone by the next cycle, and nothing is noted on
    /// the subscription after it.
    fn observe_log(&mut self) -> Result<(), RaftError> {
        let Some(subscription) = self.log_subscription.as_mut() else {
            return Ok(());
        };
        subscription.poll(|_, _| {}, 0).map_err(RaftError::Poll)?;
        self.image = match self.image {
            LogImage::Awaited => subscription
                .images()
                .first()
                .map_or(LogImage::Awaited, |image| LogImage::Joined {
                    correlation_id: image.correlation_id(),
                    session_id: image.session_id(),
                    join_position: image.position(),
                }),
            LogImage::Joined { correlation_id, .. }
                if subscription
                    .images()
                    .iter()
                    .any(|image| image.correlation_id() == correlation_id) =>
            {
                self.image
            }
            LogImage::Joined { .. } | LogImage::Gone => LogImage::Gone,
        };
        let LogImage::Joined {
            session_id,
            join_position,
            ..
        } = self.image
        else {
            return Ok(());
        };
        subscription
            .bounded_poll(session_id, |_, _| {}, i64::MAX, LOG_POLL_LIMIT)
            .map_err(RaftError::Poll)?;
        if let Some(armed) = self.armed.as_mut()
            && let Some(recording) = armed.poll()?
        {
            self.held = Some(Held {
                recording,
                start: join_position,
            });
            self.armed = None;
        }
        Ok(())
    }
}

impl Io for ClientIo {
    type Fault = RaftError;

    fn send(&mut self, member: i32, message: &Message<'_>) -> bool {
        let Some(peer) = self.peers.iter_mut().find(|peer| peer.id == member) else {
            return false;
        };
        let mut buffer = [0u8; 128];
        let Ok(length) = message.write(&mut buffer) else {
            return false;
        };
        buffer
            .get(..length)
            .is_some_and(|bytes| peer.consensus.offer(bytes).is_ok())
    }

    fn store_candidate_term(&mut self, term: i64) -> Result<(), Self::Fault> {
        Ok(self.terms.set_candidate_term(term)?)
    }

    fn record_term(&mut self, term: i64, base: i64) -> Result<(), Self::Fault> {
        Ok(self.terms.append(term, base, NO_RECORDING)?)
    }

    fn add_log(&mut self, term: i64, base: i64) -> Result<Option<Log>, Self::Fault> {
        // The publication held is the one of the term it was created for, at that
        // term's base: a call for the same term is a step tried again, and one for
        // another term would append that term's entries where the earlier term's
        // publication stands. A member that led an earlier term in this process gives
        // its publication and its recording up before it leads again, which is the
        // step-down's to do (ADR 0077).
        if self.log_term.is_some_and(|held| held != term) {
            return Err(RaftError::Unsupported {
                what: "a term led by a member that still holds the log publication of an \
                       earlier term",
            });
        }
        if self.log_publication.is_none() {
            // A term's publication is created at the term's base, so that every frame
            // of the term lies at its position in the cluster's log, and the recording
            // started below begins there (`spec/cluster.md`, state 5; ADR 0059).
            let Some(channel) = log_channel_at(&self.log_control_channel, base) else {
                return Err(RaftError::Unsupported {
                    what: "a term whose base no publication's position keys can name",
                });
            };
            self.log_publication = Some(
                self.client
                    .add_exclusive_publication(&channel, LOG_STREAM_ID)?,
            );
            self.log_term = Some(term);
        }
        let Some(publication) = self.log_publication.as_ref() else {
            return Ok(None);
        };
        while let Some(peer) = self.peers.get(self.destinations_added) {
            self.client
                .add_destination(publication.registration_id(), &peer.log_destination)?;
            self.destinations_added += 1;
        }
        if self.log_recording.is_none() {
            self.log_recording = Some(
                self.client
                    .start_recording(&self.log_control_channel, LOG_STREAM_ID)?,
            );
        }
        Ok(self.log_recording.as_ref().map(|recording| Log {
            session_id: publication.session_id(),
            recording_id: recording.recording_id(),
        }))
    }

    fn join_log(&mut self) -> Result<(), Self::Fault> {
        if self.log_subscription.is_none() {
            self.log_subscription = Some(
                self.client
                    .add_subscription(&self.log_endpoint_channel, LOG_STREAM_ID)?,
            );
        }
        if self.armed.is_none() && self.held.is_none() {
            self.armed = Some(
                self.client
                    .arm_recording(&self.log_endpoint_channel, LOG_STREAM_ID)?,
            );
        }
        Ok(())
    }

    fn log_image(&mut self, session_id: i32) -> Result<Option<i64>, Self::Fault> {
        Ok(match self.image {
            LogImage::Joined {
                session_id: noted,
                join_position,
                ..
            } if noted == session_id => Some(join_position),
            _ => None,
        })
    }

    /// Stops the recording held and takes its stop as the log's end, then drops the
    /// subscription. The recording stays held until its stop has been read: a stop the
    /// driver refused or did not answer, or a stop position not read within
    /// `STOP_TIMEOUT`, returns with nothing else changed. The core then stays in init
    /// with the log joined and calls again on the next cycle, which stops the recording
    /// again, a success with nothing written for one already stopped, and waits again.
    /// So a leave that fails never loses the recording whose stop is the log's end, and
    /// it never takes a synced position short of the stop.
    fn leave_log(&mut self) -> Result<(), Self::Fault> {
        if let Some(held) = self.held.as_ref() {
            // A stop already read, the driver's at the image's end or this member's own
            // on a cycle before, needs no command.
            if held.recording.stop_position().is_none() {
                self.client.stop_recording(held.recording.recording_id())?;
            }
            let deadline = Instant::now() + STOP_TIMEOUT;
            let stop = loop {
                if let Some(stop) = held.recording.stop_position() {
                    break stop;
                }
                if Instant::now() >= deadline {
                    return Err(RaftError::StopTimeout {
                        recording_id: held.recording.recording_id(),
                    });
                }
                std::thread::sleep(Duration::from_micros(50));
            };
            // The recording's stop is where this member's log ends when the recording
            // began where the log ended before it; one that began past it is no part of
            // the member's contiguous log.
            if held.start == self.stopped_at {
                self.stopped_at = stop;
            }
            self.held = None;
        }
        // The driver drops a recording still armed with the stream's last subscription.
        self.armed = None;
        self.log_subscription = None;
        self.image = LogImage::Awaited;
        Ok(())
    }

    fn append_position(&mut self) -> i64 {
        if let Some(recording) = &self.log_recording {
            ask_sync(&mut self.client, recording, &mut self.sync_asked_at);
            return recording.synced_position();
        }
        match &self.held {
            Some(held) if held.start == self.stopped_at => {
                ask_sync(&mut self.client, &held.recording, &mut self.sync_asked_at);
                held.recording.synced_position()
            }
            _ => self.stopped_at,
        }
    }

    fn append(&mut self, entry: &Message<'_>) -> Result<bool, Self::Fault> {
        let Some(publication) = self.log_publication.as_mut() else {
            return Ok(false);
        };
        let Ok(length) = entry.write(self.scratch.as_mut_slice()) else {
            return Ok(false);
        };
        let Some(bytes) = self.scratch.get(..length) else {
            return Ok(false);
        };
        match publication.offer(bytes) {
            Ok(_) => Ok(true),
            Err(Offer::BackPressured | Offer::NotConnected | Offer::AdminAction) => Ok(false),
            Err(other) => Err(RaftError::Client(ClientError::Driver {
                what: format!("the log publication refused an entry: {other}"),
            })),
        }
    }
}

/// The channel of a log publication whose first frame lies at `base`: `control`, the
/// member's log control channel, as it is for a base of 0, where a publication's log
/// begins unless its channel says otherwise, and for any other base `control` with
/// the three position keys, an `init-term-id` of 0, the whole terms below `base` as
/// `term-id` and the rest of `base` as `term-offset` (`spec/udp.md`, "The sender";
/// ADR 0059). `None` for a base below 0 and for one whose terms are more than a
/// `term-id` holds, 2,147,483,647 terms of 16 MiB.
fn log_channel_at(control: &str, base: i64) -> Option<String> {
    if base == 0 {
        return Some(control.to_owned());
    }
    if base < 0 {
        return None;
    }
    let term_length = i64::from(UDP_TERM_LENGTH);
    let term_id = i32::try_from(base.checked_div(term_length)?).ok()?;
    let term_offset = base.checked_rem(term_length)?;
    Some(format!(
        "{control}|init-term-id=0|term-id={term_id}|term-offset={term_offset}"
    ))
}

/// One of the module's counters with the value it last stored.
struct Stored {
    counter: Counter,
    last: i64,
}

impl Stored {
    /// Adds the counter of type `id` for `member_id` and stores `value` in it at once,
    /// so that the 0 a counter holds between its allocation and its first store is
    /// never read as a value: term 0 is a real term, and recording 0 a real recording.
    fn add(
        client: &mut Cavalry,
        id: CounterId,
        name: &str,
        member_id: i32,
        value: i64,
    ) -> Result<Self, ClientError> {
        let counter = client.add_counter(
            id.0,
            &member_id.to_le_bytes(),
            &format!("{name} {member_id}"),
        )?;
        counter.store_release(value)?;
        Ok(Self {
            counter,
            last: value,
        })
    }

    /// Stores `value` when it is not the value last stored: a release store, which a
    /// reader of a position loads with acquire. Whether the counter holds `value`
    /// now: not when the client is closed, which stores nothing.
    fn store(&mut self, value: i64) -> bool {
        if value != self.last && self.counter.store_release(value).is_ok() {
            self.last = value;
        }
        self.last == value
    }
}

/// A count as a counter's value.
fn count_value(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// The member's role as `cluster.role` holds it: 0 follower, 1 candidate, 2 leader.
fn role_code(core: &Consensus) -> i64 {
    i64::from(core.role() as u8)
}

/// The member's counters in the counters file (`spec/cluster.md`, "Counters").
struct Counters {
    role: Stored,
    election_state: Stored,
    leadership_term_id: Stored,
    append_position: Stored,
    /// `cluster.log_recording_id`, added before `cluster.commit_position` and stored
    /// before it (`spec/cluster.md`, "The service").
    log_recording_id: Stored,
    commit_position: Stored,
    /// The core's counts, one counter each, in the order of [`Counts::by_counter`]:
    /// [`Counts::LEN`] of them, added once when the module starts.
    counts: Vec<Stored>,
    messages_rejected: Stored,
}

impl Counters {
    /// Adds the member's counters, each holding what `core` holds from its first
    /// store, which follows its add at once. `cluster.log_recording_id` is added and
    /// holds −1 before `cluster.commit_position` is added: a service reads the
    /// recording id only of a member whose commit position it has found
    /// (`spec/cluster.md`, "The service").
    fn add(client: &mut Cavalry, member_id: i32, core: &Consensus) -> Result<Self, ClientError> {
        let mut add = |id, name, value| Stored::add(client, id, name, member_id, value);
        Ok(Self {
            role: add(CLUSTER_ROLE, "cluster.role", role_code(core))?,
            election_state: add(
                CLUSTER_ELECTION_STATE,
                "cluster.election_state",
                i64::from(core.election_state().code()),
            )?,
            leadership_term_id: add(
                CLUSTER_LEADERSHIP_TERM_ID,
                "cluster.leadership_term_id",
                core.leadership_term_id(),
            )?,
            append_position: add(
                CLUSTER_APPEND_POSITION,
                "cluster.append_position",
                core.append_position(),
            )?,
            log_recording_id: add(
                CLUSTER_LOG_RECORDING_ID,
                "cluster.log_recording_id",
                NO_RECORDING,
            )?,
            commit_position: add(
                CLUSTER_COMMIT_POSITION,
                "cluster.commit_position",
                core.commit_position(),
            )?,
            counts: {
                // Each count's counter, by the id the core pairs it with and under the
                // name `counters.toml` gives that id (ADR 0067). Every id of
                // `by_counter` is an entry of that table, which its doctest checks,
                // so the other name is never a label.
                let mut counts = Vec::with_capacity(Counts::LEN);
                for (id, value) in core.counts().by_counter() {
                    let name = COUNTERS
                        .iter()
                        .find(|entry| entry.id == id)
                        .map_or("cluster.count", |entry| entry.name);
                    counts.push(add(id, name, count_value(value))?);
                }
                counts
            },
            messages_rejected: add(CLUSTER_MESSAGES_REJECTED, "cluster.messages_rejected", 0)?,
        })
    }

    /// Stores what the core holds, each value only when it has moved, and
    /// `log_recording_id`, the recording the member records the live log into or −1.
    /// The recording id is stored before the commit position, and a commit position
    /// only once the counter holds that id, so that no commit position is stored
    /// beside a recording id that does not hold it (`spec/cluster.md`, "The service").
    fn store(&mut self, core: &Consensus, messages_rejected: u64, log_recording_id: i64) {
        let counts: Counts = core.counts();
        self.role.store(role_code(core));
        self.election_state
            .store(i64::from(core.election_state().code()));
        self.leadership_term_id.store(core.leadership_term_id());
        self.append_position.store(core.append_position());
        if self.log_recording_id.store(log_recording_id) {
            self.commit_position.store(core.commit_position());
        }
        for (stored, (_, value)) in self.counts.iter_mut().zip(counts.by_counter()) {
            stored.store(count_value(value));
        }
        self.messages_rejected.store(count_value(messages_rejected));
    }
}

/// One cluster member's consensus module, a client of the driver in its directory.
pub struct ConsensusModule {
    core: Consensus,
    consensus: Subscription,
    io: ClientIo,
    counters: Counters,
    messages_rejected: u64,
    started: Instant,
    /// The clock of the last duty cycle, which an offer between two cycles is stamped
    /// with: the clock is read once a cycle and never per entry.
    clock: Clock,
}

impl ConsensusModule {
    /// Starts the member with id `member_id` of the cluster `members` names, the
    /// members string of `spec/cluster.md`, "Members and streams", as a client of the
    /// driver in `dir`, with its record of terms in `log_dir`, the driver's log
    /// directory. It subscribes to the consensus stream on its `consensus` endpoint,
    /// adds a publication to every other member's, and adds its counters; the log comes
    /// with its election.
    ///
    /// A log directory that holds a term or a vote is [`RaftError::Restart`]: a member
    /// of this version starts on a new log directory, with every other member of its
    /// cluster.
    pub fn start(
        dir: impl AsRef<Path>,
        log_dir: impl AsRef<Path>,
        members: &str,
        member_id: i32,
    ) -> Result<Self, RaftError> {
        let mut parsed = [EMPTY_MEMBER; MAX_MEMBERS];
        let count = parse_members(members.as_bytes(), &mut parsed, Some(member_id))
            .map_err(RaftError::Members)?;
        let parsed = parsed.get(..count).unwrap_or(&[]);
        let mut ids = [0i32; MAX_MEMBERS];
        for (slot, member) in ids.iter_mut().zip(parsed) {
            *slot = member.id;
        }
        let core = Consensus::new(ids.get(..count).unwrap_or(&[]), member_id)
            .map_err(RaftError::Member)?;
        let Some(own) = parsed.iter().find(|member| member.id == member_id) else {
            return Err(RaftError::Member(MembersRefused::NotAMember {
                id: member_id,
            }));
        };

        let terms = TermsFile::open(log_dir.as_ref())?;
        if terms.log_leadership_term_id() != -1 || terms.candidate_term() != -1 {
            return Err(RaftError::Restart {
                log_leadership_term_id: terms.log_leadership_term_id(),
                candidate_term: terms.candidate_term(),
            });
        }

        let mut client = Cavalry::connect_to(dir)?;
        let consensus =
            client.add_subscription(&endpoint_channel(&own.consensus), CONSENSUS_STREAM_ID)?;
        let mut peers = Vec::with_capacity(MAX_MEMBERS);
        for member in parsed.iter().filter(|member| member.id != member_id) {
            peers.push(Peer {
                id: member.id,
                consensus: client.add_exclusive_publication(
                    &endpoint_channel(&member.consensus),
                    CONSENSUS_STREAM_ID,
                )?,
                log_destination: endpoint_channel(&member.log),
            });
        }
        // Each counter holds the core's value from its add: −1 for the term and the
        // recording, before a duty cycle has run.
        let counters = Counters::add(&mut client, member_id, &core)?;
        let started = Instant::now();
        Ok(Self {
            core,
            consensus,
            io: ClientIo {
                client,
                terms,
                peers,
                log_control_channel: format!(
                    "cavalry:udp?control={}|control-mode=manual|fc=min",
                    host_port(&own.log)
                ),
                log_endpoint_channel: endpoint_channel(&own.log),
                scratch: Box::new([0u8; 1408]),
                log_publication: None,
                log_term: None,
                destinations_added: 0,
                log_recording: None,
                log_subscription: None,
                armed: None,
                image: LogImage::Awaited,
                held: None,
                stopped_at: 0,
                sync_asked_at: NO_SYNC_ASKED,
            },
            counters,
            messages_rejected: 0,
            started,
            clock: read_clock(started),
        })
    }

    /// One duty cycle: the consensus messages that have arrived, at most 16, what a
    /// follower's log subscription holds, the core's own cycle, and the counters that
    /// moved. Returns how much it did, 0 for nothing, for the caller's idle strategy.
    /// An error is a step the member cannot go on without; the member stays in the
    /// state it was in, and a later call tries the step again.
    pub fn do_work(&mut self) -> Result<usize, RaftError> {
        self.clock = read_clock(self.started);
        let clock = self.clock;
        let Self {
            core,
            consensus,
            io,
            messages_rejected,
            ..
        } = self;
        let mut fault = None;
        let read = consensus
            .poll(
                |body, header| {
                    // A consensus message is one frame; a fragment of something longer
                    // is none of this page's, and is counted as a message that failed
                    // its length.
                    let whole = header.flags().contains(Flags::UNFRAGMENTED);
                    match Message::parse(Stream::Consensus, body) {
                        Ok(message) if whole => {
                            if fault.is_none()
                                && let Err(error) = core.on_message(clock, &message, io)
                            {
                                fault = Some(error);
                            }
                        }
                        _ => *messages_rejected += 1,
                    }
                },
                CONSENSUS_POLL_LIMIT,
            )
            .map_err(RaftError::Poll)?;
        if let Some(error) = fault {
            return Err(error);
        }
        io.observe_log()?;
        let work = core.do_work(clock, io)?;
        let log_recording_id = self.log_recording_id().unwrap_or(NO_RECORDING);
        self.counters
            .store(&self.core, self.messages_rejected, log_recording_id);
        Ok(read + work)
    }

    /// Appends `payload` to the cluster's log as a session message of no session, for
    /// the leader's own process (`spec/cluster.md`, "The appointed leader"). It is
    /// committed once a quorum has recorded it, which
    /// [`ConsensusModule::commit_position`] says. The entry carries the cluster's time
    /// of the last duty cycle. A follower, a leader whose election
    /// has not closed and a back-pressured log are [`RaftError::Offer`], each with what
    /// to do; a payload over [`MAX_PAYLOAD_LENGTH`] is [`RaftError::PayloadTooLong`].
    pub fn offer(&mut self, payload: &[u8]) -> Result<(), RaftError> {
        if payload.len() > MAX_PAYLOAD_LENGTH {
            return Err(RaftError::PayloadTooLong {
                length: payload.len(),
            });
        }
        self.core
            .offer(self.clock, payload, &mut self.io)?
            .map_err(RaftError::Offer)
    }

    /// This member's id.
    pub fn member_id(&self) -> i32 {
        self.core.member_id()
    }

    /// This member's role.
    pub fn role(&self) -> Role {
        self.core.role()
    }

    /// The state of this member's election, closed when none runs.
    pub fn election_state(&self) -> ElectionState {
        self.core.election_state()
    }

    /// The term this member leads or follows, −1 before the first.
    pub fn leadership_term_id(&self) -> i64 {
        self.core.leadership_term_id()
    }

    /// This member's append position: what its recording of the log has synced.
    pub fn append_position(&self) -> i64 {
        self.core.append_position()
    }

    /// The commit position this member knows, which never falls.
    pub fn commit_position(&self) -> i64 {
        self.core.commit_position()
    }

    /// What this member has counted, as its counters in the counters file say it.
    pub fn counts(&self) -> Counts {
        self.core.counts()
    }

    /// The id of this member's recording of the log, once it has one: the leader's of
    /// its publication, a follower's of the leader's image; `None` before the first and
    /// from when the member has left the log, its recording's stop read, until it has
    /// joined it again. A leave that failed holds the recording, named, while the next
    /// cycle tries it again. Each duty
    /// cycle stores it in `cluster.log_recording_id`, −1 for `None`, before the commit
    /// position (`spec/cluster.md`, "The service").
    pub fn log_recording_id(&self) -> Option<i64> {
        self.io
            .log_recording
            .as_ref()
            .or(self.io.held.as_ref().map(|held| &held.recording))
            .map(Recording::recording_id)
    }

    /// Stops the module: closes its client, which removes its publications, its
    /// subscriptions and its counters. Its recordings are the driver's and stay; the
    /// driver stops each when its publication or its image ends.
    pub fn close(self) -> Result<(), RaftError> {
        let Self {
            consensus,
            io,
            counters,
            ..
        } = self;
        drop(counters);
        drop(consensus);
        let ClientIo {
            client,
            peers,
            log_publication,
            log_subscription,
            ..
        } = io;
        drop(peers);
        drop(log_publication);
        drop(log_subscription);
        Ok(client.close()?)
    }
}

impl core::fmt::Debug for ConsensusModule {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ConsensusModule")
            .field("member_id", &self.core.member_id())
            .field("role", &self.core.role())
            .field("election_state", &self.core.election_state())
            .field("leadership_term_id", &self.core.leadership_term_id())
            .field("append_position", &self.core.append_position())
            .field("commit_position", &self.core.commit_position())
            .finish()
    }
}

#[cfg(test)]
mod tests;
