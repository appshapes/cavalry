//! The receiver: the driver's fifth thread, which rebuilds every `cavalry:udp` stream a
//! subscription wants from the wire into an image, a log buffer of type 2 its subscribers
//! read as any log, by the rules of `spec/udp.md`, "The receiver" (ADR 0039, ADR 0040,
//! ADR 0042). The conductor binds each receive endpoint's socket on the control plane
//! and hands it over through a bounded queue with the streams its subscriptions want;
//! the receiver hands a setup frame back through a second queue, the conductor creates
//! the image and hands it here with its writer, its two counters and its readers; and
//! the receiver answers with the states only it can see, an image retired, a reader
//! removed or rejected, an endpoint closed. On each duty cycle the receiver reads its
//! clock once, handles a bounded number of requests, polls every endpoint's socket for
//! up to a poll's worth of datagrams and acts on each by its session's record, loads
//! every reader's position once, and visits every image: the rebuild scan and the NAK
//! for the gap it finds, the status message on its triggers, the liveness timeout and
//! the end of stream; then, every 100 ms, the sweep of expired session records. Nothing
//! here allocates per cycle, locks, logs or reads the clock per message; the syscalls
//! are the endpoint's own polls, one per poll on Linux, and the control frames it
//! sends. The receiver is built on its own thread, as the sender is, since the headers
//! its batched receive fills ([`ReceiveBuffers`]) hold raw pointers and cannot be sent
//! to it.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cavalry_proto::frame::{DataFrameHeader, Flags, FrameType};
use cavalry_proto::udp::{
    ControlFrame, Datagram, IMAGE_LIVENESS_TIMEOUT, MAX_TERM_LENGTH, NAK_DELAY, NAK_RETRY, Nak,
    RttMeasurement, STATUS_MESSAGE_END_OF_STREAM, STATUS_MESSAGE_INTERVAL,
    STATUS_MESSAGE_SEND_SETUP, SetupFrame, StatusMessage, parse, receiver_bounds,
    status_message_threshold,
};
use cavalry_shm::counters::generated::{
    IMAGE_CREATED, IMAGE_ENDED, IMAGE_TIMED_OUT, IMAGE_UNSUBSCRIBED, LOG_BUFFER_REJECTED_POSITION,
    RECEIVER_BYTES_RECEIVED, RECEIVER_DROPPED_COOL_DOWN, RECEIVER_DROPPED_FRAMES,
    RECEIVER_DROPPED_IN_PROGRESS, RECEIVER_DROPPED_PENDING, RECEIVER_DROPPED_UNWANTED,
    RECEIVER_FRAMES_RECEIVED, RECEIVER_GAPS_DETECTED, RECEIVER_HEARTBEATS_RECEIVED,
    RECEIVER_NAKS_SENT, RECEIVER_OVER_RUNS, RECEIVER_RECEIVE_FAILED, RECEIVER_REJECTED_DATAGRAMS,
    RECEIVER_RTT_REPLIES_SENT, RECEIVER_SESSIONS_FULL, RECEIVER_SETUPS_ELICITED,
    RECEIVER_SETUPS_RECEIVED, RECEIVER_SETUPS_UNWANTED, RECEIVER_STATUS_MESSAGES_SENT,
    RECEIVER_UNDER_RUNS, SENDER_SEND_FAILED,
};
use cavalry_shm::counters::{Counter, CounterId, CountersError};
use cavalry_shm::log::{ImageScan, ImageWriter, LogBuffers, Positions};
use cavalry_shm::spsc::{Consumer, Producer};
use nix::sys::socket::SockaddrStorage;

use crate::conductor::now_nanos;
use crate::idle::{IdleStrategy, Idler};
use crate::socket::{ReceiveBuffers, ReceiveSocket};
use crate::spec;

/// How long the receiver parks while it holds no endpoint: the conductor unparks it with
/// every request, so a driver that receives nothing wakes no CPU for the receiver, as it
/// wakes none for a sender with nothing to send (ADR 0041).
const IDLE_PARK: Duration = Duration::from_secs(1);

/// A frame's alignment as the positions count.
const ALIGN: i64 = cavalry_proto::frame::FRAME_ALIGNMENT as i64;

/// How many replies the receiver holds back when the reply queue is full: a setup per
/// session record, a retire per image, a removed and a rejected reader per reader, a
/// close per endpoint, and one per request handled in a cycle. With the pending replies
/// at this bound the receiver handles no request and marks no session in progress until
/// the conductor takes replies again; it keeps receiving (ADR 0042).
pub(crate) const PENDING_REPLIES: usize = spec::MAX_SESSION_RECORDS
    + spec::MAX_IMAGES
    + 2 * (spec::MAX_IMAGES + spec::MAX_RECORDINGS)
    + spec::MAX_RECEIVE_ENDPOINTS
    + spec::RECEIVER_REQUESTS_PER_CYCLE;

/// How many readers the receiver holds: a `subscription.position` per subscription and
/// image that match, bounded by the images registry, and a `recording.position` per
/// recording of an image, bounded by the recordings registry.
const MAX_READERS: usize = spec::MAX_IMAGES + spec::MAX_RECORDINGS;

/// The slots of the session table: twice the records, so that a probe is short.
const SESSION_SLOTS: usize = 2 * spec::MAX_SESSION_RECORDS;

/// An empty slot of the session table.
const EMPTY: u32 = u32::MAX;

/// Where an image is in its life (`spec/udp.md`, "The receiver", step 5); every
/// transition adds one to a counter, and `counters.toml` lists them. The receiver moves
/// an image from pending to live when the conductor hands it over, and to ended, timed
/// out or unsubscribed when it retires it; the conductor moves it to removed when it
/// has unlinked the log.
// state machine: image
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImageState {
    /// The setup frame is with the conductor, which creates the image.
    Pending,
    /// The image receives: its log fills, its subscribers read.
    Live,
    /// The stream ended with EOS and every reader reached its end, or the liveness
    /// timeout passed after it.
    Ended,
    /// No accepted datagram or heartbeat for the image liveness timeout.
    TimedOut,
    /// Its last subscription on the endpoint and stream was removed.
    Unsubscribed,
    /// The conductor unlinked its log after `unavailable image` went out.
    Removed,
}

/// A reader of an image: a subscription, through the `subscription.position` the
/// client owns, or a recording, through its `recording.position` once the recorder has
/// attached the counter and its start position until then (ADR 0042).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReaderKey {
    /// A subscription's registration id.
    Subscription(i64),
    /// A recording's publication id in the conductor's registry: the image's
    /// registration id, since a recording is keyed by what it records.
    Recording(i64),
}

/// A stream a subscription on an endpoint wants, and how many want it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Interest {
    /// The stream id.
    pub stream_id: i32,
    /// How many subscriptions on the endpoint want it.
    pub count: u32,
}

/// What the conductor created for a setup frame, carried to the receiver once: the
/// image's log and writer, its two counters, and the readers it starts with.
pub(crate) struct AddImage {
    /// The image's registration id, which names its log and its counters.
    pub registration_id: i64,
    /// The endpoint it arrived on.
    pub endpoint_id: i64,
    /// The stream's session id.
    pub session_id: i32,
    /// The stream id.
    pub stream_id: i32,
    /// The source address of the setup frame: where every control frame goes.
    pub source: SocketAddr,
    /// The image's log, created at the setup's position.
    pub log: LogBuffers,
    /// The receiver's writer on it.
    pub writer: ImageWriter,
    /// Its `image.hwm_position` counter.
    pub hwm: Counter,
    /// Its `image.rebuild_position` counter.
    pub rebuild: Counter,
    /// The position the setup frame named: where the image starts.
    pub start_position: i64,
    /// The readers it starts with: the subscriptions on the endpoint and stream, each
    /// with its position counter at the start position.
    pub readers: Vec<(ReaderKey, Option<Counter>, i64)>,
}

/// What the conductor asks the receiver.
pub(crate) enum Request {
    /// A receive endpoint the conductor bound: its socket moves to the receiver, with
    /// the list its interests live in, preallocated by the conductor.
    AddEndpoint {
        /// The endpoint's registration id: the correlation id of the command that
        /// bound it, which keys its `channel.status` counter.
        registration_id: i64,
        /// The socket, bound and non-blocking.
        socket: ReceiveSocket,
        /// The streams the endpoint's subscriptions want, empty, with room for every
        /// subscription.
        wanted: Vec<Interest>,
    },
    /// Close an endpoint left with no subscription and no image: the socket is dropped
    /// and the session records keyed by it freed; answered with
    /// [`Reply::EndpointClosed`].
    RemoveEndpoint {
        /// The endpoint.
        registration_id: i64,
    },
    /// A subscription on the endpoint wants a stream.
    AddInterest {
        /// The endpoint.
        endpoint_id: i64,
        /// The stream.
        stream_id: i32,
    },
    /// A subscription on the endpoint was removed: its readers go, answered with
    /// [`Reply::ReaderRemoved`] each, and an image left with no reader is retired.
    RemoveInterest {
        /// The endpoint.
        endpoint_id: i64,
        /// The stream.
        stream_id: i32,
        /// The subscription.
        subscription_id: i64,
    },
    /// The image the conductor created for a setup the receiver handed it.
    AddImage(AddImage),
    /// The conductor could not create the image: the session's record is freed, so
    /// the sender's next setup tries again.
    RefuseImage {
        /// The endpoint.
        endpoint_id: i64,
        /// The session.
        session_id: i32,
        /// The stream.
        stream_id: i32,
    },
    /// A reader joins an image: a subscription with its counter, or a recording with
    /// its start position as a floor until the counter arrives.
    AddReader {
        /// The image.
        registration_id: i64,
        /// The reader.
        key: ReaderKey,
        /// Its position counter, when it exists.
        counter: Option<Counter>,
        /// Its position until the counter exists.
        floor: i64,
    },
    /// A recording's counter, once the recorder has given the recording id.
    AttachReader {
        /// The image.
        registration_id: i64,
        /// The reader.
        key: ReaderKey,
        /// Its `recording.position` counter.
        counter: Counter,
    },
    /// A reader leaves an image; answered with [`Reply::ReaderRemoved`].
    RemoveReader {
        /// The image.
        registration_id: i64,
        /// The reader.
        key: ReaderKey,
    },
}

/// What the receiver answers: the states only it can see.
pub(crate) enum Reply {
    /// A setup frame for a stream a subscription wants and a session with no image: the
    /// session is in progress until the conductor answers with an image or a refusal.
    Setup {
        /// The endpoint it arrived on.
        endpoint_id: i64,
        /// Where it came from: the image's source identity, and where every control
        /// frame for the image goes.
        source: SocketAddr,
        /// The frame.
        setup: SetupFrame,
    },
    /// The image was retired, in the state named; the receiver dropped its log, its
    /// writer, its counters and its readers' counters.
    Retired {
        /// The image.
        registration_id: i64,
        /// Ended, timed out or unsubscribed.
        state: ImageState,
    },
    /// A reader left the image, on [`Request::RemoveInterest`] or
    /// [`Request::RemoveReader`]; the receiver dropped its counter, so the conductor
    /// may free the record.
    ReaderRemoved {
        /// The image.
        registration_id: i64,
        /// The reader.
        key: ReaderKey,
    },
    /// A reader's position was outside its bound; it is not used again, and the
    /// conductor evicts its client (ADR 0010). One per rejected reader.
    ReaderRejected {
        /// The image.
        registration_id: i64,
        /// The reader.
        key: ReaderKey,
    },
    /// The endpoint's socket is closed.
    EndpointClosed {
        /// The endpoint.
        registration_id: i64,
    },
}

/// The static counters the receiver moves (`spec/udp.md`, "Counters").
pub(crate) struct Statics {
    bytes_received: Counter,
    frames_received: Counter,
    setups_received: Counter,
    setups_unwanted: Counter,
    setups_elicited: Counter,
    heartbeats_received: Counter,
    status_messages_sent: Counter,
    naks_sent: Counter,
    gaps_detected: Counter,
    under_runs: Counter,
    over_runs: Counter,
    rejected_datagrams: Counter,
    receive_failed: Counter,
    dropped_frames: Counter,
    dropped_pending: Counter,
    dropped_in_progress: Counter,
    dropped_cool_down: Counter,
    dropped_unwanted: Counter,
    sessions_full: Counter,
    rtt_replies_sent: Counter,
    image_created: Counter,
    image_ended: Counter,
    image_timed_out: Counter,
    image_unsubscribed: Counter,
    rejected_position: Counter,
    send_failed: Counter,
}

impl Statics {
    /// The receiver's counters, each taken from the counters file through `counter`.
    pub(crate) fn new(
        counter: impl Fn(CounterId) -> Result<Counter, CountersError>,
    ) -> Result<Self, CountersError> {
        Ok(Self {
            bytes_received: counter(RECEIVER_BYTES_RECEIVED)?,
            frames_received: counter(RECEIVER_FRAMES_RECEIVED)?,
            setups_received: counter(RECEIVER_SETUPS_RECEIVED)?,
            setups_unwanted: counter(RECEIVER_SETUPS_UNWANTED)?,
            setups_elicited: counter(RECEIVER_SETUPS_ELICITED)?,
            heartbeats_received: counter(RECEIVER_HEARTBEATS_RECEIVED)?,
            status_messages_sent: counter(RECEIVER_STATUS_MESSAGES_SENT)?,
            naks_sent: counter(RECEIVER_NAKS_SENT)?,
            gaps_detected: counter(RECEIVER_GAPS_DETECTED)?,
            under_runs: counter(RECEIVER_UNDER_RUNS)?,
            over_runs: counter(RECEIVER_OVER_RUNS)?,
            rejected_datagrams: counter(RECEIVER_REJECTED_DATAGRAMS)?,
            receive_failed: counter(RECEIVER_RECEIVE_FAILED)?,
            dropped_frames: counter(RECEIVER_DROPPED_FRAMES)?,
            dropped_pending: counter(RECEIVER_DROPPED_PENDING)?,
            dropped_in_progress: counter(RECEIVER_DROPPED_IN_PROGRESS)?,
            dropped_cool_down: counter(RECEIVER_DROPPED_COOL_DOWN)?,
            dropped_unwanted: counter(RECEIVER_DROPPED_UNWANTED)?,
            sessions_full: counter(RECEIVER_SESSIONS_FULL)?,
            rtt_replies_sent: counter(RECEIVER_RTT_REPLIES_SENT)?,
            image_created: counter(IMAGE_CREATED)?,
            image_ended: counter(IMAGE_ENDED)?,
            image_timed_out: counter(IMAGE_TIMED_OUT)?,
            image_unsubscribed: counter(IMAGE_UNSUBSCRIBED)?,
            rejected_position: counter(LOG_BUFFER_REJECTED_POSITION)?,
            send_failed: counter(SENDER_SEND_FAILED)?,
        })
    }

    /// Counts what the socket did with one control frame: a send the operating system
    /// refused, or took only part of, counts in `sender.send_failed`, the one counter
    /// for a refused send on either kind of endpoint (`spec/udp.md`, "The receiver").
    fn count_sent(&self, sent: std::io::Result<bool>, whole: &Counter) {
        match sent {
            Ok(true) => whole.increment(),
            Ok(false) | Err(_) => self.send_failed.increment(),
        }
    }
}

/// The key of a session record: the endpoint, the stream and the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SessionKey {
    endpoint_id: i64,
    stream_id: i32,
    session_id: i32,
}

impl SessionKey {
    /// A slot of the table: a 64-bit mix of the three ids, masked.
    fn hash(self) -> usize {
        let mut x = (self.endpoint_id as u64)
            ^ ((self.stream_id as u32 as u64) << 32)
            ^ (self.session_id as u32 as u64).rotate_left(17);
        // splitmix64's finalizer: every bit of the key reaches every bit of the slot.
        x ^= x >> 30;
        x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x ^= x >> 27;
        x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^= x >> 31;
        (x as usize) & (SESSION_SLOTS - 1)
    }
}

/// Where a session record is: the four states of `spec/udp.md`, "The receiver".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Session {
    /// A status message asked the sender for a setup frame; expires at `until`.
    Pending {
        /// When the record expires, by the receiver's clock.
        until: i64,
    },
    /// The setup frame is with the conductor.
    InProgress,
    /// The image at this index of the live list.
    Image {
        /// The index into [`Receiver::live`].
        index: usize,
    },
    /// The image was retired; the record expires at `until`.
    CoolDown {
        /// When the record expires, by the receiver's clock.
        until: i64,
    },
}

/// One session record.
#[derive(Clone, Copy, Debug)]
struct Record {
    key: SessionKey,
    session: Session,
}

/// The session table: [`spec::MAX_SESSION_RECORDS`] records found through an
/// open-addressing table of [`SESSION_SLOTS`] slots with linear probing and
/// backward-shift deletion, so that a datagram's session is found in a bounded number
/// of probes with no allocation after the table is built (ADR 0042).
struct SessionTable {
    slots: Box<[u32]>,
    records: Vec<Record>,
}

impl SessionTable {
    fn new() -> Self {
        Self {
            slots: vec![EMPTY; SESSION_SLOTS].into_boxed_slice(),
            records: Vec::with_capacity(spec::MAX_SESSION_RECORDS),
        }
    }

    /// The slot that holds `key`, when one does.
    fn slot_of(&self, key: SessionKey) -> Option<usize> {
        let mut slot = key.hash();
        for _ in 0..SESSION_SLOTS {
            let entry = self.slots[slot];
            if entry == EMPTY {
                return None;
            }
            if self
                .records
                .get(entry as usize)
                .is_some_and(|record| record.key == key)
            {
                return Some(slot);
            }
            slot = (slot + 1) & (SESSION_SLOTS - 1);
        }
        None
    }

    /// The record of `key`, when one exists.
    fn get(&self, key: SessionKey) -> Option<usize> {
        self.slot_of(key).map(|slot| self.slots[slot] as usize)
    }

    /// Records `key` in `session`; `None` when the table is full.
    fn insert(&mut self, key: SessionKey, session: Session) -> Option<usize> {
        if self.records.len() >= spec::MAX_SESSION_RECORDS {
            return None;
        }
        let mut slot = key.hash();
        for _ in 0..SESSION_SLOTS {
            if self.slots[slot] == EMPTY {
                let index = self.records.len();
                self.records.push(Record { key, session });
                self.slots[slot] = index as u32;
                return Some(index);
            }
            slot = (slot + 1) & (SESSION_SLOTS - 1);
        }
        None
    }

    /// Frees the record of `key`, when one exists.
    fn remove(&mut self, key: SessionKey) {
        let Some(slot) = self.slot_of(key) else {
            return;
        };
        let index = self.slots[slot] as usize;
        // The last record moves into the hole; its slot follows it. Its slot is found
        // before the swap, while the slot's entry still names a record with its key.
        let last = self.records.len() - 1;
        if index != last {
            let moved = self.records[last].key;
            let moved_slot = self.slot_of(moved);
            self.records.swap(index, last);
            if let Some(moved_slot) = moved_slot {
                self.slots[moved_slot] = index as u32;
            }
        }
        self.records.pop();
        // Backward-shift deletion: every entry after the hole that could not have been
        // probed past it moves back, so that no probe finds an empty slot before its key.
        let mask = SESSION_SLOTS - 1;
        let mut hole = slot;
        self.slots[hole] = EMPTY;
        let mut next = hole;
        loop {
            next = (next + 1) & mask;
            let entry = self.slots[next];
            if entry == EMPTY {
                break;
            }
            let Some(record) = self.records.get(entry as usize) else {
                break;
            };
            let home = record.key.hash();
            // The entry stays where it is when its home lies in the cyclic range
            // (hole, next]; otherwise it moves into the hole.
            let stays = if hole <= next {
                hole < home && home <= next
            } else {
                home > hole || home <= next
            };
            if !stays {
                self.slots[hole] = entry;
                self.slots[next] = EMPTY;
                hole = next;
            }
        }
    }
}

/// One receive endpoint: its socket and the streams its subscriptions want.
struct Endpoint {
    registration_id: i64,
    socket: ReceiveSocket,
    wanted: Vec<Interest>,
}

impl Endpoint {
    fn wants(&self, stream_id: i32) -> bool {
        self.wanted
            .iter()
            .any(|interest| interest.stream_id == stream_id && interest.count > 0)
    }
}

/// A gap the rebuild scan found: the NAK's range and its clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Gap {
    start: i64,
    length: i64,
    /// When the gap was first seen with this start and length: the NAK delay runs from
    /// here.
    seen_at: i64,
    /// When the last NAK for it went: the retry runs from here.
    last_nak_at: Option<i64>,
}

/// One image the receiver rebuilds.
struct Live {
    registration_id: i64,
    key: SessionKey,
    source: SockaddrStorage,
    positions: Positions,
    term_length: i64,
    receiver_window_length: i32,
    threshold: i64,
    log: LogBuffers,
    writer: ImageWriter,
    hwm: Counter,
    rebuild: Counter,
    start_position: i64,
    hwm_position: i64,
    rebuild_position: i64,
    /// The position of the last status message sent: s.
    last_sm_position: i64,
    last_sm_at: i64,
    clean_position: i64,
    gap: Option<Gap>,
    /// Where the next cycle's scan goes on from: set when a scan ended at its bound
    /// inside the gap at the rebuild position, so that the gap's end is found a bound of
    /// term per cycle, not the same words again ([`Receiver::rebuild`]).
    gap_cursor: Option<i64>,
    last_activity_at: i64,
    /// The end of stream position a heartbeat with EOS named.
    end_of_stream: Option<i64>,
    /// The end of stream position is stored in the log: the rebuild reached it.
    ended: bool,
    /// Status messages with the end-of-stream flag sent since the stream ended.
    end_of_stream_sms: u8,
    state: ImageState,
    /// The readers' positions this cycle, and how many were read.
    slowest: i64,
    fastest: i64,
    reader_count: usize,
    /// How many of the readers are subscriptions, rejected or not: an image with none
    /// is retired as unsubscribed, checked every cycle (`serve`).
    subscriber_count: usize,
}

/// One reader of an image.
struct Reader {
    image_index: usize,
    key: ReaderKey,
    counter: Option<Counter>,
    floor: i64,
    /// Its position failed its bound: not read again until it is removed.
    rejected: bool,
    /// It was removed while the pending replies were at their bound: read no more,
    /// counted no more, and dropped with its reply once there is room (`flush_leaving`).
    leaving: bool,
}

impl Reader {
    fn position(&self) -> i64 {
        match &self.counter {
            Some(counter) => counter.load_acquire(),
            None => self.floor,
        }
    }
}

/// The receiver's state: the queues, the endpoints, the images, the readers, the
/// session table, the counters and the buffers every endpoint's poll fills. Not
/// `Send`, because of the buffers' headers on Linux: [`Receiver::new`] runs on the
/// receiver's own thread, and the tests that drive a receiver by hand build it on
/// theirs.
pub(crate) struct Receiver {
    requests: Consumer<Request>,
    replies: Producer<Reply>,
    /// Replies the reply queue had no room for, offered again next cycle.
    pending: VecDeque<Reply>,
    endpoints: Vec<Endpoint>,
    live: Vec<Live>,
    readers: Vec<Reader>,
    sessions: SessionTable,
    statics: Statics,
    buffers: ReceiveBuffers,
    /// This driver's receiver id, in every status message.
    receiver_id: i64,
    last_sweep_at: i64,
    idler: Idler,
}

impl Receiver {
    /// A receiver over the queues to and from the conductor, with its counters; the
    /// buffers for a poll are preallocated here, on the thread that calls this, which
    /// must be the one that runs the receiver.
    pub(crate) fn new(
        requests: Consumer<Request>,
        replies: Producer<Reply>,
        statics: Statics,
        idle: IdleStrategy,
    ) -> Self {
        let now = now_nanos();
        Self {
            requests,
            replies,
            pending: VecDeque::with_capacity(PENDING_REPLIES),
            endpoints: Vec::with_capacity(spec::MAX_RECEIVE_ENDPOINTS),
            live: Vec::with_capacity(spec::MAX_IMAGES),
            readers: Vec::with_capacity(MAX_READERS),
            sessions: SessionTable::new(),
            statics,
            buffers: ReceiveBuffers::preallocate(),
            receiver_id: receiver_id(now),
            last_sweep_at: now,
            idler: Idler::new(idle),
        }
    }

    /// This driver's receiver id, for the tests.
    #[cfg(test)]
    pub(crate) fn receiver_id(&self) -> i64 {
        self.receiver_id
    }

    /// The high-water mark, the rebuild position and the last status message's
    /// position of an image, for the tests.
    #[cfg(test)]
    pub(crate) fn positions_of(&self, registration_id: i64) -> Option<(i64, i64, i64)> {
        self.live
            .iter()
            .find(|live| live.registration_id == registration_id)
            .map(|live| {
                (
                    live.hwm_position,
                    live.rebuild_position,
                    live.last_sm_position,
                )
            })
    }

    /// The gap of an image as the receiver knows it, its start and its length, for the
    /// tests.
    #[cfg(test)]
    pub(crate) fn gap_of(&self, registration_id: i64) -> Option<(i64, i64)> {
        self.live
            .iter()
            .find(|live| live.registration_id == registration_id)
            .and_then(|live| live.gap.map(|gap| (gap.start, gap.length)))
    }

    /// How many replies the receiver holds back for the reply queue, for the tests.
    #[cfg(test)]
    pub(crate) fn pending_replies(&self) -> usize {
        self.pending.len()
    }

    /// How many session records the table holds, for the tests.
    #[cfg(test)]
    pub(crate) fn session_records(&self) -> usize {
        self.sessions.records.len()
    }

    /// The receiver's loop: duty cycles until `stop`, idling with the strategy while it
    /// holds an endpoint and parked while it holds none. The sockets close with the
    /// receiver; a sender sees its status messages stop and disconnects.
    pub(crate) fn run(mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Acquire) {
            let now = now_nanos();
            if self.duty_cycle(now) != 0 {
                self.idler.reset();
            } else if self.endpoints.is_empty() && self.live.is_empty() && self.pending.is_empty() {
                // Nothing can change until the conductor offers a request, and it
                // unparks this thread when it does (`sender::Sender::run` says why no
                // request waits for the timeout).
                std::thread::park_timeout(IDLE_PARK);
                self.idler.reset();
            } else {
                self.idler.idle();
            }
        }
        self.flush_replies();
    }

    /// One duty cycle; returns how much work it found, 0 when the receiver may idle. The
    /// clock is read once by the caller and passed down.
    pub(crate) fn duty_cycle(&mut self, now: i64) -> usize {
        let mut work = self.flush_replies();
        for _ in 0..spec::RECEIVER_REQUESTS_PER_CYCLE {
            if self.pending.len() >= PENDING_REPLIES {
                break;
            }
            let Some(request) = self.requests.poll() else {
                break;
            };
            self.handle(request, now);
            work += 1;
        }
        for index in 0..self.endpoints.len() {
            work += self.poll_endpoint(index, now);
        }
        self.load_readers();
        let mut index = 0;
        while index < self.live.len() {
            let (did, removed) = self.serve(index, now);
            work += did;
            if !removed {
                index += 1;
            }
        }
        if now.saturating_sub(self.last_sweep_at) >= spec::nanos(spec::SESSION_SWEEP_INTERVAL) {
            self.last_sweep_at = now;
            work += self.sweep(now);
        }
        work
    }

    fn flush_replies(&mut self) -> usize {
        let mut sent = 0;
        while let Some(reply) = self.pending.pop_front() {
            match self.replies.offer(reply) {
                Ok(()) => sent += 1,
                Err(reply) => {
                    self.pending.push_front(reply);
                    break;
                }
            }
        }
        sent
    }

    /// Queues a reply: into the queue, or into `pending` when it is full. Callers check
    /// [`PENDING_REPLIES`] first, so the bound holds.
    fn reply(&mut self, reply: Reply) {
        let reply = if self.pending.is_empty() {
            match self.replies.offer(reply) {
                Ok(()) => return,
                Err(reply) => reply,
            }
        } else {
            reply
        };
        debug_assert!(
            self.pending.len() < PENDING_REPLIES,
            "the pending replies passed their bound"
        );
        self.pending.push_back(reply);
    }

    fn endpoint_index(&self, registration_id: i64) -> Option<usize> {
        self.endpoints
            .iter()
            .position(|endpoint| endpoint.registration_id == registration_id)
    }

    fn live_index(&self, registration_id: i64) -> Option<usize> {
        self.live
            .iter()
            .position(|live| live.registration_id == registration_id)
    }

    fn handle(&mut self, request: Request, now: i64) {
        match request {
            Request::AddEndpoint {
                registration_id,
                socket,
                wanted,
            } => {
                debug_assert!(self.endpoints.len() < spec::MAX_RECEIVE_ENDPOINTS);
                self.endpoints.push(Endpoint {
                    registration_id,
                    socket,
                    wanted,
                });
            }
            Request::RemoveEndpoint { registration_id } => {
                if let Some(index) = self.endpoint_index(registration_id) {
                    drop(self.endpoints.swap_remove(index));
                }
                // The records keyed by the endpoint die with it; an image on it was
                // retired before the conductor closed it.
                let mut index = 0;
                while index < self.sessions.records.len() {
                    let record = self.sessions.records[index];
                    if record.key.endpoint_id == registration_id
                        && !matches!(record.session, Session::Image { .. })
                    {
                        self.sessions.remove(record.key);
                    } else {
                        index += 1;
                    }
                }
                self.reply(Reply::EndpointClosed { registration_id });
            }
            Request::AddInterest {
                endpoint_id,
                stream_id,
            } => {
                if let Some(index) = self.endpoint_index(endpoint_id) {
                    let wanted = &mut self.endpoints[index].wanted;
                    if let Some(position) = wanted
                        .iter()
                        .position(|interest| interest.stream_id == stream_id)
                    {
                        wanted[position].count += 1;
                    } else if wanted.len() < wanted.capacity() {
                        // The conductor bounds the subscriptions, so the list it
                        // preallocated has room; a list without is left as it is.
                        wanted.push(Interest {
                            stream_id,
                            count: 1,
                        });
                    }
                }
            }
            Request::RemoveInterest {
                endpoint_id,
                stream_id,
                subscription_id,
            } => {
                if let Some(index) = self.endpoint_index(endpoint_id) {
                    let wanted = &mut self.endpoints[index].wanted;
                    if let Some(position) = wanted
                        .iter()
                        .position(|interest| interest.stream_id == stream_id)
                    {
                        wanted[position].count = wanted[position].count.saturating_sub(1);
                        if wanted[position].count == 0 {
                            wanted.swap_remove(position);
                        }
                    }
                }
                self.remove_readers(ReaderKey::Subscription(subscription_id), None);
            }
            Request::AddImage(add) => self.add_image(add, now),
            Request::RefuseImage {
                endpoint_id,
                session_id,
                stream_id,
            } => {
                let key = SessionKey {
                    endpoint_id,
                    stream_id,
                    session_id,
                };
                if let Some(index) = self.sessions.get(key)
                    && self.sessions.records[index].session == Session::InProgress
                {
                    self.sessions.remove(key);
                }
            }
            Request::AddReader {
                registration_id,
                key,
                counter,
                floor,
            } => {
                if let Some(image_index) = self.live_index(registration_id)
                    && self.readers.len() < MAX_READERS
                {
                    self.readers.push(Reader {
                        image_index,
                        key,
                        counter,
                        floor,
                        rejected: false,
                        leaving: false,
                    });
                }
            }
            Request::AttachReader {
                registration_id,
                key,
                counter,
            } => {
                if let Some(image_index) = self.live_index(registration_id)
                    && let Some(reader) = self
                        .readers
                        .iter_mut()
                        .find(|reader| reader.image_index == image_index && reader.key == key)
                {
                    reader.counter = Some(counter);
                }
            }
            Request::RemoveReader {
                registration_id,
                key,
            } => {
                let image_index = self.live_index(registration_id);
                self.remove_readers(key, image_index);
            }
        }
    }

    /// Removes every reader with `key`, on one image or on all, answering each with
    /// [`Reply::ReaderRemoved`]. An image left with no subscription reader is retired
    /// by `serve` on this cycle, or on the first with room for the reply.
    fn remove_readers(&mut self, key: ReaderKey, image_index: Option<usize>) {
        for reader in &mut self.readers {
            if reader.key == key && image_index.is_none_or(|image| image == reader.image_index) {
                reader.leaving = true;
            }
        }
        self.flush_leaving();
    }

    /// Drops every reader marked as leaving whose reply fits the pending replies'
    /// bound, answering each with [`Reply::ReaderRemoved`]; the rest wait, read and
    /// counted no more, for the next cycle, so that a remove handled with the pending
    /// replies just under their bound cannot push them past it (invariant 3).
    fn flush_leaving(&mut self) {
        let mut index = 0;
        while index < self.readers.len() {
            let reader = &self.readers[index];
            if !reader.leaving {
                index += 1;
                continue;
            }
            if self.pending.len() >= PENDING_REPLIES {
                break;
            }
            let key = reader.key;
            let registration_id = self
                .live
                .get(reader.image_index)
                .map_or(0, |live| live.registration_id);
            drop(self.readers.swap_remove(index));
            self.reply(Reply::ReaderRemoved {
                registration_id,
                key,
            });
        }
    }

    /// The conductor's image for a setup the receiver handed it: into the session's
    /// record, with its readers, and the first status message at once (`spec/udp.md`,
    /// "The receiver", step 1). An image for a session no longer in progress, or whose
    /// stream no subscription on the endpoint wants any more, is retired at once.
    fn add_image(&mut self, add: AddImage, now: i64) {
        let key = SessionKey {
            endpoint_id: add.endpoint_id,
            stream_id: add.stream_id,
            session_id: add.session_id,
        };
        let in_progress = self
            .sessions
            .get(key)
            .is_some_and(|index| self.sessions.records[index].session == Session::InProgress);
        let wanted = self
            .endpoint_index(add.endpoint_id)
            .is_some_and(|index| self.endpoints[index].wants(add.stream_id));
        let subscribed = add
            .readers
            .iter()
            .any(|(key, _, _)| matches!(key, ReaderKey::Subscription(_)));
        // Kept, or retired at once: an image retired on arrival takes no reader, so
        // that `serve` retries the retire when the pending replies' bound puts it off
        // here; the conductor frees the readers' records on the retire's reply.
        let kept = in_progress && wanted && subscribed;
        let positions = add.log.positions();
        let term_length = i64::from(positions.term_length());
        let window = i32::try_from(add.log.header().receiver_window_length()).unwrap_or(i32::MAX);
        let live = Live {
            registration_id: add.registration_id,
            key,
            source: SockaddrStorage::from(add.source),
            positions,
            term_length,
            receiver_window_length: window,
            threshold: i64::from(status_message_threshold(window)),
            log: add.log,
            writer: add.writer,
            hwm: add.hwm,
            rebuild: add.rebuild,
            start_position: add.start_position,
            hwm_position: add.start_position,
            rebuild_position: add.start_position,
            last_sm_position: add.start_position,
            last_sm_at: now,
            clean_position: add.start_position,
            gap: None,
            gap_cursor: None,
            last_activity_at: now,
            end_of_stream: None,
            ended: false,
            end_of_stream_sms: 0,
            state: ImageState::Pending,
            slowest: add.start_position,
            fastest: add.start_position,
            reader_count: if kept { add.readers.len() } else { 0 },
            subscriber_count: if kept {
                add.readers
                    .iter()
                    .filter(|(key, _, _)| matches!(key, ReaderKey::Subscription(_)))
                    .count()
            } else {
                0
            },
        };
        debug_assert!(self.live.len() < spec::MAX_IMAGES);
        let index = self.live.len();
        self.live.push(live);
        if kept {
            for (key, counter, floor) in add.readers {
                if self.readers.len() < MAX_READERS {
                    self.readers.push(Reader {
                        image_index: index,
                        key,
                        counter,
                        floor,
                        rejected: false,
                        leaving: false,
                    });
                }
            }
        }
        self.live[index].state = ImageState::Live;
        self.statics.image_created.increment();
        self.live[index]
            .hwm
            .store_release(self.live[index].hwm_position);
        self.live[index]
            .rebuild
            .store_release(self.live[index].rebuild_position);
        if kept {
            if let Some(record) = self.sessions.get(key) {
                self.sessions.records[record].session = Session::Image { index };
            }
            self.send_status_message(index, now);
            return;
        }
        // Not wanted any more, or the record went with its endpoint: retired as step 5
        // says, which frees the readers and puts the record on cool-down. A retire the
        // pending replies' bound puts off here is tried again by `serve` every cycle,
        // since the image has no subscription reader.
        if self.sessions.get(key).is_none() {
            let _ = self.sessions.insert(key, Session::InProgress);
        }
        if let Some(record) = self.sessions.get(key) {
            self.sessions.records[record].session = Session::Image { index };
        }
        if self.pending.len() < PENDING_REPLIES {
            self.retire(index, ImageState::Unsubscribed, now);
        }
    }

    /// Retires the image at `index` in `state`: its readers go, its record goes on
    /// cool-down, the transition counts, and the conductor is told; the log, the writer
    /// and the counters drop with it (`spec/udp.md`, "The receiver", step 5). The caller
    /// checked the pending replies' bound. The log's end of stream position is the
    /// stream's own, stored when the stream ended, or not ended at all: a retire for
    /// the liveness timeout or the last subscription's removal is not an end, and a
    /// subscriber that read one as an end would be told a lie (invariant 1).
    fn retire(&mut self, index: usize, state: ImageState, now: i64) {
        // A last scan to the first gap or the high-water mark, whatever a cycle's bound,
        // in calls of the bound each, at most the window's worth and once per image:
        // nothing is inserted after this, the reply's rebuild position is the end of
        // the contiguous frames, and the conductor stops a recording of the image
        // there once the recorder has written up to it.
        {
            let live = &mut self.live[index];
            let mut calls = live.term_length / i64::from(spec::REBUILD_SCAN_LIMIT) + 2;
            while live.hwm_position > live.rebuild_position && calls > 0 {
                calls -= 1;
                let Ok(scan) = live.writer.scan(
                    live.rebuild_position,
                    live.hwm_position,
                    spec::REBUILD_SCAN_LIMIT,
                ) else {
                    break;
                };
                if scan.rebuild_position <= live.rebuild_position {
                    break;
                }
                live.rebuild_position = scan.rebuild_position;
                live.rebuild.store_release(scan.rebuild_position);
            }
        }
        let last = self.live.len() - 1;
        let live = self.live.swap_remove(index);
        // The readers of the retired image go; those of the image that moved into its
        // place follow it.
        let mut reader = 0;
        while reader < self.readers.len() {
            if self.readers[reader].image_index == index {
                drop(self.readers.swap_remove(reader));
            } else {
                if self.readers[reader].image_index == last {
                    self.readers[reader].image_index = index;
                }
                reader += 1;
            }
        }
        if index != last {
            let moved = self.live[index].key;
            if let Some(record) = self.sessions.get(moved) {
                self.sessions.records[record].session = Session::Image { index };
            }
        }
        if let Some(record) = self.sessions.get(live.key) {
            self.sessions.records[record].session = Session::CoolDown {
                until: now.saturating_add(spec::nanos(spec::IMAGE_COOL_DOWN)),
            };
        }
        match state {
            ImageState::Ended => self.statics.image_ended.increment(),
            ImageState::TimedOut => self.statics.image_timed_out.increment(),
            ImageState::Unsubscribed => self.statics.image_unsubscribed.increment(),
            ImageState::Pending | ImageState::Live | ImageState::Removed => {}
        }
        let registration_id = live.registration_id;
        drop(live);
        self.reply(Reply::Retired {
            registration_id,
            state,
        });
    }

    /// Takes every datagram that arrived at the endpoint and acts on it; returns how
    /// many arrived. A receive the operating system refused is counted in
    /// `receiver.receive_failed` and retried on the next cycle, and is not work, as the
    /// sender's is not.
    fn poll_endpoint(&mut self, index: usize, now: i64) -> usize {
        let count = match self.endpoints[index].socket.receive(&mut self.buffers) {
            Ok(count) => count,
            Err(_) => {
                self.statics.receive_failed.increment();
                return 0;
            }
        };
        for datagram in 0..count {
            self.dispatch(index, datagram, now);
        }
        count
    }

    /// One datagram from the endpoint's poll, by its first eight bytes and its session's
    /// record (`spec/udp.md`, "The receiver", steps 1, 2 and 6).
    fn dispatch(&mut self, endpoint_index: usize, datagram: usize, now: i64) {
        let endpoint_id = self.endpoints[endpoint_index].registration_id;
        let (bytes, source) = self.buffers.datagram(datagram);
        let Some(&source) = source else {
            return;
        };
        // The frame's version and type, from its first eight bytes: a data datagram is
        // read whole below; a control frame is parsed as one.
        let Some(head) = bytes.get(..8) else {
            self.statics.dropped_frames.increment();
            return;
        };
        let frame_length = i32::from_le_bytes([head[0], head[1], head[2], head[3]]);
        let frame_type = u16::from_le_bytes([head[6], head[7]]);
        let is_data = (frame_type == FrameType::Data as u16 || frame_type == FrameType::Pad as u16)
            && frame_length != 0;
        if is_data {
            self.on_data(endpoint_index, endpoint_id, datagram, source, now);
            return;
        }
        // A control frame or a heartbeat, parsed against the longest term the spec
        // admits, so that only its shape is checked here: a heartbeat's term offset is
        // held against its image's own term in `on_heartbeat`, once the session has an
        // image, since a setup frame may name a term longer than the driver's own
        // (`spec/udp.md`, "What a reader rejects", `term-offset`); a setup frame checks
        // its offset against the term it carries, and a status message or a NAK at a
        // receive endpoint is not read.
        let frame = match parse(bytes, MAX_TERM_LENGTH) {
            Ok(frame) => frame,
            Err(_) => {
                self.statics.dropped_frames.increment();
                return;
            }
        };
        match frame {
            ControlFrame::Setup(setup) => {
                self.on_setup(endpoint_index, endpoint_id, setup, source, now)
            }
            ControlFrame::Heartbeat(heartbeat) => {
                self.on_heartbeat(endpoint_index, endpoint_id, heartbeat, source, now);
            }
            ControlFrame::RttMeasurement(measurement) => {
                self.on_rtt_measurement(endpoint_index, endpoint_id, measurement);
            }
            // A status message or a NAK at a receive endpoint is a frame this side does
            // not handle, dropped and counted as "The datagram" says; data was
            // dispatched above.
            ControlFrame::StatusMessage(_) | ControlFrame::Nak(_) => {
                self.statics.dropped_frames.increment();
            }
            ControlFrame::Data => {}
        }
    }

    /// A data datagram: into the image's log within the bounds of the last status
    /// message, or dropped by the session's state (`spec/udp.md`, "The receiver", steps
    /// 1 and 2).
    fn on_data(
        &mut self,
        endpoint_index: usize,
        endpoint_id: i64,
        datagram: usize,
        source: SockaddrStorage,
        now: i64,
    ) {
        let (bytes, _) = self.buffers.datagram(datagram);
        // The first frame's header, checked against the datagram as the datagram check
        // will check it again: the session and stream it names pick the record.
        let first = match DataFrameHeader::parse_within(bytes, bytes.len()) {
            Ok(header) => header,
            Err(_) => {
                self.statics.rejected_datagrams.increment();
                return;
            }
        };
        let key = SessionKey {
            endpoint_id,
            stream_id: first.stream_id(),
            session_id: first.session_id(),
        };
        let end_of_stream = first.flags().contains(Flags::EOS);
        let session = self
            .sessions
            .get(key)
            .map(|index| self.sessions.records[index].session);
        let index = match session {
            Some(Session::Image { index }) => index,
            Some(Session::Pending { .. }) => {
                self.statics.dropped_pending.increment();
                return;
            }
            Some(Session::InProgress) => {
                self.statics.dropped_in_progress.increment();
                return;
            }
            Some(Session::CoolDown { .. }) => {
                self.statics.dropped_cool_down.increment();
                return;
            }
            None => {
                self.elicit_setup(endpoint_index, key, source, end_of_stream, now);
                return;
            }
        };
        // The hot path: the datagram checked whole, the receiver's bounds, then each
        // frame inserted at its term offset.
        let Self {
            buffers,
            live,
            statics,
            ..
        } = self;
        let (bytes, _) = buffers.datagram(datagram);
        let image = &mut live[index];
        let datagram = match Datagram::check(bytes, image.positions.term_length()) {
            Ok(datagram) => datagram,
            Err(_) => {
                statics.rejected_datagrams.increment();
                return;
            }
        };
        let term_id = first.term_id();
        let position = image
            .positions
            .position(term_id, datagram.first_term_offset());
        let bounds = receiver_bounds(
            image.last_sm_position,
            position,
            i32::try_from(datagram.length()).unwrap_or(i32::MAX),
            image.positions.term_length(),
        );
        if bounds.under_run {
            statics.under_runs.increment();
            return;
        }
        if bounds.over_run {
            statics.over_runs.increment();
            return;
        }
        let mut offset = datagram.first_term_offset();
        for frame in datagram.frames() {
            match image.writer.insert(term_id, offset, frame.bytes) {
                Ok(true) => {
                    statics.bytes_received.add(frame.bytes.len() as i64);
                    statics.frames_received.increment();
                }
                // A duplicate changes nothing; a frame the writer refused was checked
                // as the datagram was, so a refusal is a bug, counted and not hidden.
                Ok(false) => {}
                Err(_) => statics.rejected_datagrams.increment(),
            }
            offset += frame.bytes.len() as u32;
        }
        let end = position.saturating_add(i64::from(datagram.length()));
        if end > image.hwm_position {
            image.hwm_position = end;
            image.hwm.store_release(end);
        }
        image.last_activity_at = now;
    }

    /// A data datagram or a heartbeat for a session with no record: a stream a
    /// subscription wants is asked for its setup with a status message carrying the
    /// send-setup flag, once per pending setup timeout; a stream nobody wants, or a
    /// datagram with EOS, asks for nothing (`spec/udp.md`, "The receiver", step 1).
    fn elicit_setup(
        &mut self,
        endpoint_index: usize,
        key: SessionKey,
        source: SockaddrStorage,
        end_of_stream: bool,
        now: i64,
    ) {
        if end_of_stream || !self.endpoints[endpoint_index].wants(key.stream_id) {
            self.statics.dropped_unwanted.increment();
            return;
        }
        let until = now.saturating_add(spec::nanos(spec::PENDING_SETUP_TIMEOUT));
        if self
            .sessions
            .insert(key, Session::Pending { until })
            .is_none()
        {
            self.statics.sessions_full.increment();
            return;
        }
        let message = StatusMessage {
            flags: STATUS_MESSAGE_SEND_SETUP,
            session_id: key.session_id,
            stream_id: key.stream_id,
            consumption_term_id: 0,
            consumption_term_offset: 0,
            receiver_window_length: 0,
            receiver_id: self.receiver_id,
            group_tag: None,
        };
        let bytes = message.to_bytes();
        let sent = self.endpoints[endpoint_index]
            .socket
            .send_to(&bytes[..message.frame_length()], &source);
        self.statics.count_sent(sent, &self.statics.setups_elicited);
        self.statics.dropped_pending.increment();
    }

    /// A setup frame (`spec/udp.md`, "The receiver", step 1): for a wanted stream and a
    /// session with no record or pending a setup, the session goes in progress and the
    /// conductor is handed the frame; otherwise dropped by the session's state.
    fn on_setup(
        &mut self,
        endpoint_index: usize,
        endpoint_id: i64,
        setup: SetupFrame,
        source: SockaddrStorage,
        now: i64,
    ) {
        self.statics.setups_received.increment();
        if !self.endpoints[endpoint_index].wants(setup.stream_id) {
            self.statics.setups_unwanted.increment();
            return;
        }
        let key = SessionKey {
            endpoint_id,
            stream_id: setup.stream_id,
            session_id: setup.session_id,
        };
        let session = self
            .sessions
            .get(key)
            .map(|index| self.sessions.records[index].session);
        match session {
            Some(Session::Image { .. }) => return,
            Some(Session::InProgress) => {
                self.statics.dropped_in_progress.increment();
                return;
            }
            Some(Session::CoolDown { .. }) => {
                self.statics.dropped_cool_down.increment();
                return;
            }
            Some(Session::Pending { .. }) | None => {}
        }
        let Some(source) = socket_addr(&source) else {
            self.statics.dropped_frames.increment();
            return;
        };
        // No room for the reply: the session stays as it was, and the sender's next
        // setup tries again.
        if self.pending.len() >= PENDING_REPLIES {
            self.statics.dropped_in_progress.increment();
            return;
        }
        match self.sessions.get(key) {
            Some(index) => self.sessions.records[index].session = Session::InProgress,
            None => {
                if self.sessions.insert(key, Session::InProgress).is_none() {
                    self.statics.sessions_full.increment();
                    return;
                }
            }
        }
        let _ = now;
        self.reply(Reply::Setup {
            endpoint_id,
            source,
            setup,
        });
    }

    /// A heartbeat (`spec/udp.md`, "Heartbeat"; "The receiver", step 2): its term offset
    /// checked against the image's term, it moves the high-water mark, counts as
    /// activity at or above the last status message's position less a term, and with
    /// EOS names the end of stream.
    fn on_heartbeat(
        &mut self,
        endpoint_index: usize,
        endpoint_id: i64,
        heartbeat: cavalry_proto::udp::Heartbeat,
        source: SockaddrStorage,
        now: i64,
    ) {
        let key = SessionKey {
            endpoint_id,
            stream_id: heartbeat.stream_id,
            session_id: heartbeat.session_id,
        };
        let session = self
            .sessions
            .get(key)
            .map(|index| self.sessions.records[index].session);
        let index = match session {
            Some(Session::Image { index }) => index,
            Some(Session::Pending { .. }) => {
                self.statics.dropped_pending.increment();
                return;
            }
            Some(Session::InProgress) => {
                self.statics.dropped_in_progress.increment();
                return;
            }
            Some(Session::CoolDown { .. }) => {
                self.statics.dropped_cool_down.increment();
                return;
            }
            None => {
                self.elicit_setup(
                    endpoint_index,
                    key,
                    source,
                    heartbeat.is_end_of_stream(),
                    now,
                );
                return;
            }
        };
        let image = &mut self.live[index];
        // The term offset against the image's term, the one its setup frame named, which
        // may be longer than the driver's own: one not below it is the spec's
        // `term-offset` rejection, dropped and counted (`spec/udp.md`, "What a reader
        // rejects"). `parse` checked the rest of the offset's shape.
        if i64::from(heartbeat.term_offset) >= image.term_length {
            self.statics.dropped_frames.increment();
            return;
        }
        let position = image
            .positions
            .position(heartbeat.term_id, heartbeat.term_offset as u32);
        let half = image.term_length / 2;
        if position > image.last_sm_position.saturating_add(half) {
            self.statics.over_runs.increment();
            return;
        }
        // Accepted: counted only now, so that `receiver.heartbeats_received` and
        // `receiver.over_runs` read as their docs say, one or the other per heartbeat.
        self.statics.heartbeats_received.increment();
        if position > image.hwm_position {
            image.hwm_position = position;
            image.hwm.store_release(position);
        }
        if position >= image.last_sm_position.saturating_sub(image.term_length) {
            image.last_activity_at = now;
        }
        if heartbeat.is_end_of_stream() && image.end_of_stream.is_none() {
            image.end_of_stream = Some(position);
        }
    }

    /// An RTT measurement with the reply flag for an image's session, naming this
    /// receiver or any, is answered as the sender answers one; one for a session with
    /// no image, or for another receiver, is dropped and counted (`spec/udp.md`, "The
    /// receiver", step 6).
    fn on_rtt_measurement(
        &mut self,
        endpoint_index: usize,
        endpoint_id: i64,
        measurement: RttMeasurement,
    ) {
        if !measurement.is_probe() {
            return;
        }
        let key = SessionKey {
            endpoint_id,
            stream_id: measurement.stream_id,
            session_id: measurement.session_id,
        };
        let image = match self
            .sessions
            .get(key)
            .map(|i| self.sessions.records[i].session)
        {
            Some(Session::Image { index }) => index,
            _ => {
                self.statics.dropped_unwanted.increment();
                return;
            }
        };
        if measurement.receiver_id != 0 && measurement.receiver_id != self.receiver_id {
            self.statics.dropped_unwanted.increment();
            return;
        }
        let bytes = measurement.answer().to_bytes();
        let sent = self.endpoints[endpoint_index]
            .socket
            .send_to(&bytes, &self.live[image].source);
        self.statics
            .count_sent(sent, &self.statics.rtt_replies_sent);
    }

    /// Loads every reader's position once per cycle into its image's slowest and
    /// fastest, each checked against its bound (ADR 0010): one outside it is not used,
    /// counts `log_buffer.rejected_position` and is reported to the conductor, which
    /// evicts its client, with a reply of its own for every rejected reader; one whose
    /// reply finds the pending replies at their bound is checked again next cycle, and
    /// counted and reported then. Each image's subscription readers are counted too,
    /// rejected or not, since a subscription holds its image until the conductor removes
    /// it (`serve`).
    fn load_readers(&mut self) {
        self.flush_leaving();
        for live in &mut self.live {
            live.slowest = i64::MAX;
            live.fastest = i64::MIN;
            live.reader_count = 0;
            live.subscriber_count = 0;
        }
        for index in 0..self.readers.len() {
            let (image_index, key, rejected, leaving) = {
                let reader = &self.readers[index];
                (
                    reader.image_index,
                    reader.key,
                    reader.rejected,
                    reader.leaving,
                )
            };
            if leaving {
                continue;
            }
            let Some(live) = self.live.get_mut(image_index) else {
                continue;
            };
            if matches!(key, ReaderKey::Subscription(_)) {
                live.subscriber_count += 1;
            }
            if rejected {
                continue;
            }
            let position = self.readers[index].position();
            if position < live.start_position
                || position > live.positions.max_position()
                || position % ALIGN != 0
            {
                if self.pending.len() < PENDING_REPLIES {
                    let registration_id = live.registration_id;
                    self.readers[index].rejected = true;
                    self.statics.rejected_position.increment();
                    self.reply(Reply::ReaderRejected {
                        registration_id,
                        key,
                    });
                }
                continue;
            }
            live.slowest = live.slowest.min(position);
            live.fastest = live.fastest.max(position);
            live.reader_count += 1;
        }
    }

    /// One image's duties for this cycle (`spec/udp.md`, "The receiver", steps 3 to 5);
    /// returns how much work they were and whether the image left the list.
    fn serve(&mut self, index: usize, now: i64) -> (usize, bool) {
        // 5, first: an image whose last subscription left is retired as unsubscribed,
        // and one with a recording alone too, since a recording is not a subscriber.
        // Checked every cycle from the readers loaded, so that a retire the pending
        // replies' bound put off, on the request that removed the last reader or on the
        // image's arrival, is tried again until there is room (invariant 3).
        if self.live[index].state == ImageState::Live
            && self.live[index].subscriber_count == 0
            && self.pending.len() < PENDING_REPLIES
        {
            self.retire(index, ImageState::Unsubscribed, now);
            return (1, true);
        }
        let mut work = 0;
        // 3. The rebuild scan and the NAK for the gap it finds.
        work += self.rebuild(index, now);
        // The end of stream: once the rebuild reaches the position the heartbeat named,
        // the position is stored and the status messages carry the flag.
        {
            let live = &mut self.live[index];
            if let Some(end) = live.end_of_stream
                && !live.ended
                && live.rebuild_position >= end
            {
                let _ = live.log.set_end_of_stream_position(end);
                live.ended = true;
                work += 1;
            }
        }
        // 4. A status message on its triggers.
        {
            let live = &self.live[index];
            let consumption = if live.reader_count > 0 {
                live.slowest
            } else {
                live.last_sm_position
            };
            let due = consumption >= live.last_sm_position.saturating_add(live.threshold)
                || now.saturating_sub(live.last_sm_at) >= spec::nanos(STATUS_MESSAGE_INTERVAL);
            let eos_done =
                live.ended && live.end_of_stream_sms >= spec::END_OF_STREAM_STATUS_MESSAGES;
            if due && !eos_done {
                self.send_status_message(index, now);
                work += 1;
            }
        }
        // 5. The end: every reader past the end of stream, or no activity for the
        // liveness timeout.
        if self.pending.len() < PENDING_REPLIES {
            let live = &self.live[index];
            let timed_out =
                now.saturating_sub(live.last_activity_at) >= spec::nanos(IMAGE_LIVENESS_TIMEOUT);
            if live.ended {
                let reached = live
                    .end_of_stream
                    .is_some_and(|end| live.reader_count > 0 && live.slowest >= end);
                if reached || timed_out {
                    self.retire(index, ImageState::Ended, now);
                    return (work + 1, true);
                }
            } else if timed_out {
                self.retire(index, ImageState::TimedOut, now);
                return (work + 1, true);
            }
        }
        (work, false)
    }

    /// The rebuild scan of the image at `index`: the contiguous frames from the greater
    /// of the rebuild position and the fastest reader, the gap after them, and the NAK
    /// the gap is due (`spec/udp.md`, "The receiver", step 3; ADR 0042). A cycle reads
    /// at most [`spec::REBUILD_SCAN_LIMIT`] bytes of the image's term, in one scan: from
    /// the rebuild position, or, when the last scan ended at its bound inside the gap
    /// there, on from where it stopped, so that a gap longer than the bound is walked
    /// once, a bound of term per cycle, and the NAK that follows names it whole.
    fn rebuild(&mut self, index: usize, now: i64) -> usize {
        let endpoint = self.endpoint_index(self.live[index].key.endpoint_id);
        let live = &mut self.live[index];
        if live.hwm_position <= live.rebuild_position {
            live.gap = None;
            live.gap_cursor = None;
            return 0;
        }
        let from = if live.reader_count > 0 {
            live.rebuild_position.max(live.fastest)
        } else {
            live.rebuild_position
        };
        let hwm = live.hwm_position;
        let mut work = 0;
        // The continuation: the last scan stopped at its bound inside the gap at the
        // rebuild position, so this cycle scans on from there instead of over the same
        // words again. The rebuild position stays where that scan left it: a retransmit
        // that lands at it is found by the scan from it, on the cycle after the
        // continuation finds the gap's end. A frame at the cursor itself ends the gap
        // there. A cursor that no longer names the gap at the rebuild position, because
        // a reader moved past it, is dropped for a scan from the rebuild position.
        let continued = match live.gap_cursor {
            Some(cursor)
                if cursor > from
                    && cursor < hwm
                    && live.gap.is_some_and(|gap| gap.start == from) =>
            {
                match live.writer.scan(cursor, hwm, spec::REBUILD_SCAN_LIMIT) {
                    Ok(more) if more.rebuild_position > cursor => Some(ImageScan {
                        rebuild_position: from,
                        next_frame: Some(cursor),
                        scanned_to: cursor,
                    }),
                    Ok(more) => Some(ImageScan {
                        rebuild_position: from,
                        next_frame: more.next_frame,
                        scanned_to: more.scanned_to,
                    }),
                    Err(_) => None,
                }
            }
            _ => None,
        };
        let scan = match continued {
            Some(scan) => scan,
            None => {
                let Ok(scan) = live.writer.scan(from, hwm, spec::REBUILD_SCAN_LIMIT) else {
                    live.gap_cursor = None;
                    return 0;
                };
                if scan.rebuild_position > live.rebuild_position {
                    live.rebuild_position = scan.rebuild_position;
                    live.rebuild.store_release(scan.rebuild_position);
                    work += 1;
                }
                scan
            }
        };
        if live.rebuild_position >= hwm {
            live.gap = None;
            live.gap_cursor = None;
            return work;
        }
        // The gap: from the rebuild position to the next frame, or as far as the scan
        // looked. Its end is known when a frame or the high-water mark ended the scan,
        // and open when the bound did, in which case the next cycle scans on from there.
        // A scan the bound ended among the frames knows no gap yet.
        let (gap_end, end_known) = match scan.next_frame {
            Some(next) => (next, true),
            None if scan.scanned_to > live.rebuild_position => {
                (scan.scanned_to, scan.scanned_to >= hwm)
            }
            None => {
                live.gap = None;
                live.gap_cursor = None;
                return work;
            }
        };
        live.gap_cursor = if end_known {
            None
        } else {
            Some(scan.scanned_to)
        };
        let start = live.rebuild_position;
        let length = gap_end - start;
        // A gap first seen counts; one that changes restarts the NAK delay: its end
        // moves out as a scan looks further or the high-water mark rises, or a frame
        // landed inside it and its end came in. A scan the bound ended inside a gap
        // already known further says nothing new, and the gap stays as it is, so that
        // the delay runs on and the NAK names what is known.
        match &mut live.gap {
            Some(gap) if gap.start == start => {
                if length > gap.length || (end_known && length < gap.length) {
                    gap.length = length;
                    gap.seen_at = now;
                    gap.last_nak_at = None;
                    work += 1;
                }
            }
            _ => {
                self.statics.gaps_detected.increment();
                live.gap = Some(Gap {
                    start,
                    length,
                    seen_at: now,
                    last_nak_at: None,
                });
                work += 1;
            }
        }
        let Some(gap) = live.gap else {
            return work;
        };
        let delayed = now.saturating_sub(gap.seen_at) >= spec::nanos(NAK_DELAY);
        let retry_due = gap
            .last_nak_at
            .is_none_or(|at| now.saturating_sub(at) >= spec::nanos(NAK_RETRY));
        if delayed && retry_due {
            // A NAK names a range within one term.
            let term_offset = live.positions.term_offset_of(gap.start);
            let in_term = i64::from(live.positions.term_length()) - i64::from(term_offset);
            let nak = Nak {
                flags: 0,
                session_id: live.key.session_id,
                stream_id: live.key.stream_id,
                term_id: live.positions.term_id_of(gap.start),
                term_offset: term_offset as i32,
                length: i32::try_from(gap.length.min(in_term)).unwrap_or(i32::MAX),
            };
            if let Some(gap) = &mut live.gap {
                gap.last_nak_at = Some(now);
            }
            let bytes = nak.to_bytes();
            let source = live.source;
            if let Some(endpoint) = endpoint {
                let sent = self.endpoints[endpoint].socket.send_to(&bytes, &source);
                self.statics.count_sent(sent, &self.statics.naks_sent);
            }
            work += 1;
        }
        work
    }

    /// Sends the image's status message: the slowest reader's position, the window, the
    /// receiver id and the end-of-stream flag once the stream has ended, after cleaning
    /// the log a term behind the position it carries (`spec/udp.md`, "The receiver",
    /// step 4).
    fn send_status_message(&mut self, index: usize, now: i64) {
        let endpoint = self.endpoint_index(self.live[index].key.endpoint_id);
        let live = &mut self.live[index];
        let consumption = if live.reader_count > 0 {
            live.slowest.max(live.last_sm_position)
        } else {
            live.last_sm_position
        };
        // Before s rises, the log is cleaned up to a term behind the new s, whole terms
        // at a time, so that a term is zero before a frame lands in it.
        let clean_to = consumption.saturating_sub(live.term_length);
        while live.clean_position < clean_to {
            match live.log.clean(live.clean_position, clean_to) {
                Ok(cleaned) if cleaned > live.clean_position => live.clean_position = cleaned,
                _ => break,
            }
        }
        let flags = if live.ended {
            STATUS_MESSAGE_END_OF_STREAM
        } else {
            0
        };
        let message = StatusMessage {
            flags,
            session_id: live.key.session_id,
            stream_id: live.key.stream_id,
            consumption_term_id: live.positions.term_id_of(consumption),
            consumption_term_offset: live.positions.term_offset_of(consumption) as i32,
            receiver_window_length: live.receiver_window_length,
            receiver_id: self.receiver_id,
            group_tag: None,
        };
        live.last_sm_position = consumption;
        live.last_sm_at = now;
        if live.ended {
            live.end_of_stream_sms = live.end_of_stream_sms.saturating_add(1);
        }
        let bytes = message.to_bytes();
        let source = live.source;
        if let Some(endpoint) = endpoint {
            let sent = self.endpoints[endpoint]
                .socket
                .send_to(&bytes[..message.frame_length()], &source);
            self.statics
                .count_sent(sent, &self.statics.status_messages_sent);
        }
    }

    /// Frees the session records whose pending setup or cool-down expired; returns how
    /// many went.
    fn sweep(&mut self, now: i64) -> usize {
        let mut freed = 0;
        let mut index = 0;
        while index < self.sessions.records.len() {
            let record = self.sessions.records[index];
            let expired = match record.session {
                Session::Pending { until } | Session::CoolDown { until } => now >= until,
                Session::InProgress | Session::Image { .. } => false,
            };
            if expired {
                self.sessions.remove(record.key);
                freed += 1;
            } else {
                index += 1;
            }
        }
        freed
    }
}

/// This driver's receiver id: an i64 drawn from the clock and the process id when the
/// receiver starts, mixed so that two drivers started in the same instant differ.
fn receiver_id(now: i64) -> i64 {
    let mut x = (now as u64) ^ (u64::from(std::process::id()) << 40);
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    x as i64
}

/// The `std` address of a datagram's source, for the image's source identity.
fn socket_addr(address: &SockaddrStorage) -> Option<SocketAddr> {
    if let Some(v4) = address.as_sockaddr_in() {
        return Some(SocketAddr::from(*v4));
    }
    if let Some(v6) = address.as_sockaddr_in6() {
        return Some(SocketAddr::from(*v6));
    }
    None
}

#[cfg(test)]
mod tests {
    //! The session table alone: its bound, its lookups and the backward-shift deletion
    //! that keeps every probe sound.

    use super::*;

    fn key(n: i64) -> SessionKey {
        SessionKey {
            endpoint_id: n,
            stream_id: (n % 7) as i32,
            session_id: (n * 31) as i32,
        }
    }

    #[test]
    fn the_session_table_fills_at_its_capacity_and_frees_on_remove() {
        let mut table = SessionTable::new();
        for n in 0..spec::MAX_SESSION_RECORDS as i64 {
            assert!(
                table.insert(key(n), Session::InProgress).is_some(),
                "record {n} fits"
            );
        }
        assert_eq!(table.records.len(), spec::MAX_SESSION_RECORDS);
        assert!(
            table.insert(key(-1), Session::InProgress).is_none(),
            "a 4097th record is refused"
        );
        for n in 0..spec::MAX_SESSION_RECORDS as i64 {
            assert_eq!(
                table.get(key(n)).map(|index| table.records[index].key),
                Some(key(n)),
                "record {n} is found"
            );
        }
        table.remove(key(17));
        assert!(table.get(key(17)).is_none());
        assert!(table.insert(key(-1), Session::InProgress).is_some());
        assert_eq!(
            table.get(key(-1)).map(|i| table.records[i].key),
            Some(key(-1))
        );
    }

    #[test]
    fn removing_records_keeps_every_other_record_findable() {
        let mut table = SessionTable::new();
        let count = 1000i64;
        for n in 0..count {
            table.insert(key(n), Session::Pending { until: n });
        }
        // Remove every third record, in an order that moves the last record into
        // holes and shifts slots back, then check every survivor and every removed key.
        for n in (0..count).step_by(3) {
            table.remove(key(n));
        }
        for n in 0..count {
            let found = table.get(key(n)).map(|i| table.records[i]);
            if n % 3 == 0 {
                assert!(found.is_none(), "record {n} was removed");
            } else {
                let record = found.unwrap_or_else(|| panic!("record {n} is still found"));
                assert_eq!(record.key, key(n));
                assert_eq!(record.session, Session::Pending { until: n });
            }
        }
        assert_eq!(table.records.len() as i64, count - (count + 2) / 3);
        // Removing a key that is not there changes nothing.
        table.remove(key(count + 5));
        assert_eq!(table.records.len() as i64, count - (count + 2) / 3);
    }

    #[test]
    fn the_receiver_id_differs_by_instant() {
        assert_ne!(receiver_id(1), receiver_id(2));
    }
}
