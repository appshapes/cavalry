//! The conductor: the one thread that keeps the driver's directory. On every duty cycle
//! it stores its heartbeat, reads and answers the command ring, unblocks it, times out
//! clients, and keeps every publication's positions, cleaning, unblocking and closing
//! as `spec/log-buffer.md`, "The driver" says; what each command means is
//! `spec/client-protocol.md` (ADR 0019). Nothing here is on the message path: a
//! message goes from a publisher's claim to a subscriber's handler through the log
//! buffer alone, and the conductor only moves the limit that bounds the publisher. It
//! loads a publication's producer position at most once every two milliseconds, so that the
//! publisher's cache line is not read on every cycle, and a move of that position is
//! not work for the idle strategy (ADR 0022). Recording is the recorder thread's
//! (`crate::recorder`): the conductor registers each recording, hands the recorder its
//! starts and stops through a bounded queue, takes its replies, allocates the two
//! counters once the recording id is known, and lets the recorder's position bound the
//! publisher's limit and the cleaning, so a slow disk back-pressures the publisher and
//! never loses a byte (ADR 0025). Replay is the replayer thread's (`crate::replayer`)
//! the same way: the conductor registers each replay, hands the replayer its start,
//! takes the reply that says the recording's slot and position check, creates the
//! replay's publication, a log of the driver's own at the replay's start position,
//! attaches the replayer's writer to it, and ends the replay when the replayer says it
//! ended, a client stops it, or it has had no subscriber for the connect timeout; the
//! publication's limit paces the replayer as it paces a publisher (ADR 0030).

use std::collections::VecDeque;
use std::fs::{File, TryLockError};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cavalry_log::dir::Recovery;

use cavalry_proto::channel::{Channel, ChannelError, FlowControl, PublicationPosition, UdpChannel};
use cavalry_proto::client_protocol::{
    Command, ErrorCode, Event, IPC_CHANNEL, MAX_STRING_LENGTH, NO_CHANNEL_STATUS, NULL_LENGTH,
    ProtocolError, ReplayStopReason,
};
use cavalry_proto::frame::FRAME_ALIGNMENT;
use cavalry_proto::udp::{MAX_DESTINATIONS, MAX_FLOW_CONTROL_RECEIVERS};
use cavalry_proto::udp::{SetupFrame, image_start};
use cavalry_shm::broadcast::{BroadcastBuffer, Transmitter};
use cavalry_shm::command_ring::{
    CommandRing, CommandRingError, Consumer, Producer as RingProducer,
    UnblockOutcome as RingUnblock,
};
use cavalry_shm::counters::generated::COUNTERS;
use cavalry_shm::counters::generated::{
    CHANNEL_CLOSED, CHANNEL_DESTINATIONS, CHANNEL_IMAGES, CHANNEL_OPENED, CHANNEL_PUBLICATIONS,
    CHANNEL_SOCKET_RCVBUF, CHANNEL_SOCKET_SNDBUF, CHANNEL_STATUS, CLIENT_PROTOCOL_ERRORS,
    CLIENT_PROTOCOL_REJECTED_COMMAND, CLIENT_TIMED_OUT, COMMAND_RING_REJECTED_RECORD,
    COMMAND_RING_REJECTED_TAIL, COMMAND_RING_UNBLOCKED, CONDUCTOR_IDLE_STRATEGY,
    DRIVER_LOG_CREATE_FAILED, DRIVER_REGISTRY_FULL, IMAGE_HWM_POSITION, IMAGE_REBUILD_POSITION,
    IMAGE_REMOVED, LOG_BUFFER_REJECTED_POSITION, LOG_BUFFER_REJECTED_TAIL, PUBLICATION_LIMIT,
    PUBLICATION_POSITION, PUBLICATION_UNBLOCKED, RECORDING_ARMED, RECORDING_ARMED_DROPPED,
    RECORDING_ARMED_FAILED, RECORDING_ARMED_STARTED, RECORDING_BYTES, RECORDING_CATALOG_FULL,
    RECORDING_CATALOG_TRUNCATED, RECORDING_DELETED, RECORDING_FILES_REMOVED,
    RECORDING_INDEX_REWRITTEN, RECORDING_INVALIDATED, RECORDING_POSITION, RECORDING_RECOVERED,
    RECORDING_RETENTION_FAILED, RECORDING_SLOT_DROPPED, RECORDING_START_FAILED, RECORDING_STARTED,
    RECORDING_STOPPED, RECORDING_SYNCED_POSITION, RECORDING_TRUNCATED, REPLAY_ENDED, REPLAY_FAILED,
    REPLAY_NOT_CONNECTED, REPLAY_POSITION, REPLAY_START_FAILED, REPLAY_STARTED, REPLAY_STOPPED,
    SENDER_FLOW_CONTROL_RECEIVERS, SENDER_LIMIT, SENDER_POSITION, SUBSCRIPTION_ADD_DEFERRED,
    SUBSCRIPTION_ADD_DEFERRED_TIMED_OUT, SUBSCRIPTION_ADD_DEFERRED_WAITING, SUBSCRIPTION_POSITION,
};
use cavalry_shm::counters::{Counter, CounterId, CountersError, CountersFile, Scope};
use cavalry_shm::log::layout as log_layout;
use cavalry_shm::log::{HeaderFields, LogBuffers, LogType, UnblockOutcome as LogUnblock};
use cavalry_shm::spsc;

use crate::error::DriverError;
use crate::idle::{IdleStrategy, Idler};
use crate::receiver::{self, ImageState, ReaderKey};
use crate::recorder::{Reply, Request, Start, StartFailure, SyncOutcome};
use crate::replayer;
use crate::sender;
use crate::socket::{ReceiveSocket, SendSocket};
use crate::spec;

/// The docs anchor an error event's message names.
const ERRORS_DOC: &str = "docs/reference/client-protocol.md#error-codes";
/// The docs anchor an `add counter` of a type that is not a client's names: the page
/// that lists every counter with its scope.
const COUNTERS_DOC: &str = "docs/reference/counters.md";

/// How many commands one duty cycle reads before it turns to the publications.
const COMMANDS_PER_CYCLE: usize = 16;

/// The deferred requests' bound: one attach and one stop per registered recording
/// (`Conductor::deferred` says why it holds).
const DEFERRED_CAPACITY: usize = 2 * spec::MAX_RECORDINGS;

/// The replayer's deferred requests' bound: one attach and one stop per registered
/// replay, by the argument on `Conductor::deferred`, and one recording stop per
/// registered replay, since a stop is sent only for a recording a registered replay
/// plays and a recording stops once.
const REPLAYER_DEFERRED_CAPACITY: usize = 3 * spec::MAX_REPLAYS;

/// The sender's deferred requests' bound: one close per publication the sender may
/// hold, a client's or a replay's over `cavalry:udp` (ADR 0043), and one remove per
/// endpoint, since an add that finds no room in the sender's queue is refused with code
/// 10 and never deferred (ADR 0041).
const SENDER_DEFERRED_CAPACITY: usize = spec::MAX_SENDER_PUBLICATIONS + spec::MAX_SEND_ENDPOINTS;

/// The receiver's deferred requests' bound: a remove per subscription and per endpoint,
/// an image or a refusal per session record in progress, and a reader's attach or
/// remove per image and per recording, since an `add subscription` that finds no room in
/// the receiver's queue is refused with code 10 and never deferred (ADR 0042).
const RECEIVER_DEFERRED_CAPACITY: usize = spec::MAX_SUBSCRIPTIONS
    + spec::MAX_RECEIVE_ENDPOINTS
    + spec::MAX_SESSION_RECORDS
    + spec::MAX_IMAGES
    + spec::MAX_RECORDINGS;

/// A frame's alignment as the positions count.
const FRAME_ALIGN: i64 = FRAME_ALIGNMENT as i64;

/// How long a starter waits for a command ring that is `not-ready` before it treats the
/// file as a dead driver's, polling at this interval.
const NOT_READY_POLL: Duration = Duration::from_millis(10);

/// The driver's clock: nanoseconds since the Unix epoch, the clock the consumer
/// heartbeat stores and every timeout is measured on; the recorder reads the same clock
/// once per duty cycle.
pub(crate) fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        })
}

/// The name of the file in which a driver writes its pid, for the starter it refuses.
const PID_FILE_NAME: &str = "pid";

/// How long a starter that was refused the lock waits before it reads the holder's pid
/// from the file: a holder that had only just taken the lock has written it by then.
const LOCK_PID_WAIT: Duration = Duration::from_millis(10);

/// A driver's hold on its directory: an exclusive advisory lock of the operating
/// system on the directory itself, taken before the directory is read and dropped
/// with this value, or with the process however it ends (`spec/client-protocol.md`,
/// "The driver directory"; ADR 0079).
#[derive(Debug)]
pub(crate) struct DirLock {
    _dir: File,
}

/// The directory protocol of `spec/client-protocol.md`, "The driver directory". The
/// starter creates the directory when it is not there and tries the lock on it once:
/// held by another, a driver runs or is starting there, and the answer is
/// `driver-active` with nothing changed. With the lock, the `pid` file says whose the
/// directory was. A pid there is a driver of this version, which held this lock and
/// has gone, whatever its heartbeat says. No pid is a directory that is new, or a
/// driver of an earlier version, which holds no lock and is told by the command
/// ring's heartbeat as before. Then the starter writes its own pid and removes
/// everything else in the directory, for the files it is about to create with
/// create-new semantics. The lock it returns is the driver's for its life.
pub(crate) fn prepare_dir(dir: &Path) -> Result<DirLock, DriverError> {
    let io = |what: &'static str, source: std::io::Error| DriverError::Io {
        what,
        path: dir.to_path_buf(),
        source,
    };
    std::fs::create_dir_all(dir).map_err(|source| io("create the directory", source))?;
    let lock = File::open(dir).map_err(|source| io("open the directory to lock it", source))?;
    let pid_path = dir.join(PID_FILE_NAME);
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            std::thread::sleep(LOCK_PID_WAIT);
            return Err(DriverError::Active {
                dir: dir.to_path_buf(),
                pid: holder_pid(&pid_path),
            });
        }
        Err(TryLockError::Error(source)) => {
            return Err(io("lock the directory", source));
        }
    }
    if holder_pid(&pid_path) == 0 {
        // No driver of this version left its pid here: one of an earlier version, which
        // holds no lock, may run here, and its heartbeat says whether it does.
        let deadline = now_nanos().saturating_add(spec::nanos(spec::DRIVER_TIMEOUT));
        loop {
            match CommandRing::open(dir, false) {
                Ok(ring) => {
                    let heartbeat = ring.consumer_heartbeat();
                    let fresh = heartbeat != cavalry_shm::command_ring::layout::DRIVER_CLOSED
                        && now_nanos().saturating_sub(heartbeat)
                            < spec::nanos(spec::DRIVER_TIMEOUT);
                    if fresh {
                        return Err(DriverError::Active {
                            dir: dir.to_path_buf(),
                            pid: ring.header().creator_pid(),
                        });
                    }
                    break;
                }
                Err(CommandRingError::NotReady) if now_nanos() < deadline => {
                    std::thread::sleep(NOT_READY_POLL);
                }
                Err(_) => break,
            }
        }
    }
    // The directory is this driver's: its pid, for the starter it will refuse, and for
    // the next one to know that a driver of this version was here.
    std::fs::write(&pid_path, format!("{}\n", std::process::id()))
        .map_err(|source| io("write the directory's pid file", source))?;
    // What the driver that has gone left: everything but the pid file just written.
    let entries =
        std::fs::read_dir(dir).map_err(|source| io("read the driver's directory", source))?;
    for entry in entries {
        let entry = entry.map_err(|source| io("read the driver's directory", source))?;
        if entry.file_name() == PID_FILE_NAME {
            continue;
        }
        let path = entry.path();
        let is_dir = entry
            .file_type()
            .map_err(|source| io("read the driver's directory", source))?
            .is_dir();
        if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        }
        .map_err(|source| io("remove what the last driver left in the directory", source))?;
    }
    Ok(DirLock { _dir: lock })
}

/// The pid in the directory's pid file, or 0 when the file is not there or holds no
/// number.
fn holder_pid(pid_path: &Path) -> i64 {
    std::fs::read_to_string(pid_path)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

/// One client the driver knows: its id and when it last sent a command.
struct Client {
    id: i64,
    last_seen: i64,
}

/// One counter a client added with `add counter` (`spec/client-protocol.md`,
/// "Commands"; ADR 0062): what `remove counter` names, and what goes with its client.
struct ClientCounter {
    client_id: i64,
    /// The add command's correlation id.
    registration_id: i64,
    /// The record the driver allocated; the client alone writes its value.
    counter_id: u32,
}

/// One registration of a publication by a client: what `remove publication` names.
#[derive(Clone, Copy)]
struct Registration {
    client_id: i64,
    correlation_id: i64,
}

/// One publication: a log and its counters, shared by its registrations.
struct Publication {
    /// The first registration's correlation id: names the log and the images.
    registration_id: i64,
    session_id: i32,
    stream_id: i32,
    exclusive: bool,
    /// A replay's publication (the registration id is the replay session id): it
    /// counts against [`spec::MAX_REPLAY_PUBLICATIONS`] while it lives and while it
    /// lingers after the replay ended, since the replay's registry slot is free by then.
    replay: bool,
    log: LogBuffers,
    position: Counter,
    limit: Counter,
    registrations: Vec<Registration>,
    /// The largest position any subscriber reached, or the join position.
    consumer_position: i64,
    /// Up to where the log is zero behind the consumer position.
    clean_position: i64,
    /// The limit last stored.
    last_limit: i64,
    /// The connected flag as last stored to the log header: raised in the limit pass
    /// after the limit's raise, and lowered where the last subscriber, receiver or
    /// recording goes, before the limit pass lowers the limit (ADR 0048; card 187).
    connected_flag: bool,
    /// The producer position as last loaded, and when, by the conductor's clock: loaded
    /// at most once per [`spec::PRODUCER_POSITION_INTERVAL`] (ADR 0022, ADR 0032).
    producer: i64,
    producer_loaded_at: i64,
    /// The consumer position that has not moved, and since when.
    stalled: Option<(i64, i64)>,
    /// The end of stream position and when it was stored.
    closing: Option<(i64, i64)>,
    /// Whether `unavailable image` has gone to its images: once, when every subscriber
    /// has reached the end of stream or the linger has passed, which may be before the
    /// log is removed (ADR 0078).
    withdrawn: bool,
    log_file_name: String,
    /// What a `cavalry:udp` publication carries beside: its endpoint and what the
    /// sender has told the conductor (ADR 0041); `None` for `cavalry:ipc`.
    udp: Option<UdpPublication>,
}

/// A `cavalry:udp` publication as the conductor sees it: the sender puts its log on the
/// wire, and the conductor keeps its limit from the sender position, the consumer
/// position over UDP, and its connected flag from the sender's replies
/// (`spec/udp.md`, "The sender"; ADR 0041).
struct UdpPublication {
    /// The send endpoint's registration id.
    endpoint_id: i64,
    /// The position keys the publication was created with, which a join must name
    /// too (`spec/udp.md`, version 2, "The channel URI"; ADR 0059).
    position: Option<PublicationPosition>,
    /// The sender's `sender.position` counter, loaded on the duty cycle; freed with the
    /// publication's own two when the close completes.
    sender_position: Counter,
    /// The id of the sender's `sender.limit` counter, which the sender alone reads and
    /// moves; kept so that the conductor frees its record with the others.
    sender_limit_id: u32,
    /// The log's term length: the log is cleaned a term behind the sender position.
    term_length: i64,
    /// A status message arrived within the connection timeout, as the sender last said.
    connected: bool,
    /// The close was handed to the sender.
    close_sent: bool,
    /// The sender's linger ended and it dropped the publication.
    sender_closed: bool,
    /// The channel's `fc`, read from the command that created the publication: a join
    /// under another is refused (`spec/udp.md`, "The channel URI"; ADR 0053).
    flow_control: FlowControl,
    /// The id of the sender's `sender.flow_control_receivers` gauge, freed with the
    /// others.
    receivers_id: u32,
}

/// Where a send endpoint's socket sends (`spec/udp.md`, "The channel URI"; ADR 0053):
/// a channel's resolved endpoint and interface, connected; or a control channel's
/// resolved control address, which the socket binds and sends from to the endpoint's
/// destinations. Two channels are the same channel when these are the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SendTarget {
    /// The endpoint, or the control address.
    address: SocketAddr,
    /// The interface, when the channel names one; never with a control address.
    interface: Option<SocketAddr>,
    /// Whether `address` is a control address.
    control: bool,
}

impl SendTarget {
    /// The resolved channel, as the endpoint's counters are labelled with it.
    fn resolved(&self) -> String {
        match (self.control, self.interface) {
            (true, _) => format!("cavalry:udp?control={}|control-mode=manual", self.address),
            (false, Some(interface)) => {
                format!(
                    "cavalry:udp?endpoint={}|interface={interface}",
                    self.address
                )
            }
            (false, None) => format!("cavalry:udp?endpoint={}", self.address),
        }
    }
}

/// Where one send endpoint is in its life (`spec/udp.md`, "The receiver", the
/// endpoints): opened as the first `add publication` on its resolved channel is
/// handled, closed when its last publication's linger ends; every transition adds one
/// to a counter, and `counters.toml` lists them.
// state machine: channel
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChannelState {
    /// The registry slot is taken and the socket not yet open: a moment a client never
    /// observes.
    Initializing,
    /// The socket is open and with the sender; `channel.status` reads 1.
    Active,
    /// The last publication's linger ended: the socket is closing with the sender, and
    /// the counters are freed once it has; `channel.status` reads 2.
    Closing,
    /// Reserved for the change that re-resolves names while a stream lives, which ADR
    /// 0040 leaves to a later version and no card numbers yet (`spec/udp.md`, "The
    /// receiver", the endpoints): nothing in this version stores it, so a client never
    /// reads −1 from `channel.status`.
    #[allow(dead_code)]
    Errored,
}

/// One send endpoint the conductor opened: the resolved channel, the socket's owner's
/// state, its five counters and how many publications send on it.
struct SendEndpoint {
    /// The correlation id of the `add publication` that opened it: keys `channel.status`.
    registration_id: i64,
    target: SendTarget,
    state: ChannelState,
    status: Counter,
    socket_rcvbuf: Counter,
    socket_sndbuf: Counter,
    images: Counter,
    publications: Counter,
    /// `channel.destinations`: how many the endpoint sends to; 0 off a control channel.
    destinations: Counter,
    /// A control channel's destinations in the order added, the sender's copy kept in
    /// step through its requests; room for [`MAX_DESTINATIONS`] and a 65th is code 10
    /// (`spec/client-protocol.md`, "Sizes"; ADR 0053).
    destination_list: Vec<Destination>,
    publication_count: usize,
}

/// One destination of a control channel's endpoint: the endpoint as its add wrote it,
/// which an add compares byte for byte and a remove names, beside the address it
/// resolved to, which the sender sends to (`spec/udp.md`, "The sender"; ADR 0053).
struct Destination {
    host: String,
    port: u16,
    address: SocketAddr,
}

/// One subscription: a client's interest in a stream.
struct Subscription {
    registration_id: i64,
    client_id: i64,
    stream_id: i32,
    /// The receive endpoint a `cavalry:udp` subscription receives on; `None` for
    /// `cavalry:ipc`.
    endpoint: Option<i64>,
}

/// An `add subscription` on a `cavalry:udp` channel whose previous receive endpoint
/// was still closing, its socket with the receiver, held until the receiver's
/// `EndpointClosed` reply frees the slot and then run as if it had just arrived
/// (`spec/client-protocol.md`, "Commands"; ADR 0049; card 194), or answered with code 4
/// once it has waited [`spec::CLOSING_ENDPOINT_WAIT`]. It holds a slot of the
/// subscriptions registry while it waits.
struct PendingSubscription {
    client_id: i64,
    correlation_id: i64,
    stream_id: i32,
    /// The channel as the command carried it, so the add runs again as it arrived.
    channel: Vec<u8>,
    endpoint: SocketAddr,
    interface: Option<SocketAddr>,
    /// When the conductor read the add, on the driver's clock: the closing endpoint
    /// wait is measured from here.
    arrived: i64,
}

/// One receive endpoint the conductor bound: the resolved channel, the socket's
/// owner's state, its five counters, and how many subscriptions and images it carries
/// (`spec/udp.md`, "The receiver"; ADR 0042).
struct ReceiveEndpoint {
    /// The correlation id of the `add subscription` that bound it: keys `channel.status`.
    registration_id: i64,
    endpoint: SocketAddr,
    interface: Option<SocketAddr>,
    /// The resolved channel, as `unavailable image` names it.
    channel: String,
    state: ChannelState,
    status: Counter,
    socket_rcvbuf: Counter,
    socket_sndbuf: Counter,
    images: Counter,
    publications: Counter,
    subscription_count: usize,
    image_count: usize,
}

/// One image over UDP as the conductor sees it: the ids it will free, the subscriptions
/// that read it with their position counters' ids, and where it is in its life; the
/// receiver owns its log and its writer (ADR 0042).
struct UdpImage {
    registration_id: i64,
    endpoint_id: i64,
    session_id: i32,
    stream_id: i32,
    /// The `image.hwm_position` counter, kept to free its record.
    hwm: Counter,
    /// The `image.rebuild_position` counter, read for a subscription's join position.
    rebuild: Counter,
    log_file_name: String,
    source_identity: String,
    /// The subscriptions reading it, each with its `subscription.position` record,
    /// freed when the receiver says the reader went. On a retired image, the ones
    /// whose record waits for a client that may not have read the announcement yet
    /// (`waiting`).
    subscriptions: Vec<UdpPair>,
    state: ImageState,
    /// Its last subscription was removed: the receiver has been asked to drop the
    /// image's last reader and retires the image when it does (`spec/udp.md`, "The
    /// receiver", step 5). Until its reply the image is listed here as live, with a
    /// log that is unlinked when the reply comes, so it is given to no subscription
    /// and no recording that arrives meanwhile (ADR 0068).
    leaving: bool,
    /// Retired, with its log and the records in `subscriptions` kept for clients that
    /// may not have read their `available image` yet: `remove_retired_images` removes
    /// it once none of them waits (ADR 0078).
    waiting: bool,
}

/// One subscription's place on an image over UDP.
#[derive(Clone, Copy)]
struct UdpPair {
    subscription_id: i64,
    /// The id of its `subscription.position` record.
    record: u32,
    /// The position it joined at, stored in the record before the image was announced.
    joined: i64,
    /// When the driver sent its `available image`.
    announced: i64,
}

/// Whether what an `available image` named must still be kept: the log and the
/// position counter stay for the counter reuse timeout after the event, while the
/// position stands where the image joined. A position that has moved is the
/// subscriber's own store, made through the counter after reading the log, so its
/// client has both (`spec/log-buffer.md`, "Closing"; ADR 0078).
fn announcement_waits(announced: i64, joined: i64, position: i64, now: i64) -> bool {
    now.saturating_sub(announced) < spec::nanos(spec::COUNTER_REUSE_TIMEOUT) && position == joined
}

/// One image: a subscription's view of one publication, with the position counter the
/// client owns.
struct Image {
    publication_id: i64,
    subscription_id: i64,
    position: Counter,
    /// The position the image joined at, which the driver stored in `position` before
    /// it announced the image.
    joined: i64,
    /// When the driver sent its `available image`: the publication's log is not
    /// removed, and this counter not freed, while [`announcement_waits`] says so,
    /// unless the subscription is removed first (ADR 0078).
    announced: i64,
}

/// The driver's one recorder, seen from the conductor: the request queue it offers
/// starts, attaches and stops to, the reply queue it takes answers from, and the next
/// recording id, the catalog's slot count, which every reply refreshes (ADR 0025).
pub(crate) struct RecorderLink {
    /// Starts, attaches and stops to the recorder.
    pub requests: spsc::Producer<Request>,
    /// The recorder's answers.
    pub replies: spsc::Consumer<Reply>,
    /// The catalog's slot count: a `stop recording` naming an id at or past it is code 21.
    pub next_recording_id: i64,
}

/// Where one recording is in its life, as the conductor sees it, from the command to
/// the reply that ends it; every transition adds one to a counter, and `counters.toml`
/// lists them (`spec/client-protocol.md`, "Commands").
// state machine: recording
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecordingState {
    /// The start is with the recorder; the limit respects the start position.
    Starting,
    /// The slot and files exist, the counters are allocated, `recording ready` went out.
    Live,
    /// The recorder wrote the stop; `recording stopped` went out, the counters are freed.
    Stopped,
    /// The start failed, or the counters could not be allocated: the client got an error.
    /// A failed start whose slot the recorder holds stays registered, a floor at its
    /// start position, until the recorder's stop reply removes it.
    Failed,
}

/// The driver's one replayer, seen from the conductor: the queues to and from it
/// (ADR 0030).
pub(crate) struct ReplayerLink {
    /// Starts, attaches and stops to the replayer.
    pub requests: spsc::Producer<replayer::Request>,
    /// The replayer's answers.
    pub replies: spsc::Consumer<replayer::Reply>,
}

/// The driver's one sender, seen from the conductor: the queues to and from it (ADR
/// 0041).
pub(crate) struct SenderLink {
    /// Endpoints and publications to the sender, and closes.
    pub requests: spsc::Producer<sender::Request>,
    /// The sender's answers: connected, disconnected, closed.
    pub replies: spsc::Consumer<sender::Reply>,
}

/// The driver's one receiver, seen from the conductor: the queues to and from it (ADR
/// 0042).
pub(crate) struct ReceiverLink {
    /// Endpoints, interests, images and readers to the receiver.
    pub requests: spsc::Producer<receiver::Request>,
    /// The receiver's answers: a setup to create an image for, an image retired, a
    /// reader removed or rejected, an endpoint closed.
    pub replies: spsc::Consumer<receiver::Reply>,
}

/// Where one replay is in its life, as the conductor sees it, from the command to the
/// reply that ends it; every transition adds one to a counter, and `counters.toml`
/// lists them (`spec/client-protocol.md`, "Commands"; ADR 0030).
// state machine: replay
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReplayState {
    /// The start is with the replayer, which reads the slot and checks the position.
    Starting,
    /// The publication exists at the start position, `replay ready` went out, the
    /// replayer's writer is attached or on its way.
    Live,
    /// The replayer ended it; `replay stopped` went out, the publication is closing.
    Stopped,
    /// The start failed: the client got an error, and nothing of the replay is left.
    Failed,
}

/// One replay the conductor registers: its id, which is its publication's registration
/// id, the recording it plays, and, once live, its `replay.position` counter.
struct ReplayEntry {
    replay_session_id: i64,
    recording_id: i64,
    replay_stream_id: i32,
    state: ReplayState,
    position: Option<Counter>,
    /// Since when the replay's publication has had no image, or over UDP no receiver
    /// answering, for the connect timeout.
    unconnected_since: Option<i64>,
    /// A stop is with the replayer; a second `stop replay` is answered at once.
    stop_requested: bool,
    /// The resolved endpoint and interface of a `cavalry:udp` replay channel, kept from
    /// the command for the publication the replayer's reply creates (ADR 0043); `None`
    /// for `cavalry:ipc`.
    udp: Option<(SendTarget, FlowControl)>,
    /// Over `cavalry:udp`, whether a receiver answers the publication's setup frames,
    /// as the sender last said: the replay's subscriber for the connect timeout.
    connected: bool,
}

/// One recording the conductor registers: its publication, the command that started
/// it, its start position, and, once the recorder has given the id, the two counters.
/// A recording armed by `start recording` on a `cavalry:udp` stream with a live
/// subscription and no image yet, held until the stream's next image arrives on the
/// endpoint or its last subscription goes (`spec/client-protocol.md`, "Commands",
/// start recording; ADR 0059).
struct ArmedRecording {
    endpoint_id: i64,
    stream_id: i32,
    /// The arming command's correlation id, which the second answer names.
    correlation_id: i64,
}

struct RecordingEntry {
    publication_id: i64,
    session_id: i32,
    stream_id: i32,
    start_position: i64,
    state: RecordingState,
    recording_id: Option<i64>,
    position: Option<Counter>,
    synced: Option<Counter>,
    /// The `recording.bytes` gauge, the recorder's at each roll (ADR 0031).
    bytes: Option<Counter>,
    /// A stop is with the recorder; a second `stop recording` is answered at once.
    stop_requested: bool,
    /// Started by an armed recording when its image arrived: its `recording ready` is
    /// counted in `recording.armed_started`, and a failure the recorder or the counters
    /// file reports in `recording.armed_failed` (ADR 0059).
    armed: bool,
}

/// One retention command in progress: truncate recording, purge segments or purge
/// recording, from admission to the answer, through its hops on the replayer and the
/// recorder (`spec/client-protocol.md`, "Commands"; ADR 0031). While an entry names a
/// recording, the other two commands and `start replay` on it are code 24.
struct CommandEntry {
    correlation_id: i64,
    command: replayer::Retention,
    hop: Hop,
    /// A hop the request queue had no room for, offered again each duty cycle in
    /// admission order; the registry bounds what waits, and no hop is dropped or sent
    /// twice.
    pending: Option<PendingHop>,
    /// A file error waiting for the repair's reply before it is answered.
    failure: Option<String>,
}

/// Where a retention command stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hop {
    /// The replayer checks the slot, the position and the frame.
    Checking,
    /// The recorder makes the first catalog write.
    CatalogFirst,
    /// The replayer does the file work, one file per duty cycle.
    FileWork,
    /// The recorder writes a truncate's stop.
    CatalogLast,
    /// The replayer runs the file side of recovery's rules for the slot after a file
    /// error, where the files are open.
    RepairFiles,
    /// The recorder makes the one catalog write the repair ended in.
    RepairSlot,
}

/// A hop kept for the next cycle.
enum PendingHop {
    Recorder(Request),
    Replayer(replayer::Request),
}

/// The static counters the conductor moves.
struct Statics {
    client_timed_out: Counter,
    rejected_command: Counter,
    errors: Counter,
    registry_full: Counter,
    log_create_failed: Counter,
    publication_unblocked: Counter,
    command_ring_unblocked: Counter,
    command_ring_rejected_tail: Counter,
    command_ring_rejected_record: Counter,
    log_buffer_rejected_position: Counter,
    log_buffer_rejected_tail: Counter,
    recording_started: Counter,
    recording_stopped: Counter,
    recording_truncated: Counter,
    recording_deleted: Counter,
    recording_retention_failed: Counter,
    recording_start_failed: Counter,
    recording_catalog_full: Counter,
    recording_armed: Counter,
    recording_armed_started: Counter,
    recording_armed_failed: Counter,
    recording_armed_dropped: Counter,
    replay_started: Counter,
    replay_stopped: Counter,
    replay_ended: Counter,
    replay_not_connected: Counter,
    replay_failed: Counter,
    replay_start_failed: Counter,
    channel_opened: Counter,
    channel_closed: Counter,
    subscription_add_deferred: Counter,
    subscription_add_deferred_timed_out: Counter,
    subscription_add_deferred_waiting: Counter,
    image_removed: Counter,
}

impl Statics {
    fn new(counters: &CountersFile) -> Result<Self, CountersError> {
        let get = |id: CounterId| counters.counter(id.record());
        Ok(Self {
            client_timed_out: get(CLIENT_TIMED_OUT)?,
            rejected_command: get(CLIENT_PROTOCOL_REJECTED_COMMAND)?,
            errors: get(CLIENT_PROTOCOL_ERRORS)?,
            registry_full: get(DRIVER_REGISTRY_FULL)?,
            log_create_failed: get(DRIVER_LOG_CREATE_FAILED)?,
            publication_unblocked: get(PUBLICATION_UNBLOCKED)?,
            command_ring_unblocked: get(COMMAND_RING_UNBLOCKED)?,
            command_ring_rejected_tail: get(COMMAND_RING_REJECTED_TAIL)?,
            command_ring_rejected_record: get(COMMAND_RING_REJECTED_RECORD)?,
            log_buffer_rejected_position: get(LOG_BUFFER_REJECTED_POSITION)?,
            log_buffer_rejected_tail: get(LOG_BUFFER_REJECTED_TAIL)?,
            recording_started: get(RECORDING_STARTED)?,
            recording_stopped: get(RECORDING_STOPPED)?,
            recording_truncated: get(RECORDING_TRUNCATED)?,
            recording_deleted: get(RECORDING_DELETED)?,
            recording_retention_failed: get(RECORDING_RETENTION_FAILED)?,
            recording_armed: get(RECORDING_ARMED)?,
            recording_armed_started: get(RECORDING_ARMED_STARTED)?,
            recording_armed_failed: get(RECORDING_ARMED_FAILED)?,
            recording_armed_dropped: get(RECORDING_ARMED_DROPPED)?,
            recording_start_failed: get(RECORDING_START_FAILED)?,
            recording_catalog_full: get(RECORDING_CATALOG_FULL)?,
            replay_started: get(REPLAY_STARTED)?,
            replay_stopped: get(REPLAY_STOPPED)?,
            replay_ended: get(REPLAY_ENDED)?,
            replay_not_connected: get(REPLAY_NOT_CONNECTED)?,
            replay_failed: get(REPLAY_FAILED)?,
            replay_start_failed: get(REPLAY_START_FAILED)?,
            channel_opened: get(CHANNEL_OPENED)?,
            channel_closed: get(CHANNEL_CLOSED)?,
            subscription_add_deferred: get(SUBSCRIPTION_ADD_DEFERRED)?,
            subscription_add_deferred_timed_out: get(SUBSCRIPTION_ADD_DEFERRED_TIMED_OUT)?,
            subscription_add_deferred_waiting: get(SUBSCRIPTION_ADD_DEFERRED_WAITING)?,
            image_removed: get(IMAGE_REMOVED)?,
        })
    }
}

/// The longest command of this version, `start replay`: client id, correlation id,
/// recording id, position, length, replay stream id, and a channel of
/// [`MAX_STRING_LENGTH`] bytes with its length (`spec/client-protocol.md`, "Commands").
/// A record longer than this is no command, and is refused, by the spec's checks run on
/// its first this many bytes, without being copied past this.
const MAX_COMMAND_LENGTH: usize = 8 + 8 + 8 + 8 + 8 + 4 + 4 + MAX_STRING_LENGTH;

/// A command the ring delivered, copied out so that the ring's read can finish before
/// the command is handled. The buffer is sized once, at create, to
/// [`MAX_COMMAND_LENGTH`]: the command read allocates nothing (CLAUDE.md, invariant 2; the
/// harness's zero-allocation assertion covers the client path, keepalives included).
struct Pending {
    type_id: i32,
    /// The record's message length, which may exceed the buffer: an over-long record.
    length: usize,
    message: Vec<u8>,
}

impl Pending {
    fn new() -> Self {
        Self {
            type_id: 0,
            length: 0,
            message: vec![0u8; MAX_COMMAND_LENGTH],
        }
    }
}

/// The conductor's state: the files, the registries and the counters.
pub(crate) struct Conductor {
    dir: PathBuf,
    counters: CountersFile,
    consumer: Consumer,
    transmitter: Transmitter,
    statics: Statics,
    clients: Vec<Client>,
    /// The counters clients added, in the order they were added (ADR 0062).
    client_counters: Vec<ClientCounter>,
    publications: Vec<Publication>,
    subscriptions: Vec<Subscription>,
    /// Adds waiting for a closing receive endpoint, in arrival order (ADR 0049).
    pending_subscriptions: Vec<PendingSubscription>,
    images: Vec<Image>,
    recordings: Vec<RecordingEntry>,
    /// At most one per endpoint and stream a live subscription holds, so the
    /// subscriptions registry bounds them (ADR 0059).
    armed_recordings: Vec<ArmedRecording>,
    replays: Vec<ReplayEntry>,
    /// The retention commands in progress (`spec/client-protocol.md`, "Sizes").
    commands: Vec<CommandEntry>,
    /// The term length of every log this conductor creates: the spec's, and the
    /// smallest in a test at a registry's capacity (ADR 0027).
    term_length: u32,
    recorder: RecorderLink,
    replayer: ReplayerLink,
    /// The replayer's thread, once the driver has spawned it: unparked with every
    /// request offered, since a replayer that holds no replay parks until one arrives
    /// (`replayer::Replayer::run`). `None` in the tests that drive the duty cycles by
    /// hand.
    replayer_thread: Option<std::thread::Thread>,
    /// The replayer's attaches, stops and recording stops its request queue had no
    /// room for, kept and bounded as `deferred` is; while it holds any, `start replay`
    /// is code 10.
    replayer_deferred: VecDeque<replayer::Request>,
    /// Attaches and stops the request queue had no room for, offered again each cycle
    /// in order, before anything newer. While it holds any, `start recording` is
    /// answered with code 10, so no recording is registered until it empties; every
    /// request it takes meanwhile is one attach or one stop of a recording already
    /// registered, at most [`spec::MAX_RECORDINGS`], so it never holds more than two per
    /// recording, its capacity.
    deferred: VecDeque<Request>,
    /// The sender's queues (ADR 0041).
    sender: SenderLink,
    /// The sender's closes and endpoint removes its request queue had no room for, kept
    /// and bounded as `deferred` is, at [`SENDER_DEFERRED_CAPACITY`]; while it holds
    /// any, `add publication` on a `cavalry:udp` channel is code 10.
    sender_deferred: VecDeque<sender::Request>,
    /// The send endpoints registry (`spec/client-protocol.md`, "Sizes").
    send_endpoints: Vec<SendEndpoint>,
    /// The sender's thread, once the driver has spawned it: unparked with every request
    /// offered, since a sender that holds no endpoint and no publication parks until one
    /// arrives (`sender::Sender::run`). `None` in the tests that drive the duty cycles by
    /// hand.
    sender_thread: Option<std::thread::Thread>,
    /// The receiver's queues (ADR 0042).
    receiver: ReceiverLink,
    /// The receiver's requests its queue had no room for, kept and bounded as
    /// `sender_deferred` is, at [`RECEIVER_DEFERRED_CAPACITY`]; while it holds any,
    /// `add subscription` on a `cavalry:udp` channel is code 10.
    receiver_deferred: VecDeque<receiver::Request>,
    /// The receive endpoints registry (`spec/client-protocol.md`, "Sizes").
    receive_endpoints: Vec<ReceiveEndpoint>,
    /// The images over UDP, one per session on an endpoint and stream.
    udp_images: Vec<UdpImage>,
    /// How many subscription and image pairs the images over UDP hold: they count
    /// against the images registry with the IPC pairs in `images`.
    udp_pairs: usize,
    /// How many retired images over UDP are `waiting`, so that a duty cycle with none
    /// does not look for one.
    udp_waiting: usize,
    /// The receiver's thread, once the driver has spawned it, unparked with every
    /// request offered as the sender's is. `None` in the tests that drive the duty
    /// cycles by hand.
    receiver_thread: Option<std::thread::Thread>,
    /// A producer on the command ring, for the registration id an image over UDP takes
    /// from the correlation id counter (`spec/client-protocol.md`, "Commands"; ADR 0040).
    correlation: RingProducer,
    /// Registrations across every publication: the publications registry's fill.
    registration_count: usize,
    /// The session id the next publication takes: random at the start
    /// ([`first_session_id`]), one more for each publication.
    next_session_id: i32,
    last_heartbeat: i64,
    ring_corrupt: bool,
    pending: Vec<Pending>,
    /// How many of `pending` the last read filled.
    pending_count: usize,
    scratch: Vec<u8>,
    idler: Idler,
    // The ring and the buffer outlive their consumer and transmitter; kept so that the
    // mappings stay the driver's.
    _ring: CommandRing,
    _broadcast: BroadcastBuffer,
}

impl Conductor {
    /// Creates the counters file, the command ring and the broadcast buffer in `dir`,
    /// in that order, with the sizes of [`spec`], records `idle` in the
    /// `conductor.idle_strategy` counter for `doctor`, and keeps `recorder` and
    /// `replayer`, the queues to the recorder and replayer threads.
    pub(crate) fn create(
        dir: &Path,
        idle: IdleStrategy,
        recorder: RecorderLink,
        replayer: ReplayerLink,
        sender: SenderLink,
        receiver: ReceiverLink,
    ) -> Result<Self, DriverError> {
        let counters = CountersFile::create(dir, spec::COUNTERS_CAPACITY)?;
        let statics = Statics::new(&counters)?;
        counters
            .counter(CONDUCTOR_IDLE_STRATEGY.record())?
            .set(idle.counter_value());
        let ring = CommandRing::create(dir, spec::COMMAND_RING_CAPACITY)?;
        let consumer = ring.consumer(spec::COMMAND_RING_UNBLOCK_TIMEOUT)?;
        let correlation = ring.producer(spec::DRIVER_TIMEOUT)?;
        let broadcast = BroadcastBuffer::create(dir, spec::BROADCAST_CAPACITY)?;
        let transmitter = broadcast.transmitter()?;
        let now = now_nanos();
        let scratch_length = transmitter
            .max_message_length()
            .min(8 + 2 * (4 + MAX_STRING_LENGTH) + 64);
        Ok(Self {
            dir: dir.to_path_buf(),
            counters,
            consumer,
            transmitter,
            statics,
            clients: Vec::with_capacity(spec::MAX_CLIENTS),
            client_counters: Vec::with_capacity(spec::MAX_CLIENT_COUNTERS),
            // A replay's publication has no registration, so the publications sit
            // beside the registrations' bound.
            publications: Vec::with_capacity(
                spec::MAX_PUBLICATIONS + spec::MAX_REPLAY_PUBLICATIONS,
            ),
            subscriptions: Vec::with_capacity(spec::MAX_SUBSCRIPTIONS),
            pending_subscriptions: Vec::with_capacity(spec::MAX_SUBSCRIPTIONS),
            images: Vec::with_capacity(spec::MAX_IMAGES),
            recordings: Vec::with_capacity(spec::MAX_RECORDINGS),
            armed_recordings: Vec::with_capacity(spec::MAX_SUBSCRIPTIONS),
            replays: Vec::with_capacity(spec::MAX_REPLAYS),
            commands: Vec::with_capacity(spec::MAX_COMMANDS_IN_PROGRESS),
            recorder,
            replayer,
            replayer_thread: None,
            replayer_deferred: VecDeque::with_capacity(REPLAYER_DEFERRED_CAPACITY),
            term_length: spec::IPC_TERM_LENGTH,
            deferred: VecDeque::with_capacity(DEFERRED_CAPACITY),
            sender,
            sender_deferred: VecDeque::with_capacity(SENDER_DEFERRED_CAPACITY),
            send_endpoints: Vec::with_capacity(spec::MAX_SEND_ENDPOINTS),
            sender_thread: None,
            receiver,
            receiver_deferred: VecDeque::with_capacity(RECEIVER_DEFERRED_CAPACITY),
            receive_endpoints: Vec::with_capacity(spec::MAX_RECEIVE_ENDPOINTS),
            udp_images: Vec::with_capacity(spec::MAX_IMAGES),
            udp_pairs: 0,
            udp_waiting: 0,
            receiver_thread: None,
            correlation,
            registration_count: 0,
            next_session_id: first_session_id(now),
            last_heartbeat: now,
            ring_corrupt: false,
            pending: (0..COMMANDS_PER_CYCLE).map(|_| Pending::new()).collect(),
            pending_count: 0,
            scratch: vec![0u8; scratch_length],
            idler: Idler::new(idle),
            _ring: ring,
            _broadcast: broadcast,
        })
    }

    /// A handle on a static counter, for the recorder's two.
    pub(crate) fn counter(&self, id: CounterId) -> Result<Counter, CountersError> {
        self.counters.counter(id.record())
    }

    /// Names the replayer's thread, which `wake_replayer` unparks: a replayer that
    /// holds no replay parks until a request arrives (`replayer::Replayer::run`), so
    /// the driver sets this before the conductor's loop starts.
    pub(crate) fn set_replayer_thread(&mut self, thread: std::thread::Thread) {
        self.replayer_thread = Some(thread);
    }

    /// Unparks the replayer after a request went into its queue: an atomic swap, and a
    /// futex wake only when the thread is parked. On the command path, never per
    /// message.
    fn wake_replayer(&self) {
        if let Some(thread) = &self.replayer_thread {
            thread.unpark();
        }
    }

    /// Keeps the sender's thread, so that a request offered to a sender parked with
    /// nothing to send wakes it (ADR 0041).
    pub(crate) fn set_sender_thread(&mut self, thread: std::thread::Thread) {
        self.sender_thread = Some(thread);
    }

    /// Unparks the sender after a request went into its queue, as [`Self::wake_replayer`]
    /// does for the replayer: on the command path, never per message.
    fn wake_sender(&self) {
        if let Some(thread) = &self.sender_thread {
            thread.unpark();
        }
    }

    /// Keeps the receiver's thread, so that a request offered to a receiver parked with
    /// no endpoint wakes it (ADR 0042).
    pub(crate) fn set_receiver_thread(&mut self, thread: std::thread::Thread) {
        self.receiver_thread = Some(thread);
    }

    /// Unparks the receiver after a request went into its queue, as [`Self::wake_sender`]
    /// does for the sender.
    fn wake_receiver(&self) {
        if let Some(thread) = &self.receiver_thread {
            thread.unpark();
        }
    }

    /// Stores what the recovery pass did when the log directory was opened in the six
    /// `recording.*` recovery counters, so that `doctor` reads it from the counters file
    /// (`spec/durable-log.md`, "The driver"; invariant 4).
    pub(crate) fn record_recovery(&self, recovery: &Recovery) -> Result<(), CountersError> {
        let set = |id: CounterId, value: u64| -> Result<(), CountersError> {
            self.counters
                .counter(id.record())?
                .set(i64::try_from(value).unwrap_or(i64::MAX));
            Ok(())
        };
        set(RECORDING_RECOVERED, recovery.recovered)?;
        set(RECORDING_INVALIDATED, recovery.invalidated)?;
        set(RECORDING_INDEX_REWRITTEN, recovery.index_rewritten)?;
        set(RECORDING_CATALOG_TRUNCATED, u64::from(recovery.truncated))?;
        set(
            RECORDING_SLOT_DROPPED,
            u64::from(recovery.dropped_empty_slot),
        )?;
        set(RECORDING_FILES_REMOVED, recovery.files_removed)
    }

    /// The conductor's loop: duty cycles until `stop`, then −1 in the heartbeat.
    pub(crate) fn run(mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Acquire) {
            let now = now_nanos();
            if self.duty_cycle(now) == 0 {
                self.idler.idle();
            } else {
                self.idler.reset();
            }
        }
        self.consumer.close();
    }

    /// One duty cycle; returns how much work it found, 0 when the conductor may idle.
    fn duty_cycle(&mut self, now: i64) -> usize {
        let mut work = 0;
        if now.saturating_sub(self.last_heartbeat) >= spec::nanos(spec::HEARTBEAT_INTERVAL) {
            self.consumer.store_heartbeat(now);
            self.last_heartbeat = now;
        }
        work += self.read_commands(now);
        work += self.unblock_ring(now);
        work += self.time_out_clients(now);
        work += self.take_replies(now);
        work += self.flush_deferred();
        work += self.take_replayer_replies(now);
        work += self.flush_replayer_deferred();
        // The sender answers only for endpoints this registry holds, from the add that
        // put the entry there to the closed reply that removes it (no request goes to the
        // sender for an endpoint `abandon_send_endpoint` undoes), so while it holds none
        // the reply queue is empty and the poll's load of the sender's tail is skipped: a
        // driver that sends nothing pays nothing per cycle for the sender's queue
        // (invariant 2); card 182's rows are the first that send.
        if !self.send_endpoints.is_empty() {
            work += self.take_sender_replies(now);
        }
        work += self.flush_sender_deferred();
        // The receiver the same way: it answers only for endpoints this registry holds.
        if !self.receive_endpoints.is_empty() {
            work += self.take_receiver_replies(now);
        }
        // After the replies, so that a close that came this cycle runs its adds first;
        // an add waits only while the list holds it (ADR 0049).
        if !self.pending_subscriptions.is_empty() {
            work += self.time_out_pending_subscriptions(now);
        }
        work += self.flush_receiver_deferred();
        if !self.udp_images.is_empty() {
            work += self.stop_recordings_of_retired_images();
            if self.udp_waiting > 0 {
                work += self.remove_retired_images(now);
            }
        }
        work += self.flush_commands();
        work += self.keep_replays(now);
        work += self.keep_publications(now);
        work
    }

    /// Reads up to [`COMMANDS_PER_CYCLE`] commands and handles each.
    fn read_commands(&mut self, now: i64) -> usize {
        if self.ring_corrupt {
            return 0;
        }
        let pending = &mut self.pending;
        let mut count = 0;
        let read = self.consumer.read(COMMANDS_PER_CYCLE, |type_id, message| {
            // The consumer delivers at most COMMANDS_PER_CYCLE records, one per slot.
            if let Some(slot) = pending.get_mut(count) {
                let copied = message.len().min(MAX_COMMAND_LENGTH);
                slot.type_id = type_id;
                slot.length = message.len();
                slot.message[..copied].copy_from_slice(&message[..copied]);
                count += 1;
            }
        });
        self.pending_count = count;
        match read {
            Ok(_) => {}
            Err(CommandRingError::Record { .. }) => {
                self.statics.command_ring_rejected_record.increment();
                self.ring_corrupt = true;
            }
            Err(CommandRingError::Tail { .. }) => {
                self.statics.command_ring_rejected_tail.increment();
                self.ring_corrupt = true;
            }
            Err(_) => self.ring_corrupt = true,
        }
        let commands = std::mem::take(&mut self.pending);
        let count = self.pending_count;
        for command in &commands[..count] {
            if command.length > MAX_COMMAND_LENGTH {
                let error = over_long(command.type_id, &command.message, command.length);
                self.refuse(error, &command.message);
            } else {
                self.handle(command.type_id, &command.message[..command.length], now);
            }
        }
        self.pending = commands;
        count
    }

    /// The ring's unblock: a stale claim or a gap becomes padding after the timeout.
    fn unblock_ring(&mut self, now: i64) -> usize {
        if self.ring_corrupt {
            return 0;
        }
        match self.consumer.unblock(now) {
            Ok(RingUnblock::NoAction) => 0,
            Ok(RingUnblock::Unblocked { .. }) => {
                self.statics.command_ring_unblocked.increment();
                1
            }
            Err(CommandRingError::Record { .. }) => {
                self.statics.command_ring_rejected_record.increment();
                self.ring_corrupt = true;
                1
            }
            Err(CommandRingError::Tail { .. }) => {
                self.statics.command_ring_rejected_tail.increment();
                self.ring_corrupt = true;
                1
            }
            Err(_) => {
                self.ring_corrupt = true;
                1
            }
        }
    }

    /// Decodes one command and acts on it, or answers with the error the spec names.
    fn handle(&mut self, type_id: i32, message: &[u8], now: i64) {
        let command = match Command::parse(type_id, message) {
            Ok(command) => command,
            Err(error) => {
                self.refuse(error, message);
                return;
            }
        };
        let correlation_id = command.correlation_id();
        if !self.touch_client(command.client_id(), now) {
            self.statics.registry_full.increment();
            self.send_error(
                correlation_id,
                ErrorCode::ResourceTemporarilyUnavailable,
                &format!(
                    "the driver's client registry holds {} clients and is full; close a client \
                     that is not used; see {ERRORS_DOC}",
                    spec::MAX_CLIENTS
                ),
            );
            return;
        }
        match command {
            Command::AddPublication {
                client_id,
                correlation_id,
                stream_id,
                channel,
            } => self.add_publication(client_id, correlation_id, stream_id, channel, false, now),
            Command::AddExclusivePublication {
                client_id,
                correlation_id,
                stream_id,
                channel,
            } => self.add_publication(client_id, correlation_id, stream_id, channel, true, now),
            Command::RemovePublication {
                client_id,
                correlation_id,
                registration_id,
            } => self.remove_publication(client_id, correlation_id, registration_id, now),
            Command::AddSubscription {
                client_id,
                correlation_id,
                stream_id,
                channel,
            } => self.add_subscription(client_id, correlation_id, stream_id, channel, now),
            Command::RemoveSubscription {
                client_id,
                correlation_id,
                registration_id,
            } => self.remove_subscription(client_id, correlation_id, registration_id, now),
            Command::ClientKeepalive { .. } => {}
            Command::ClientClose { client_id, .. } => self.remove_client(client_id, now),
            Command::StartRecording {
                correlation_id,
                stream_id,
                channel,
                ..
            } => self.start_recording(correlation_id, stream_id, channel, now),
            Command::StopRecording {
                correlation_id,
                recording_id,
                ..
            } => self.stop_recording(correlation_id, recording_id),
            Command::SyncRecording {
                correlation_id,
                recording_id,
                ..
            } => self.sync_recording(correlation_id, recording_id),
            Command::StartReplay {
                correlation_id,
                recording_id,
                position,
                length,
                replay_stream_id,
                replay_channel,
                ..
            } => self.start_replay(
                correlation_id,
                recording_id,
                position,
                length,
                replay_stream_id,
                replay_channel,
            ),
            Command::StopReplay {
                correlation_id,
                replay_session_id,
                ..
            } => self.stop_replay(correlation_id, replay_session_id),
            Command::TruncateRecording {
                correlation_id,
                recording_id,
                position,
                ..
            } => self.retention(
                correlation_id,
                replayer::Retention::Truncate {
                    recording_id,
                    position,
                },
            ),
            Command::PurgeSegments {
                correlation_id,
                recording_id,
                new_start_position,
                ..
            } => self.retention(
                correlation_id,
                replayer::Retention::Purge {
                    recording_id,
                    position: new_start_position,
                },
            ),
            Command::PurgeRecording {
                correlation_id,
                recording_id,
                ..
            } => self.retention(correlation_id, replayer::Retention::Delete { recording_id }),
            Command::AddDestination {
                client_id,
                correlation_id,
                registration_id,
                channel,
            } => self.change_destination(client_id, correlation_id, registration_id, channel, true),
            Command::RemoveDestination {
                client_id,
                correlation_id,
                registration_id,
                channel,
            } => {
                self.change_destination(client_id, correlation_id, registration_id, channel, false);
            }
            Command::AddCounter {
                client_id,
                correlation_id,
                type_id,
                key,
                label,
            } => self.add_counter(client_id, correlation_id, type_id, key, label),
            Command::RemoveCounter {
                client_id,
                correlation_id,
                registration_id,
            } => self.remove_counter(client_id, correlation_id, registration_id, now),
        }
    }

    /// `add counter` (`spec/client-protocol.md`, "Commands"; ADR 0062): a counter of a
    /// type `counters.toml` gives the scope `client`, registered under the command's
    /// correlation id, allocated with the key and the label the client gave and answered
    /// with `counter ready`. The client owns it and alone writes its value; the driver
    /// compares it with none it holds. A type id that is not a client's is code 11 and
    /// counts as a rejected command; the registry or the counters file at capacity is
    /// code 10. The decoding has bounded the key and the label and checked the label's
    /// UTF-8, so the checks here are answered and never met.
    fn add_counter(
        &mut self,
        client_id: i64,
        correlation_id: i64,
        type_id: i32,
        key: &[u8],
        label: &[u8],
    ) {
        let is_a_clients = COUNTERS
            .iter()
            .any(|def| def.id.0 == type_id && def.scope == Scope::Client);
        if !is_a_clients {
            self.statics.rejected_command.increment();
            self.send_error(
                correlation_id,
                ErrorCode::GenericError,
                &format!(
                    "counter type {type_id} is not one a client adds: add a counter whose \
                     scope is client in counters.toml; see {COUNTERS_DOC}"
                ),
            );
            return;
        }
        if self.client_counters.len() >= spec::MAX_CLIENT_COUNTERS {
            self.statics.registry_full.increment();
            self.send_error(
                correlation_id,
                ErrorCode::ResourceTemporarilyUnavailable,
                &registry_full_message("client counters", spec::MAX_CLIENT_COUNTERS),
            );
            return;
        }
        let Ok(label) = core::str::from_utf8(label) else {
            self.refuse_counter_fields(correlation_id, key.len(), label.len());
            return;
        };
        match self.counters.allocate(CounterId(type_id), key, label) {
            Ok(counter) => {
                let counter_id = counter.id();
                self.client_counters.push(ClientCounter {
                    client_id,
                    registration_id: correlation_id,
                    counter_id,
                });
                self.send(&Event::CounterReady {
                    correlation_id,
                    counter_id: i32::try_from(counter_id).unwrap_or(i32::MAX),
                });
            }
            Err(error @ CountersError::Full { .. }) => self.counters_full(correlation_id, &error),
            Err(_) => self.refuse_counter_fields(correlation_id, key.len(), label.len()),
        }
    }

    /// Answers an `add counter` whose key or label the counters file refused with code
    /// 7, as the decoding's `counter` check does: the decoding refuses both first, so
    /// this is answered and never met.
    fn refuse_counter_fields(&mut self, correlation_id: i64, key: usize, label: usize) {
        self.statics.rejected_command.increment();
        let error = ProtocolError::Counter { key, label };
        self.send_error(
            correlation_id,
            ErrorCode::MalformedCommand,
            &format!("{error}"),
        );
    }

    /// `remove counter` (`spec/client-protocol.md`, "Commands"; ADR 0062): the driver
    /// forgets the client's registration, answers `operation success`, sends
    /// `unavailable counter` and frees the record. A registration the driver does not
    /// know, or another client's, is code 5.
    fn remove_counter(
        &mut self,
        client_id: i64,
        correlation_id: i64,
        registration_id: i64,
        now: i64,
    ) {
        let found = self.client_counters.iter().position(|counter| {
            counter.registration_id == registration_id && counter.client_id == client_id
        });
        let Some(index) = found else {
            self.send_error(
                correlation_id,
                ErrorCode::UnknownCounter,
                &format!(
                    "registration {registration_id} is not a counter of client {client_id}; \
                     see {ERRORS_DOC}"
                ),
            );
            return;
        };
        let counter = self.client_counters.remove(index);
        self.send(&Event::OperationSuccess { correlation_id });
        self.release_client_counter(&counter, now);
    }

    /// Tells every client that a client's counter is going, then frees its record, which
    /// is not allocated again before the counter reuse timeout: a reader of the counter
    /// stops at the event (`spec/client-protocol.md`, "Events").
    fn release_client_counter(&mut self, counter: &ClientCounter, now: i64) {
        self.send(&Event::UnavailableCounter {
            registration_id: counter.registration_id,
            counter_id: i32::try_from(counter.counter_id).unwrap_or(i32::MAX),
        });
        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
        let _ = self.counters.free(counter.counter_id, reuse);
    }

    /// `add destination` and `remove destination` (`spec/client-protocol.md`,
    /// "Commands"; ADR 0053): the client's registration of a publication on a control
    /// channel and a destination channel of which only `endpoint` is read. An add
    /// resolves the endpoint to the first address of the control address's family, a
    /// loopback one beside a loopback control address, puts it in the endpoint's table
    /// beside the endpoint as written and hands the address to the sender; one whose
    /// endpoint as written is there already is added by nothing; a 65th is code 10. A
    /// remove names the endpoint as written, resolves nothing, and takes it out; one
    /// not there is removed by nothing. Each is answered with `operation success`. A
    /// registration the driver does not know or another client's is code 3; a
    /// publication whose channel names no control address is code 1 with the reason
    /// `control`; a destination channel that is `cavalry:ipc` or a control channel, one
    /// that names `interface` or `fc`, or one whose endpoint has no address the control
    /// address can send to, is code 1 with the reason `destination`; a host that
    /// resolves to no address at all is code 9.
    fn change_destination(
        &mut self,
        client_id: i64,
        correlation_id: i64,
        registration_id: i64,
        channel: &[u8],
        add: bool,
    ) {
        let what = if add {
            "add destination"
        } else {
            "remove destination"
        };
        let Some(index) = self.publications.iter().position(|publication| {
            publication.registrations.iter().any(|registration| {
                registration.correlation_id == registration_id
                    && registration.client_id == client_id
            })
        }) else {
            self.send_error(
                correlation_id,
                ErrorCode::UnknownPublication,
                &format!(
                    "registration {registration_id} is not a publication of client \
                     {client_id}; see {ERRORS_DOC}"
                ),
            );
            return;
        };
        let endpoint_id = self.publications[index]
            .udp
            .as_ref()
            .map(|udp| udp.endpoint_id);
        let endpoint_index = endpoint_id.and_then(|endpoint_id| {
            self.send_endpoints
                .iter()
                .position(|entry| entry.registration_id == endpoint_id && entry.target.control)
        });
        let Some(endpoint_index) = endpoint_index else {
            self.statics.rejected_command.increment();
            self.send_error(
                correlation_id,
                ErrorCode::InvalidChannel,
                &format!(
                    "the channel: channel refused (control): publication {registration_id}'s \
                     channel names no control address, so {what} has nowhere to put one; add \
                     control=<host>:<port>|control-mode=manual to the publication's channel \
                     in place of its endpoint; see {ERRORS_DOC}"
                ),
            );
            return;
        };
        let Some(parsed) = self.check_channel(correlation_id, channel, "the destination channel")
        else {
            return;
        };
        // A destination names its endpoint and nothing else: `interface`, `fc` or a
        // position key on it is refused, as a subscription's channel refuses a
        // publication's keys, since a key that does nothing is a key the reader cannot
        // check (ADR 0053, ADR 0059).
        let destination = match parsed {
            Channel::Udp(udp)
                if !udp.is_control()
                    && udp.interface().is_none()
                    && !udp.names_flow_control()
                    && !udp.names_position() =>
            {
                udp.endpoint()
            }
            _ => None,
        };
        let Some(destination) = destination else {
            self.statics.rejected_command.increment();
            self.send_error(
                correlation_id,
                ErrorCode::InvalidChannel,
                &format!(
                    "the destination channel: channel refused (destination): a destination is \
                     cavalry:udp?endpoint=<host>:<port> and names nothing else; remove any \
                     other parameter, or name an endpoint; see {ERRORS_DOC}"
                ),
            );
            return;
        };
        let control = self.send_endpoints[endpoint_index].target.address;
        let host = destination.host();
        let port = destination.port().unwrap_or(0);
        // The endpoint as written keys the table: an add compares it byte for byte, a
        // remove names it and resolves nothing (ADR 0053).
        let held = self.send_endpoints[endpoint_index]
            .destination_list
            .iter()
            .position(|known| known.host == host && known.port == port);
        let request = match (add, held) {
            (true, Some(_)) | (false, None) => None,
            (true, None) => {
                // Resolved once, to the first address of the control address's family,
                // a loopback one beside a loopback control address: the socket can send
                // to no other, so one with none is refused here, as `connect` refuses
                // an endpoint of the other family on a channel with an endpoint.
                let address = match resolve_in_family(host, port, control) {
                    Ok(address) => address,
                    Err(Unreachable::NoAddress(reason)) => {
                        self.send_error(
                            correlation_id,
                            ErrorCode::UnknownHost,
                            &format!(
                                "the destination's host `{host}` does not resolve: {reason}; \
                                 check the name, or use an address; see {ERRORS_DOC}"
                            ),
                        );
                        return;
                    }
                    Err(Unreachable::Family(reason)) => {
                        self.statics.rejected_command.increment();
                        self.send_error(
                            correlation_id,
                            ErrorCode::InvalidChannel,
                            &format!(
                                "the destination channel: channel refused (destination): the \
                                 control socket bound to {control} cannot send to {host}:{port}: \
                                 {reason}; name a destination the control address can send \
                                 to, or a control address of the destination's kind; see \
                                 {ERRORS_DOC}"
                            ),
                        );
                        return;
                    }
                };
                if self.send_endpoints[endpoint_index].destination_list.len() >= MAX_DESTINATIONS {
                    self.statics.registry_full.increment();
                    self.send_error(
                        correlation_id,
                        ErrorCode::ResourceTemporarilyUnavailable,
                        &registry_full_message("destinations", MAX_DESTINATIONS),
                    );
                    return;
                }
                if !self.sender_has_room() {
                    self.sender_queue_full(correlation_id);
                    return;
                }
                let entry = &mut self.send_endpoints[endpoint_index];
                entry.destination_list.push(Destination {
                    host: host.to_string(),
                    port,
                    address,
                });
                entry
                    .destinations
                    .set(i64::try_from(entry.destination_list.len()).unwrap_or(i64::MAX));
                Some(sender::Request::AddDestination {
                    registration_id: entry.registration_id,
                    address,
                })
            }
            (false, Some(position)) => {
                if !self.sender_has_room() {
                    self.sender_queue_full(correlation_id);
                    return;
                }
                let entry = &mut self.send_endpoints[endpoint_index];
                let removed = entry.destination_list.remove(position);
                entry
                    .destinations
                    .set(i64::try_from(entry.destination_list.len()).unwrap_or(i64::MAX));
                Some(sender::Request::RemoveDestination {
                    registration_id: entry.registration_id,
                    address: removed.address,
                })
            }
        };
        if let Some(request) = request {
            self.sender_request(request);
        }
        self.send(&Event::OperationSuccess { correlation_id });
    }

    /// Records that `client_id` sent a command now; registers a new client, or returns
    /// false when the client registry is full.
    fn touch_client(&mut self, client_id: i64, now: i64) -> bool {
        if let Some(client) = self
            .clients
            .iter_mut()
            .find(|client| client.id == client_id)
        {
            client.last_seen = now;
            return true;
        }
        if self.clients.len() >= spec::MAX_CLIENTS {
            return false;
        }
        self.clients.push(Client {
            id: client_id,
            last_seen: now,
        });
        true
    }

    fn add_publication(
        &mut self,
        client_id: i64,
        correlation_id: i64,
        stream_id: i32,
        channel: &[u8],
        exclusive: bool,
        now: i64,
    ) {
        let Some(channel) = self.check_channel(correlation_id, channel, "the channel") else {
            return;
        };
        if let Channel::Udp(udp) = channel {
            self.add_udp_publication(client_id, correlation_id, stream_id, udp, exclusive, now);
            return;
        }
        if self.registration_count >= spec::MAX_PUBLICATIONS {
            self.registry_full(correlation_id, "publications", spec::MAX_PUBLICATIONS);
            return;
        }
        let registration = Registration {
            client_id,
            correlation_id,
        };
        let live = if exclusive {
            None
        } else {
            self.publications.iter().position(|publication| {
                !publication.exclusive
                    && publication.stream_id == stream_id
                    && publication.closing.is_none()
                    && publication.udp.is_none()
            })
        };
        if let Some(index) = live {
            let publication = &mut self.publications[index];
            publication.registrations.push(registration);
            self.registration_count += 1;
            let (registration_id, session_id, limit_id) = (
                publication.registration_id,
                publication.session_id,
                counter_id(&publication.limit),
            );
            let log_file_name = publication.log_file_name.clone();
            let ready = Event::PublicationReady {
                correlation_id,
                registration_id,
                session_id,
                stream_id,
                publication_limit_counter_id: limit_id,
                channel_status_indicator_id: NO_CHANNEL_STATUS,
                log_file_name: log_file_name.as_bytes(),
            };
            self.send(&ready);
            return;
        }
        let session_id = self.advance_session_id(stream_id);
        let initial_term_id =
            i32::from_le_bytes((now >> 16).to_le_bytes()[..4].try_into().unwrap_or([0; 4]));
        let log_type = if exclusive {
            LogType::Exclusive
        } else {
            LogType::Concurrent
        };
        // The spec's term and window (`IPC_PUBLICATION_WINDOW` is half the term, checked
        // below); a test at the registry's capacity shrinks the term, since every log is
        // made resident when it is created (ADR 0027).
        const _: () = assert!(spec::IPC_PUBLICATION_WINDOW == spec::IPC_TERM_LENGTH / 2);
        let fields = match HeaderFields::new(
            log_type,
            correlation_id,
            initial_term_id,
            self.term_length,
            spec::IPC_MTU_LENGTH,
            self.term_length / 2,
            session_id,
            stream_id,
        ) {
            Ok(fields) => fields,
            Err(error) => {
                self.statics.log_create_failed.increment();
                self.send_error(correlation_id, ErrorCode::StorageSpace, &format!("{error}"));
                return;
            }
        };
        let log = match LogBuffers::create(&self.dir, &fields) {
            Ok(log) => log,
            Err(error) => {
                self.statics.log_create_failed.increment();
                self.send_error(correlation_id, ErrorCode::StorageSpace, &format!("{error}"));
                return;
            }
        };
        let key = position_key(correlation_id, session_id, stream_id);
        let label = |name: &str| format!("{name} {correlation_id} {session_id} {stream_id}");
        let position =
            match self
                .counters
                .allocate(PUBLICATION_POSITION, &key, &label("publication.position"))
            {
                Ok(counter) => counter,
                Err(error) => {
                    drop(log);
                    remove_log(&self.dir, correlation_id);
                    self.counters_full(correlation_id, &error);
                    return;
                }
            };
        let limit =
            match self
                .counters
                .allocate(PUBLICATION_LIMIT, &key, &label("publication.limit"))
            {
                Ok(counter) => counter,
                Err(error) => {
                    let _ = self.counters.free(
                        position.id(),
                        now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT)),
                    );
                    drop(log);
                    remove_log(&self.dir, correlation_id);
                    self.counters_full(correlation_id, &error);
                    return;
                }
            };
        let join_position = log.producer_position().unwrap_or(0);
        position.store_release(join_position);
        limit.store_release(join_position);
        let log_file_name = log_layout::file_name(correlation_id)
            .to_string_lossy()
            .into_owned();
        let publication = Publication {
            registration_id: correlation_id,
            session_id,
            stream_id,
            exclusive,
            replay: false,
            log,
            position,
            limit,
            registrations: vec![registration],
            consumer_position: join_position,
            clean_position: join_position,
            last_limit: join_position,
            connected_flag: false,
            producer: join_position,
            producer_loaded_at: now,
            stalled: None,
            closing: None,
            withdrawn: false,
            log_file_name,
            udp: None,
        };
        self.registration_count += 1;
        let ready = Event::PublicationReady {
            correlation_id,
            registration_id: correlation_id,
            session_id,
            stream_id,
            publication_limit_counter_id: counter_id(&publication.limit),
            channel_status_indicator_id: NO_CHANNEL_STATUS,
            log_file_name: publication.log_file_name.as_bytes(),
        };
        let exclusive_ready = Event::ExclusivePublicationReady {
            correlation_id,
            registration_id: correlation_id,
            session_id,
            stream_id,
            publication_limit_counter_id: counter_id(&publication.limit),
            channel_status_indicator_id: NO_CHANNEL_STATUS,
            log_file_name: publication.log_file_name.as_bytes(),
        };
        self.send(if exclusive { &exclusive_ready } else { &ready });
        self.publications.push(publication);
        let publication_index = self.publications.len() - 1;
        let matching: Vec<i64> = self
            .subscriptions
            .iter()
            .filter(|subscription| subscription.stream_id == stream_id)
            .map(|subscription| subscription.registration_id)
            .collect();
        for subscription_id in matching {
            self.add_image(publication_index, subscription_id, now);
        }
    }

    /// `add publication` on a `cavalry:udp` channel (`spec/client-protocol.md`,
    /// "Commands"; `spec/udp.md`, "The channel URI" and "The sender"; ADR 0041): the host
    /// is resolved once, here on the control plane, code 9 when it does not; a shared
    /// publication on the same stream and resolved channel joins the first; otherwise the
    /// first publication on the resolved channel opens its send endpoint, one socket,
    /// bound, connected and non-blocking, code 4 when the operating system refuses, with
    /// the five counters of `channel.*` and `channel.opened`; the log has the UDP sizes
    /// of "Values"; `publication.position`, `publication.limit`, `sender.position` and
    /// `sender.limit` are allocated; the endpoint's socket, when new, and the publication
    /// with a reader on its log go to the sender; and `publication ready` names the
    /// endpoint's `channel.status` counter as the channel status indicator id. The
    /// publication is connected by the first status message and not before, so no image
    /// is made and the limit stays at the sender position until the sender says so. A
    /// registry at capacity, a counters file with no free record, or a sender's queue
    /// with no room is code 10, with nothing registered.
    fn add_udp_publication(
        &mut self,
        client_id: i64,
        correlation_id: i64,
        stream_id: i32,
        channel: UdpChannel<'_>,
        exclusive: bool,
        now: i64,
    ) {
        if self.registration_count >= spec::MAX_PUBLICATIONS {
            self.registry_full(correlation_id, "publications", spec::MAX_PUBLICATIONS);
            return;
        }
        let Some(target) = self.resolve_target(correlation_id, &channel) else {
            return;
        };
        let flow_control = channel.flow_control();
        let registration = Registration {
            client_id,
            correlation_id,
        };
        // A shared publication on the same stream and resolved channel joins the first,
        // under the first's flow control strategy: one that names another is refused
        // (`spec/udp.md`, "The channel URI"; ADR 0053).
        if !exclusive && let Some(endpoint_index) = self.open_send_endpoint_index(target) {
            let endpoint_id = self.send_endpoints[endpoint_index].registration_id;
            let live = self.publications.iter().position(|publication| {
                !publication.exclusive
                    && publication.stream_id == stream_id
                    && publication.closing.is_none()
                    && publication
                        .udp
                        .as_ref()
                        .is_some_and(|udp| udp.endpoint_id == endpoint_id)
            });
            if let Some(index) = live {
                let held = self.publications[index]
                    .udp
                    .as_ref()
                    .map_or(FlowControl::Max, |udp| udp.flow_control);
                if held != flow_control {
                    self.statics.rejected_command.increment();
                    self.send_error(
                        correlation_id,
                        ErrorCode::InvalidChannel,
                        &format!(
                            "the channel: channel refused (fc): the live publication on stream \
                             {stream_id} and this channel has fc={}, and a join keeps one \
                             strategy; name fc={} to join it, or another stream; see \
                             {ERRORS_DOC}",
                            held.name(),
                            held.name()
                        ),
                    );
                    return;
                }
                // A shared log has one position: a join names the live publication's
                // position keys, or none where it named none (ADR 0059).
                let held_position = self.publications[index]
                    .udp
                    .as_ref()
                    .and_then(|udp| udp.position);
                if held_position != channel.position_keys() {
                    self.statics.rejected_command.increment();
                    let created = match held_position {
                        Some(keys) => format!(
                            "with init-term-id={} term-id={} term-offset={}",
                            keys.initial_term_id, keys.term_id, keys.term_offset
                        ),
                        None => "with no position keys".to_string(),
                    };
                    self.send_error(
                        correlation_id,
                        ErrorCode::InvalidChannel,
                        &format!(
                            "the channel: channel refused (position): the live publication on \
                             stream {stream_id} and this channel was created {created}, and a \
                             join keeps one position; name the same keys to join it, or another \
                             stream; see {ERRORS_DOC}"
                        ),
                    );
                    return;
                }
                let publication = &mut self.publications[index];
                publication.registrations.push(registration);
                self.registration_count += 1;
                let (registration_id, session_id, limit_id) = (
                    publication.registration_id,
                    publication.session_id,
                    counter_id(&publication.limit),
                );
                let log_file_name = publication.log_file_name.clone();
                let status_id = counter_id(&self.send_endpoints[endpoint_index].status);
                let ready = Event::PublicationReady {
                    correlation_id,
                    registration_id,
                    session_id,
                    stream_id,
                    publication_limit_counter_id: limit_id,
                    channel_status_indicator_id: status_id,
                    log_file_name: log_file_name.as_bytes(),
                };
                self.send(&ready);
                return;
            }
        }
        // Room in the sender's queue for an endpoint and a publication, else code 10.
        if !self.sender_has_room() {
            self.sender_queue_full(correlation_id);
            return;
        }
        // The endpoint: the open one on the resolved channel, or a new one, through the
        // path a replay's publication takes too (ADR 0043).
        let (endpoint_index, socket) = match self.open_send_endpoint(correlation_id, target, now) {
            Ok(opened) => opened,
            Err((code, message)) => {
                self.send_error(correlation_id, code, &message);
                return;
            }
        };
        let endpoint_id = self.send_endpoints[endpoint_index].registration_id;
        // The log, with the UDP sizes, and the four counters.
        let session_id = self.advance_session_id(stream_id);
        let initial_term_id =
            i32::from_le_bytes((now >> 16).to_le_bytes()[..4].try_into().unwrap_or([0; 4]));
        let log_type = if exclusive {
            LogType::Exclusive
        } else {
            LogType::Concurrent
        };
        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
        // The spec's UDP term, or the smaller one a test at a registry's capacity sets,
        // since every log is made resident when it is created (ADR 0027). Nothing
        // depends on the log's term matching what the sender parses a frame against:
        // the sender holds a status message's and a NAK's offsets against the
        // publication's own term, as it must for a replay's publication, whose log
        // keeps its recording's term (ADR 0043).
        let term_length = self.term_length.min(spec::UDP_TERM_LENGTH);
        // Version 2's position keys create the log at a position with their initial
        // term id, as a replay's log is created (`spec/udp.md`, "The channel URI"; ADR
        // 0059), the position counted in the log's own term, the spec's unless a test
        // shrank it, so that the term ids on the wire are the keys'.
        let keys = channel.position_keys();
        let initial_term_id = keys.map_or(initial_term_id, |keys| keys.initial_term_id);
        let start_position = keys.map_or(0, |keys| {
            i64::from(keys.term_count()) * i64::from(term_length) + i64::from(keys.term_offset)
        });
        let fields = HeaderFields::new(
            log_type,
            correlation_id,
            initial_term_id,
            term_length,
            spec::UDP_MTU_LENGTH,
            term_length / 2,
            session_id,
            stream_id,
        );
        let log = match fields
            .and_then(|fields| LogBuffers::create_at(&self.dir, &fields, start_position))
        {
            Ok(log) => log,
            Err(error) => {
                self.statics.log_create_failed.increment();
                self.abandon_send_endpoint(endpoint_index, socket.is_some(), reuse);
                self.send_error(correlation_id, ErrorCode::StorageSpace, &format!("{error}"));
                return;
            }
        };
        let key = position_key(correlation_id, session_id, stream_id);
        // The sender's labels end in the endpoint's registration id, so that `doctor`
        // names a publication's endpoint from the counters file alone, and the receivers
        // gauge's in the strategy after it, so that `doctor` reads which publications
        // keep a `min` table (ADR 0053).
        let label = |name: &str, endpoint: bool| {
            let mut label = format!("{name} {correlation_id} {session_id} {stream_id}");
            if endpoint {
                label.push_str(&format!(" {endpoint_id}"));
            }
            if name == "sender.flow_control_receivers" {
                label.push_str(&format!(" fc={}", flow_control.name()));
            }
            label
        };
        let mut allocated: Vec<Counter> = Vec::with_capacity(5);
        for (id, name, endpoint) in [
            (PUBLICATION_POSITION, "publication.position", false),
            (PUBLICATION_LIMIT, "publication.limit", false),
            (SENDER_POSITION, "sender.position", true),
            (SENDER_LIMIT, "sender.limit", true),
            (
                SENDER_FLOW_CONTROL_RECEIVERS,
                "sender.flow_control_receivers",
                true,
            ),
        ] {
            match self.counters.allocate(id, &key, &label(name, endpoint)) {
                Ok(counter) => allocated.push(counter),
                Err(error) => {
                    for counter in allocated {
                        let _ = self.counters.free(counter.id(), reuse);
                    }
                    drop(log);
                    remove_log(&self.dir, correlation_id);
                    self.abandon_send_endpoint(endpoint_index, socket.is_some(), reuse);
                    self.counters_full(correlation_id, &error);
                    return;
                }
            }
        }
        let mut counters = allocated.into_iter();
        let (
            Some(position),
            Some(limit),
            Some(sender_position),
            Some(sender_limit),
            Some(receivers),
        ) = (
            counters.next(),
            counters.next(),
            counters.next(),
            counters.next(),
            counters.next(),
        )
        else {
            return;
        };
        let reader = match log.reader(start_position) {
            Ok(reader) => reader,
            Err(error) => {
                for counter in [
                    &position,
                    &limit,
                    &sender_position,
                    &sender_limit,
                    &receivers,
                ] {
                    let _ = self.counters.free(counter.id(), reuse);
                }
                drop(log);
                remove_log(&self.dir, correlation_id);
                self.abandon_send_endpoint(endpoint_index, socket.is_some(), reuse);
                self.statics.log_create_failed.increment();
                self.send_error(correlation_id, ErrorCode::StorageSpace, &format!("{error}"));
                return;
            }
        };
        position.store_release(start_position);
        limit.store_release(start_position);
        sender_position.store_release(start_position);
        let log_file_name = log_layout::file_name(correlation_id)
            .to_string_lossy()
            .into_owned();
        let publication = Publication {
            registration_id: correlation_id,
            session_id,
            stream_id,
            exclusive,
            replay: false,
            log,
            position,
            limit,
            registrations: vec![registration],
            consumer_position: start_position,
            clean_position: start_position,
            last_limit: start_position,
            connected_flag: false,
            producer: start_position,
            producer_loaded_at: now,
            stalled: None,
            closing: None,
            withdrawn: false,
            log_file_name,
            udp: Some(UdpPublication {
                endpoint_id,
                sender_position: sender_position.clone(),
                sender_limit_id: sender_limit.id(),
                term_length: i64::from(term_length),
                connected: false,
                close_sent: false,
                sender_closed: false,
                flow_control,
                receivers_id: receivers.id(),
                position: keys,
            }),
        };
        let add = sender::AddPublication {
            registration_id: correlation_id,
            endpoint_id,
            session_id,
            stream_id,
            initial_term_id,
            term_length,
            mtu_length: spec::UDP_MTU_LENGTH,
            reader,
            position: sender_position,
            limit: sender_limit,
            flow_control,
            receivers,
            receiver_table: receiver_table(flow_control),
        };
        self.hand_to_sender(endpoint_index, socket, add);
        let status_id = counter_id(&self.send_endpoints[endpoint_index].status);
        self.registration_count += 1;
        let ready = Event::PublicationReady {
            correlation_id,
            registration_id: correlation_id,
            session_id,
            stream_id,
            publication_limit_counter_id: counter_id(&publication.limit),
            channel_status_indicator_id: status_id,
            log_file_name: publication.log_file_name.as_bytes(),
        };
        let exclusive_ready = Event::ExclusivePublicationReady {
            correlation_id,
            registration_id: correlation_id,
            session_id,
            stream_id,
            publication_limit_counter_id: counter_id(&publication.limit),
            channel_status_indicator_id: status_id,
            log_file_name: publication.log_file_name.as_bytes(),
        };
        self.send(if exclusive { &exclusive_ready } else { &ready });
        self.publications.push(publication);
    }

    /// Undoes a send endpoint opened for an `add publication` that failed after it: the
    /// socket is dropped with the caller's handle, the five counters are freed and the
    /// registry slot returned; nothing counts, since a client never observed it.
    fn abandon_send_endpoint(&mut self, index: usize, new: bool, reuse: i64) {
        if !new || index >= self.send_endpoints.len() {
            return;
        }
        let entry = self.send_endpoints.swap_remove(index);
        for counter in [
            entry.status,
            entry.socket_rcvbuf,
            entry.socket_sndbuf,
            entry.images,
            entry.publications,
            entry.destinations,
        ] {
            let _ = self.counters.free(counter.id(), reuse);
        }
    }

    /// The index of the open send endpoint on the resolved channel, if any.
    fn open_send_endpoint_index(&self, target: SendTarget) -> Option<usize> {
        self.send_endpoints
            .iter()
            .position(|entry| entry.target == target && entry.state == ChannelState::Active)
    }

    /// Whether the sender's request queue has room for an endpoint and a publication,
    /// with no request still waiting for it: an add that finds none is code 10, and
    /// never deferred (ADR 0041).
    fn sender_has_room(&self) -> bool {
        self.sender_deferred.is_empty()
            && self.sender.requests.capacity() - self.sender.requests.len() >= 2
    }

    /// The send endpoint a `cavalry:udp` publication sends on (`spec/udp.md`, "The
    /// sender"; ADR 0041): the open one on the resolved channel, or a new one, its
    /// socket opened, bound and connected here, or bound to a control address and
    /// connected to nothing (ADR 0053), its six counters allocated and
    /// `channel.status` at 0 until `hand_to_sender` gives the sender the socket.
    /// `correlation_id` names the command that opens it, a client's `add publication`
    /// or a replay's `start replay` (ADR 0043), and keys `channel.status`. Returns the
    /// endpoint's index and the socket when it is new, or the code and message for the
    /// caller to answer with, the refusal counted here: the registry at capacity or the
    /// counters file with no free record is code 10, a socket the operating system
    /// refused code 4, whose message says to send the command again when a closing
    /// endpoint on the channel still holds its address (ADR 0055). The caller checks
    /// the sender's room first (`sender_has_room`).
    fn open_send_endpoint(
        &mut self,
        correlation_id: i64,
        target: SendTarget,
        now: i64,
    ) -> Result<(usize, Option<SendSocket>), (ErrorCode, String)> {
        if let Some(index) = self.open_send_endpoint_index(target) {
            return Ok((index, None));
        }
        if self.send_endpoints.len() >= spec::MAX_SEND_ENDPOINTS {
            self.statics.registry_full.increment();
            return Err((
                ErrorCode::ResourceTemporarilyUnavailable,
                registry_full_message("send endpoints", spec::MAX_SEND_ENDPOINTS),
            ));
        }
        let opened = if target.control {
            SendSocket::open_control(target.address)
        } else {
            SendSocket::open(target.address, target.interface)
        };
        let socket = match opened {
            Ok(socket) => socket,
            // The channel's previous endpoint, closed with its last publication's linger,
            // still holds the address while the sender drops its socket, one sender cycle
            // while the sender keeps up: the driver's own timing, not the operator's port,
            // so the message says to send the command again (ADR 0055, answering ADR
            // 0049's Revisit). An add during the linger met the open endpoint above.
            Err(error)
                if self.send_endpoints.iter().any(|entry| {
                    entry.target == target && entry.state == ChannelState::Closing
                }) =>
            {
                return Err((
                    ErrorCode::ChannelEndpointError,
                    format!(
                        "the channel {}: its previous send endpoint is still closing, its \
                         socket with the sender after its last publication's linger, so a new \
                         socket cannot be bound to {}: {error}; send the command again after a \
                         moment, once the sender has closed it; see {ERRORS_DOC}",
                        target.resolved(),
                        target.interface.unwrap_or(target.address)
                    ),
                ));
            }
            Err(error) => {
                return Err((
                    ErrorCode::ChannelEndpointError,
                    if target.control {
                        format!(
                            "the channel's control address {}: the socket could not be opened \
                             or bound: {error}; check that the address is this host's and its \
                             port is free; see {ERRORS_DOC}",
                            target.address
                        )
                    } else {
                        format!(
                            "the channel's endpoint {}{}: the socket could not be opened, \
                             bound or connected: {error}; check that the interface is this \
                             host's and its port is free; see {ERRORS_DOC}",
                            target.address,
                            target
                                .interface
                                .map_or(String::new(), |i| format!(" from interface {i}"))
                        )
                    },
                ));
            }
        };
        let resolved = target.resolved();
        let key = correlation_id.to_le_bytes();
        let label = |name: &str| format!("{name} {resolved}");
        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
        let mut allocated: Vec<Counter> = Vec::with_capacity(6);
        for (id, name) in [
            (CHANNEL_STATUS, "channel.status"),
            (CHANNEL_SOCKET_RCVBUF, "channel.socket_rcvbuf"),
            (CHANNEL_SOCKET_SNDBUF, "channel.socket_sndbuf"),
            (CHANNEL_IMAGES, "channel.images"),
            (CHANNEL_PUBLICATIONS, "channel.publications"),
            (CHANNEL_DESTINATIONS, "channel.destinations"),
        ] {
            match self.counters.allocate(id, &key, &label(name)) {
                Ok(counter) => allocated.push(counter),
                Err(error) => {
                    for counter in allocated {
                        let _ = self.counters.free(counter.id(), reuse);
                    }
                    self.statics.registry_full.increment();
                    return Err((
                        ErrorCode::ResourceTemporarilyUnavailable,
                        format!("{error}"),
                    ));
                }
            }
        }
        // One record per name above; a count that does not match cannot happen, and is
        // freed and refused like a full counters file rather than left behind.
        let [
            status,
            socket_rcvbuf,
            socket_sndbuf,
            images,
            publications,
            destinations,
        ] = match <[Counter; 6]>::try_from(allocated) {
            Ok(six) => six,
            Err(held) => {
                for counter in held {
                    let _ = self.counters.free(counter.id(), reuse);
                }
                self.statics.registry_full.increment();
                return Err((
                    ErrorCode::ResourceTemporarilyUnavailable,
                    counters_miscounted_message(),
                ));
            }
        };
        let (rcvbuf, sndbuf) = socket.buffer_lengths();
        status.set(0);
        socket_rcvbuf.set(rcvbuf);
        socket_sndbuf.set(sndbuf);
        images.set(0);
        publications.set(0);
        destinations.set(0);
        self.send_endpoints.push(SendEndpoint {
            registration_id: correlation_id,
            target,
            state: ChannelState::Initializing,
            status,
            socket_rcvbuf,
            socket_sndbuf,
            images,
            publications,
            destinations,
            destination_list: Vec::with_capacity(if target.control { MAX_DESTINATIONS } else { 0 }),
            publication_count: 0,
        });
        Ok((self.send_endpoints.len() - 1, Some(socket)))
    }

    /// Hands the sender a publication to put on the wire, and its endpoint first when
    /// `socket` is a new endpoint's (`spec/udp.md`, "The sender"; ADR 0041): the room
    /// was checked by the caller, so neither offer fails; a new endpoint becomes active
    /// with `channel.status` 1 and `channel.opened`, and the endpoint counts one more
    /// publication.
    fn hand_to_sender(
        &mut self,
        endpoint_index: usize,
        socket: Option<SendSocket>,
        add: sender::AddPublication,
    ) {
        let endpoint_id = add.endpoint_id;
        if let Some(socket) = socket {
            // A control channel's destinations table, with its room, allocated here on
            // the control plane so that the sender never grows it (ADR 0053).
            let destinations = self.send_endpoints[endpoint_index]
                .target
                .control
                .then(|| Vec::with_capacity(MAX_DESTINATIONS));
            if self
                .sender
                .requests
                .offer(sender::Request::AddEndpoint {
                    registration_id: endpoint_id,
                    socket,
                    destinations,
                })
                .is_err()
            {
                self.statics.registry_full.increment();
            }
            let entry = &mut self.send_endpoints[endpoint_index];
            entry.state = ChannelState::Active;
            entry.status.set(1);
            self.statics.channel_opened.increment();
        }
        if self
            .sender
            .requests
            .offer(sender::Request::AddPublication(add))
            .is_err()
        {
            self.statics.registry_full.increment();
        }
        self.wake_sender();
        let entry = &mut self.send_endpoints[endpoint_index];
        entry.publication_count += 1;
        entry
            .publications
            .set(i64::try_from(entry.publication_count).unwrap_or(i64::MAX));
    }

    /// Answers `add publication` with error code 10 when the sender's request queue has
    /// no room for an endpoint and a publication, or requests are still waiting for it.
    fn sender_queue_full(&mut self, correlation_id: i64) {
        self.statics.registry_full.increment();
        self.send_error(
            correlation_id,
            ErrorCode::ResourceTemporarilyUnavailable,
            &sender_queue_full_message(),
        );
    }

    /// Offers a request to the sender, or keeps it for the next cycle when the queue is
    /// full or earlier requests are still waiting, so that the sender sees them in order.
    /// Only closes and endpoint removes come here, bounded by
    /// [`SENDER_DEFERRED_CAPACITY`]; an add that finds no room is refused instead.
    fn sender_request(&mut self, request: sender::Request) {
        let request = if self.sender_deferred.is_empty() {
            match self.sender.requests.offer(request) {
                Ok(()) => {
                    self.wake_sender();
                    return;
                }
                Err(request) => request,
            }
        } else {
            request
        };
        debug_assert!(
            self.sender_deferred.len() < SENDER_DEFERRED_CAPACITY,
            "the sender's deferred requests passed one close per publication and one remove per endpoint"
        );
        self.sender_deferred.push_back(request);
    }

    /// Offers the sender's deferred requests in order until the queue is full again.
    fn flush_sender_deferred(&mut self) -> usize {
        let mut sent = 0;
        while let Some(request) = self.sender_deferred.pop_front() {
            match self.sender.requests.offer(request) {
                Ok(()) => sent += 1,
                Err(request) => {
                    self.sender_deferred.push_front(request);
                    break;
                }
            }
        }
        if sent > 0 {
            self.wake_sender();
        }
        sent
    }

    /// Takes every reply the sender has queued and acts on it: the connected flag the
    /// publisher reads, the close the conductor finishes, the endpoint's counters freed.
    fn take_sender_replies(&mut self, now: i64) -> usize {
        let mut taken = 0;
        while let Some(reply) = self.sender.replies.poll() {
            match reply {
                sender::Reply::Connected { registration_id } => {
                    self.set_udp_connected(registration_id, true);
                }
                sender::Reply::Disconnected { registration_id } => {
                    self.set_udp_connected(registration_id, false);
                }
                sender::Reply::Closed { registration_id } => {
                    if let Some(publication) = self
                        .publications
                        .iter_mut()
                        .find(|publication| publication.registration_id == registration_id)
                        && let Some(udp) = &mut publication.udp
                    {
                        udp.sender_closed = true;
                    }
                }
                sender::Reply::EndpointClosed { registration_id } => {
                    if let Some(index) = self
                        .send_endpoints
                        .iter()
                        .position(|endpoint| endpoint.registration_id == registration_id)
                    {
                        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
                        let entry = self.send_endpoints.swap_remove(index);
                        for counter in [
                            entry.status,
                            entry.socket_rcvbuf,
                            entry.socket_sndbuf,
                            entry.images,
                            entry.publications,
                            entry.destinations,
                        ] {
                            let _ = self.counters.free(counter.id(), reuse);
                        }
                    }
                }
            }
            taken += 1;
        }
        taken
    }

    /// What the sender said about a `cavalry:udp` publication's receiver, kept for the
    /// limit pass. A disconnect stores the log's connected flag 0 here, before the limit
    /// pass lowers the limit; a connect leaves the 1 to the limit pass, after its raise
    /// (ADR 0048).
    fn set_udp_connected(&mut self, registration_id: i64, connected: bool) {
        if let Some(index) = self
            .publications
            .iter()
            .position(|publication| publication.registration_id == registration_id)
            && let Some(udp) = &mut self.publications[index].udp
        {
            udp.connected = connected;
            if !connected {
                self.lower_connected(index);
            }
        }
        // A replay's publication: the replay's entry keeps the fact for the connect
        // timeout, so that `keep_replays` reads one bool and scans nothing (ADR 0043).
        if let Some(replay) = self
            .replays
            .iter_mut()
            .find(|replay| replay.replay_session_id == registration_id)
        {
            replay.connected = connected;
        }
    }

    /// A session id no live publication of `stream_id` holds.
    fn advance_session_id(&mut self, stream_id: i32) -> i32 {
        loop {
            let candidate = self.next_session_id;
            self.next_session_id = self.next_session_id.wrapping_add(1);
            let taken = self.publications.iter().any(|publication| {
                publication.stream_id == stream_id && publication.session_id == candidate
            });
            if !taken {
                return candidate;
            }
        }
    }

    /// Creates the image of publication `publication_index` for subscription
    /// `subscription_id`: a position counter the client owns, at the producer's position,
    /// and an `available image` event, sent at `now`.
    fn add_image(&mut self, publication_index: usize, subscription_id: i64, now: i64) {
        if !self
            .subscriptions
            .iter()
            .any(|subscription| subscription.registration_id == subscription_id)
        {
            return;
        }
        if self.images.len() >= spec::MAX_IMAGES {
            self.registry_full(subscription_id, "images", spec::MAX_IMAGES);
            return;
        }
        let Some(publication) = self.publications.get(publication_index) else {
            return;
        };
        let (publication_id, session_id, stream_id) = (
            publication.registration_id,
            publication.session_id,
            publication.stream_id,
        );
        let key = position_key(publication_id, session_id, stream_id);
        let label = format!("subscription.position {publication_id} {session_id} {stream_id}");
        let position = match self.counters.allocate(SUBSCRIPTION_POSITION, &key, &label) {
            Ok(counter) => counter,
            Err(error) => {
                self.counters_full(subscription_id, &error);
                return;
            }
        };
        let join_position = publication
            .log
            .producer_position()
            .unwrap_or(publication.consumer_position);
        position.store_release(join_position);
        let first_image = !self
            .images
            .iter()
            .any(|image| image.publication_id == publication_id);
        let publication = &mut self.publications[publication_index];
        if first_image && publication.consumer_position < join_position {
            publication.consumer_position = join_position;
        }
        let log_file_name = publication.log_file_name.clone();
        let event = Event::AvailableImage {
            correlation_id: publication_id,
            session_id,
            stream_id,
            subscriber_registration_id: subscription_id,
            subscriber_position_counter_id: counter_id(&position),
            log_file_name: log_file_name.as_bytes(),
            source_identity: IPC_CHANNEL,
        };
        self.images.push(Image {
            publication_id,
            subscription_id,
            position,
            joined: join_position,
            announced: now,
        });
        self.send(&event);
    }

    fn remove_publication(
        &mut self,
        client_id: i64,
        correlation_id: i64,
        registration_id: i64,
        now: i64,
    ) {
        let found = self.publications.iter().position(|publication| {
            publication.registrations.iter().any(|registration| {
                registration.correlation_id == registration_id
                    && registration.client_id == client_id
            })
        });
        let Some(index) = found else {
            self.send_error(
                correlation_id,
                ErrorCode::UnknownPublication,
                &format!("registration {registration_id} is not a publication of client {client_id}; see {ERRORS_DOC}"),
            );
            return;
        };
        self.drop_registration(index, registration_id, now);
        self.send(&Event::OperationSuccess { correlation_id });
    }

    /// Removes one registration of the publication at `index`; the last one closes the
    /// publication: the end of stream position is stored and the linger starts.
    fn drop_registration(&mut self, index: usize, registration_id: i64, now: i64) {
        let publication = &mut self.publications[index];
        let before = publication.registrations.len();
        publication
            .registrations
            .retain(|registration| registration.correlation_id != registration_id);
        self.registration_count = self
            .registration_count
            .saturating_sub(before - publication.registrations.len());
        let mut close = None;
        if publication.registrations.is_empty() && publication.closing.is_none() {
            let end = publication
                .log
                .producer_position()
                .unwrap_or(publication.consumer_position);
            let _ = publication.log.set_end_of_stream_position(end);
            publication.closing = Some((end, now));
            if let Some(udp) = &mut publication.udp
                && !udp.close_sent
            {
                udp.close_sent = true;
                close = Some((publication.registration_id, end));
            }
        }
        // Over UDP the sender lingers the publication: what is left goes, heartbeats
        // carry EOS, NAKs are answered, and its reply ends the close (ADR 0041).
        if let Some((registration_id, end_of_stream_position)) = close {
            self.sender_request(sender::Request::Close {
                registration_id,
                end_of_stream_position,
            });
        }
    }

    fn add_subscription(
        &mut self,
        client_id: i64,
        correlation_id: i64,
        stream_id: i32,
        channel: &[u8],
        now: i64,
    ) {
        let raw = channel;
        let Some(channel) = self.check_channel(correlation_id, channel, "the channel") else {
            return;
        };
        if let Channel::Udp(udp) = channel {
            self.add_udp_subscription(client_id, correlation_id, stream_id, udp, raw);
            return;
        }
        if self.subscriptions_full() {
            self.registry_full(correlation_id, "subscriptions", spec::MAX_SUBSCRIPTIONS);
            return;
        }
        self.subscriptions.push(Subscription {
            registration_id: correlation_id,
            client_id,
            stream_id,
            endpoint: None,
        });
        self.send(&Event::SubscriptionReady {
            correlation_id,
            channel_status_indicator_id: NO_CHANNEL_STATUS,
        });
        let matching: Vec<usize> = self
            .publications
            .iter()
            .enumerate()
            .filter(|(_, publication)| {
                publication.stream_id == stream_id
                    && publication.closing.is_none()
                    && publication.udp.is_none()
            })
            .map(|(index, _)| index)
            .collect();
        for index in matching {
            self.add_image(index, correlation_id, now);
        }
    }

    /// Where a publication's channel sends (`spec/udp.md`, "The channel URI"; ADR 0053):
    /// a control channel's control address resolved once, code 9 when it does not
    /// resolve, or the endpoint and interface as `resolve_channel` resolves them.
    fn resolve_target(
        &mut self,
        correlation_id: i64,
        channel: &UdpChannel<'_>,
    ) -> Option<SendTarget> {
        if let Some(control) = channel.control() {
            let host = control.host();
            return match resolve(host, control.port().unwrap_or(0)) {
                Ok(address) => Some(SendTarget {
                    address,
                    interface: None,
                    control: true,
                }),
                Err(reason) => {
                    self.send_error(
                        correlation_id,
                        ErrorCode::UnknownHost,
                        &format!(
                            "the channel's control host `{host}` does not resolve: {reason}; \
                             check the name, or use an address; see {ERRORS_DOC}"
                        ),
                    );
                    None
                }
            };
        }
        let (address, interface) = self.resolve_channel(correlation_id, channel)?;
        Some(SendTarget {
            address,
            interface,
            control: false,
        })
    }

    /// Resolves a `cavalry:udp` channel's endpoint and interface once, on the control
    /// plane, answering code 9 for a name that does not resolve (`spec/udp.md`, "The
    /// channel URI").
    fn resolve_channel(
        &mut self,
        correlation_id: i64,
        channel: &UdpChannel<'_>,
    ) -> Option<(SocketAddr, Option<SocketAddr>)> {
        // A control channel never reaches here: a publication's goes through
        // `resolve_target`, and a subscription's is refused before.
        let address = channel.endpoint()?;
        let host = address.host();
        let endpoint = match resolve(host, address.port().unwrap_or(0)) {
            Ok(address) => address,
            Err(reason) => {
                self.send_error(
                    correlation_id,
                    ErrorCode::UnknownHost,
                    &format!(
                        "the channel's endpoint host `{host}` does not resolve: {reason}; check \
                         the name, or use an address; see {ERRORS_DOC}"
                    ),
                );
                return None;
            }
        };
        let interface = match channel.interface() {
            None => None,
            Some(address) => match resolve(address.host(), address.port().unwrap_or(0)) {
                Ok(resolved) => Some(resolved),
                Err(reason) => {
                    self.send_error(
                        correlation_id,
                        ErrorCode::UnknownHost,
                        &format!(
                            "the channel's interface `{}` does not resolve: {reason}; check \
                             the name, or use an address; see {ERRORS_DOC}",
                            address.host()
                        ),
                    );
                    return None;
                }
            },
        };
        Some((endpoint, interface))
    }

    /// Whether the subscriptions registry is at capacity: its registered subscriptions
    /// and the adds waiting for a closing receive endpoint, which hold a slot each
    /// (`spec/client-protocol.md`, "Sizes"; ADR 0049). Every `add subscription`, on
    /// `cavalry:ipc` or `cavalry:udp`, asks this one, so neither takes a waiting add's
    /// slot.
    fn subscriptions_full(&self) -> bool {
        self.subscriptions.len() + self.pending_subscriptions.len() >= spec::MAX_SUBSCRIPTIONS
    }

    /// Stores how many adds wait for a closing receive endpoint now in
    /// `subscription.add_deferred_waiting`, after every change to the list, so that
    /// `cavalry doctor` can tell a registry full of waiting adds from one full of
    /// subscriptions (ADR 0049).
    fn count_waiting_adds(&self) {
        self.statics
            .subscription_add_deferred_waiting
            .set(i64::try_from(self.pending_subscriptions.len()).unwrap_or(i64::MAX));
    }

    /// Answers every `add subscription` that has waited [`spec::CLOSING_ENDPOINT_WAIT`]
    /// for its channel's previous receive endpoint to close with code 4, the bind it
    /// would have made, and counts it; nothing is registered, and the close that comes
    /// later runs nothing for it (`spec/client-protocol.md`, "Commands", "Liveness and
    /// timeouts"; ADR 0049). The endpoint stays open that long for one of two causes,
    /// and the message names both with what to do for each: its images are still being
    /// retired, a retired image's log kept until its recording has written up to where
    /// the image stopped, a recorder behind a slow or failing disk (ADR 0068); or the
    /// receiver is behind, its thread starved under cgroup CPU throttling, or the
    /// endpoint's remove or an image's retire waiting behind a full request queue.
    /// Returns how many it answered.
    fn time_out_pending_subscriptions(&mut self, now: i64) -> usize {
        let wait = spec::nanos(spec::CLOSING_ENDPOINT_WAIT);
        let waited = |pending: &PendingSubscription| now.saturating_sub(pending.arrived) >= wait;
        // A cycle with no add due runs this scan and nothing else; `extract_if` below
        // keeps the adds that stay in their order.
        if !self.pending_subscriptions.iter().any(waited) {
            return 0;
        }
        let mut due = Vec::with_capacity(self.pending_subscriptions.len());
        due.extend(
            self.pending_subscriptions
                .extract_if(.., |pending| waited(pending)),
        );
        self.count_waiting_adds();
        let answered = due.len();
        for pending in due {
            self.statics.subscription_add_deferred_timed_out.increment();
            self.send_error(
                pending.correlation_id,
                ErrorCode::ChannelEndpointError,
                &format!(
                    "the channel's endpoint {}: the previous receive endpoint on it has not \
                     closed in the {} s this add waited, its images still being retired or \
                     its socket still with the receiver, so a new one cannot be bound; if a \
                     recording of one of its images has not yet stopped, the image's log is \
                     kept until the recorder has written up to where the image stopped and \
                     the recorder is behind, on a slow or failing disk: check \
                     recording.write_failed and the log directory's disk with `cavalry \
                     doctor`, or stop the recording before removing the stream's last \
                     subscription; otherwise the driver's receiver thread is behind, starved \
                     of CPU or busy: give the driver's threads the CPU they need; then add \
                     the subscription again; see {ERRORS_DOC}",
                    pending.endpoint,
                    spec::CLOSING_ENDPOINT_WAIT.as_secs()
                ),
            );
        }
        answered
    }

    /// `add subscription` on a `cavalry:udp` channel (`spec/client-protocol.md`,
    /// "Commands"; `spec/udp.md`, "The receiver"; ADR 0042): a channel that names
    /// `control`, `control-mode` or `fc`, a publication's keys, is code 1 with the reason
    /// `subscription`, counted in `client_protocol.rejected_command`, with nothing
    /// registered; the host is resolved once, code 9 when it does not; the first
    /// subscription on the resolved channel binds its receive endpoint, one socket bound
    /// to the endpoint and set non-blocking, code 4 when the operating system refuses,
    /// with the five counters of `channel.*` and `channel.opened`, handed to the receiver
    /// with the list its interests live in; the subscription is registered on the
    /// endpoint, the receiver told the stream is wanted, and `subscription ready` names
    /// the endpoint's `channel.status` counter.
    /// An image already live on the endpoint and stream gets a `subscription.position`
    /// for the client at the image's rebuild position, an `available image`, and a
    /// reader with the receiver. A registry at capacity, a counters file with no free
    /// record, or a receiver's queue with no room is code 10, with nothing registered.
    fn add_udp_subscription(
        &mut self,
        client_id: i64,
        correlation_id: i64,
        stream_id: i32,
        channel: UdpChannel<'_>,
        raw: &[u8],
    ) {
        if self.subscriptions_full() {
            self.registry_full(correlation_id, "subscriptions", spec::MAX_SUBSCRIPTIONS);
            return;
        }
        // `control`, `control-mode`, `fc` and the position keys are a publication's
        // (`spec/udp.md`, "The channel URI"; `spec/client-protocol.md`, "Commands", add
        // subscription; ADR 0059).
        if channel.is_control() || channel.names_flow_control() || channel.names_position() {
            self.statics.rejected_command.increment();
            self.send_error(
                correlation_id,
                ErrorCode::InvalidChannel,
                &format!(
                    "the channel: channel refused (subscription): it names control, \
                     control-mode, fc, init-term-id, term-id or term-offset, which this version \
                     reads on a publication's channel only; remove them, since a subscription \
                     receives on its endpoint; see {ERRORS_DOC}"
                ),
            );
            return;
        }
        let Some((endpoint, interface)) = self.resolve_channel(correlation_id, &channel) else {
            return;
        };
        // An endpoint its last subscription has left closes once its images are
        // retired (`spec/udp.md`, "The receiver"): it is closing from that removal,
        // though it is active here until the receiver has said the images went. An
        // add in between must neither join an image being retired nor keep the
        // endpoint, on which the stream's session would sit out a cool-down: it waits
        // for the close as an add on a closing endpoint does (ADR 0068).
        let leaving = self.receive_endpoints.iter().any(|entry| {
            entry.endpoint == endpoint
                && entry.interface == interface
                && entry.state == ChannelState::Active
                && entry.subscription_count == 0
        });
        let open = self.receive_endpoints.iter().position(|entry| {
            entry.endpoint == endpoint
                && entry.interface == interface
                && entry.state == ChannelState::Active
                && entry.subscription_count > 0
        });
        // The channel's previous endpoint, closed with its last subscription and image,
        // still holds the socket while the receiver drops it, so the bind below would
        // fail on the port in use; removing and adding a subscription on one channel is
        // an ordinary sequence no client should retry around, so the add waits, with
        // nothing registered and no event sent, until the receiver's `EndpointClosed`
        // reply frees the slot and `take_receiver_replies` runs it again as it arrived:
        // one receiver cycle while the receiver keeps up, and at most the closing
        // endpoint wait, after which `time_out_pending_subscriptions` answers it with
        // code 4 inside the client's response timeout (ADR 0049).
        if open.is_none()
            && (leaving
                || self.receive_endpoints.iter().any(|entry| {
                    entry.endpoint == endpoint
                        && entry.interface == interface
                        && entry.state == ChannelState::Closing
                }))
        {
            self.pending_subscriptions.push(PendingSubscription {
                client_id,
                correlation_id,
                stream_id,
                channel: raw.to_vec(),
                endpoint,
                interface,
                arrived: now_nanos(),
            });
            self.statics.subscription_add_deferred.increment();
            self.count_waiting_adds();
            return;
        }
        // Room in the receiver's queue for an endpoint, an interest and a reader per
        // live image on the stream, else code 10.
        let joins = match open {
            Some(index) => {
                let endpoint_id = self.receive_endpoints[index].registration_id;
                self.udp_images
                    .iter()
                    .filter(|image| {
                        image.endpoint_id == endpoint_id
                            && image.stream_id == stream_id
                            && image.state == ImageState::Live
                            && !image.leaving
                    })
                    .count()
            }
            None => 0,
        };
        if !self.receiver_deferred.is_empty()
            || self.receiver.requests.capacity() - self.receiver.requests.len() < 2 + joins
        {
            self.receiver_queue_full(correlation_id);
            return;
        }
        let now = now_nanos();
        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
        let (endpoint_index, socket) = match open {
            Some(index) => (index, None),
            None => {
                if self.receive_endpoints.len() >= spec::MAX_RECEIVE_ENDPOINTS {
                    self.registry_full(
                        correlation_id,
                        "receive endpoints",
                        spec::MAX_RECEIVE_ENDPOINTS,
                    );
                    return;
                }
                let socket = match ReceiveSocket::open(endpoint) {
                    Ok(socket) => socket,
                    Err(error) => {
                        self.send_error(
                            correlation_id,
                            ErrorCode::ChannelEndpointError,
                            &format!(
                                "the channel's endpoint {endpoint}: the socket could not be \
                                 opened or bound: {error}; check that the address is this host's \
                                 and its port is free; see {ERRORS_DOC}"
                            ),
                        );
                        return;
                    }
                };
                let resolved = match interface {
                    Some(interface) => {
                        format!("cavalry:udp?endpoint={endpoint}|interface={interface}")
                    }
                    None => format!("cavalry:udp?endpoint={endpoint}"),
                };
                let key = correlation_id.to_le_bytes();
                let label = |name: &str| format!("{name} {resolved}");
                let mut allocated: Vec<Counter> = Vec::with_capacity(5);
                for (id, name) in [
                    (CHANNEL_STATUS, "channel.status"),
                    (CHANNEL_SOCKET_RCVBUF, "channel.socket_rcvbuf"),
                    (CHANNEL_SOCKET_SNDBUF, "channel.socket_sndbuf"),
                    (CHANNEL_IMAGES, "channel.images"),
                    (CHANNEL_PUBLICATIONS, "channel.publications"),
                ] {
                    match self.counters.allocate(id, &key, &label(name)) {
                        Ok(counter) => allocated.push(counter),
                        Err(error) => {
                            for counter in allocated {
                                let _ = self.counters.free(counter.id(), reuse);
                            }
                            self.counters_full(correlation_id, &error);
                            return;
                        }
                    }
                }
                let mut counters = allocated.into_iter();
                let (
                    Some(status),
                    Some(socket_rcvbuf),
                    Some(socket_sndbuf),
                    Some(images),
                    Some(publications),
                ) = (
                    counters.next(),
                    counters.next(),
                    counters.next(),
                    counters.next(),
                    counters.next(),
                )
                else {
                    return;
                };
                let (rcvbuf, sndbuf) = socket.buffer_lengths();
                status.set(0);
                socket_rcvbuf.set(rcvbuf);
                socket_sndbuf.set(sndbuf);
                images.set(0);
                publications.set(0);
                self.receive_endpoints.push(ReceiveEndpoint {
                    registration_id: correlation_id,
                    endpoint,
                    interface,
                    channel: resolved,
                    state: ChannelState::Initializing,
                    status,
                    socket_rcvbuf,
                    socket_sndbuf,
                    images,
                    publications,
                    subscription_count: 0,
                    image_count: 0,
                });
                (self.receive_endpoints.len() - 1, Some(socket))
            }
        };
        let endpoint_id = self.receive_endpoints[endpoint_index].registration_id;
        // To the receiver: the endpoint first when it is new, then the interest; the
        // room was checked above, so neither offer fails.
        if let Some(socket) = socket {
            if self
                .receiver
                .requests
                .offer(receiver::Request::AddEndpoint {
                    registration_id: endpoint_id,
                    socket,
                    wanted: Vec::with_capacity(spec::MAX_SUBSCRIPTIONS),
                })
                .is_err()
            {
                self.statics.registry_full.increment();
            }
            let entry = &mut self.receive_endpoints[endpoint_index];
            entry.state = ChannelState::Active;
            entry.status.set(1);
            self.statics.channel_opened.increment();
        }
        if self
            .receiver
            .requests
            .offer(receiver::Request::AddInterest {
                endpoint_id,
                stream_id,
            })
            .is_err()
        {
            self.statics.registry_full.increment();
        }
        self.wake_receiver();
        self.receive_endpoints[endpoint_index].subscription_count += 1;
        let status_id = counter_id(&self.receive_endpoints[endpoint_index].status);
        self.subscriptions.push(Subscription {
            registration_id: correlation_id,
            client_id,
            stream_id,
            endpoint: Some(endpoint_id),
        });
        self.send(&Event::SubscriptionReady {
            correlation_id,
            channel_status_indicator_id: status_id,
        });
        // The images already live on the endpoint and stream: the subscription joins
        // each at its rebuild position. One its last subscription has left is being
        // retired, and is not joined (ADR 0068).
        let live: Vec<usize> = self
            .udp_images
            .iter()
            .enumerate()
            .filter(|(_, image)| {
                image.endpoint_id == endpoint_id
                    && image.stream_id == stream_id
                    && image.state == ImageState::Live
                    && !image.leaving
            })
            .map(|(index, _)| index)
            .collect();
        for index in live {
            self.join_udp_image(index, correlation_id, now);
        }
    }

    /// Gives subscription `subscription_id` a position on the image at `index` of
    /// `udp_images`, at the image's rebuild position: a `subscription.position` counter
    /// the client owns, `available image`, and a reader with the receiver. The images
    /// registry's bound holds over the IPC pairs and these together.
    fn join_udp_image(&mut self, index: usize, subscription_id: i64, now: i64) {
        if self.images.len() + self.udp_pairs >= spec::MAX_IMAGES {
            self.registry_full(subscription_id, "images", spec::MAX_IMAGES);
            return;
        }
        let image = &self.udp_images[index];
        let (image_id, session_id, stream_id) =
            (image.registration_id, image.session_id, image.stream_id);
        let join = image.rebuild.load_acquire();
        let log_file_name = image.log_file_name.clone();
        let source_identity = image.source_identity.clone();
        let key = position_key(image_id, session_id, stream_id);
        let label = format!("subscription.position {image_id} {session_id} {stream_id}");
        let position = match self.counters.allocate(SUBSCRIPTION_POSITION, &key, &label) {
            Ok(counter) => counter,
            Err(error) => {
                self.counters_full(subscription_id, &error);
                return;
            }
        };
        position.store_release(join);
        let event = Event::AvailableImage {
            correlation_id: image_id,
            session_id,
            stream_id,
            subscriber_registration_id: subscription_id,
            subscriber_position_counter_id: counter_id(&position),
            log_file_name: log_file_name.as_bytes(),
            source_identity: source_identity.as_bytes(),
        };
        let counter_record = position.id();
        self.send(&event);
        self.udp_images[index].subscriptions.push(UdpPair {
            subscription_id,
            record: counter_record,
            joined: join,
            announced: now,
        });
        self.udp_pairs += 1;
        self.receiver_request(receiver::Request::AddReader {
            registration_id: image_id,
            key: ReaderKey::Subscription(subscription_id),
            counter: Some(position),
            floor: join,
        });
    }

    /// Answers a command with error code 10 when the receiver's request queue has no
    /// room, or requests are still waiting for it.
    fn receiver_queue_full(&mut self, correlation_id: i64) {
        self.statics.registry_full.increment();
        self.send_error(
            correlation_id,
            ErrorCode::ResourceTemporarilyUnavailable,
            &format!(
                "the receiver's request queue holds {} and is full; try again once the \
                 receiver has caught up; see {ERRORS_DOC}",
                spec::RECEIVER_QUEUE_CAPACITY
            ),
        );
    }

    /// Offers a request to the receiver, or keeps it for the next cycle when the queue is
    /// full or earlier requests are still waiting, so that the receiver sees them in
    /// order; bounded by [`RECEIVER_DEFERRED_CAPACITY`], since an add that finds no room
    /// is refused instead.
    fn receiver_request(&mut self, request: receiver::Request) {
        let request = if self.receiver_deferred.is_empty() {
            match self.receiver.requests.offer(request) {
                Ok(()) => {
                    self.wake_receiver();
                    return;
                }
                Err(request) => request,
            }
        } else {
            request
        };
        debug_assert!(
            self.receiver_deferred.len() < RECEIVER_DEFERRED_CAPACITY,
            "the receiver's deferred requests passed their bound"
        );
        self.receiver_deferred.push_back(request);
    }

    /// Offers the receiver's deferred requests in order until the queue is full again.
    fn flush_receiver_deferred(&mut self) -> usize {
        let mut sent = 0;
        while let Some(request) = self.receiver_deferred.pop_front() {
            match self.receiver.requests.offer(request) {
                Ok(()) => sent += 1,
                Err(request) => {
                    self.receiver_deferred.push_front(request);
                    break;
                }
            }
        }
        if sent > 0 {
            self.wake_receiver();
        }
        sent
    }

    /// Takes every reply the receiver has queued and acts on it: an image created for a
    /// setup, a retired image's events and records, a removed reader's record freed, a
    /// rejected reader's client evicted, an endpoint's counters freed.
    fn take_receiver_replies(&mut self, now: i64) -> usize {
        let mut taken = 0;
        while let Some(reply) = self.receiver.replies.poll() {
            match reply {
                receiver::Reply::Setup {
                    endpoint_id,
                    source,
                    setup,
                } => self.on_setup(endpoint_id, source, setup, now),
                receiver::Reply::Retired {
                    registration_id,
                    state,
                } => self.on_image_retired(registration_id, state, now),
                receiver::Reply::ReaderRemoved {
                    registration_id,
                    key,
                } => {
                    if let ReaderKey::Subscription(subscription_id) = key
                        && let Some(image) = self
                            .udp_images
                            .iter_mut()
                            .find(|image| image.registration_id == registration_id)
                        && let Some(pair) = image
                            .subscriptions
                            .iter()
                            .position(|pair| pair.subscription_id == subscription_id)
                    {
                        let record = image.subscriptions.swap_remove(pair).record;
                        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
                        let _ = self.counters.free(record, reuse);
                        self.udp_pairs = self.udp_pairs.saturating_sub(1);
                    }
                }
                receiver::Reply::ReaderRejected {
                    registration_id,
                    key,
                } => {
                    // A subscriber's position outside its bound evicts its client, as
                    // one on a publication does (ADR 0010); the receiver counted it.
                    if let ReaderKey::Subscription(subscription_id) = key
                        && self
                            .udp_images
                            .iter()
                            .any(|image| image.registration_id == registration_id)
                        && let Some(client_id) = self
                            .subscriptions
                            .iter()
                            .find(|subscription| subscription.registration_id == subscription_id)
                            .map(|subscription| subscription.client_id)
                    {
                        self.remove_client(client_id, now);
                        self.send(&Event::ClientTimeout { client_id });
                    }
                }
                receiver::Reply::EndpointClosed { registration_id } => {
                    if let Some(index) = self
                        .receive_endpoints
                        .iter()
                        .position(|endpoint| endpoint.registration_id == registration_id)
                    {
                        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
                        let entry = self.receive_endpoints.swap_remove(index);
                        self.drop_armed_recordings(|armed| armed.endpoint_id == registration_id);
                        let (closed_endpoint, closed_interface) = (entry.endpoint, entry.interface);
                        for counter in [
                            entry.status,
                            entry.socket_rcvbuf,
                            entry.socket_sndbuf,
                            entry.images,
                            entry.publications,
                        ] {
                            let _ = self.counters.free(counter.id(), reuse);
                        }
                        // The adds that waited for this endpoint's close run now, in
                        // the order they arrived, each as if it had just arrived: the
                        // first binds the endpoint again (ADR 0049). They leave the
                        // list in one pass, which keeps the others in their order.
                        let mut due = Vec::with_capacity(self.pending_subscriptions.len());
                        due.extend(self.pending_subscriptions.extract_if(.., |pending| {
                            pending.endpoint == closed_endpoint
                                && pending.interface == closed_interface
                        }));
                        self.count_waiting_adds();
                        for pending in due {
                            self.add_subscription(
                                pending.client_id,
                                pending.correlation_id,
                                pending.stream_id,
                                &pending.channel,
                                now,
                            );
                        }
                    }
                }
            }
            taken += 1;
        }
        taken
    }

    /// A setup frame the receiver handed over (`spec/udp.md`, "The receiver", step 1):
    /// the image gets a registration id from the correlation id counter, a log of type
    /// 2 at the setup's position, its two counters, a `subscription.position` for every
    /// subscription on the endpoint and stream with `available image` to each, and goes
    /// to the receiver with its readers; a registry at capacity, a counters file with no
    /// free record or a log that cannot be created is a refusal back, so the sender's
    /// next setup tries again.
    fn on_setup(&mut self, endpoint_id: i64, source: SocketAddr, setup: SetupFrame, now: i64) {
        let refuse = receiver::Request::RefuseImage {
            endpoint_id,
            session_id: setup.session_id,
            stream_id: setup.stream_id,
        };
        let Some(endpoint_index) = self.receive_endpoints.iter().position(|endpoint| {
            endpoint.registration_id == endpoint_id && endpoint.state == ChannelState::Active
        }) else {
            self.receiver_request(refuse);
            return;
        };
        let subscribers: Vec<i64> = self
            .subscriptions
            .iter()
            .filter(|subscription| {
                subscription.endpoint == Some(endpoint_id)
                    && subscription.stream_id == setup.stream_id
            })
            .map(|subscription| subscription.registration_id)
            .collect();
        if self.udp_images.len() >= spec::MAX_IMAGES
            || self.images.len() + self.udp_pairs + subscribers.len() > spec::MAX_IMAGES
        {
            self.statics.registry_full.increment();
            self.receiver_request(refuse);
            return;
        }
        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
        let registration_id = self.correlation.next_correlation_id();
        let term_length = setup.term_length as u32;
        let start = image_start(
            setup.initial_term_id,
            setup.active_term_id,
            setup.term_offset,
            term_length,
        );
        let fields = HeaderFields::new_image(
            registration_id,
            setup.initial_term_id,
            term_length,
            setup.mtu_length as u32,
            start.receiver_window_length,
            setup.session_id,
            setup.stream_id,
        );
        let log = match fields
            .and_then(|fields| LogBuffers::create_at(&self.dir, &fields, start.position))
        {
            Ok(log) => log,
            Err(_) => {
                self.statics.log_create_failed.increment();
                self.receiver_request(refuse);
                return;
            }
        };
        let writer = match log.image_writer() {
            Ok(writer) => writer,
            Err(_) => {
                drop(log);
                remove_image_log(&self.dir, registration_id);
                self.statics.log_create_failed.increment();
                self.receiver_request(refuse);
                return;
            }
        };
        let _ = log.set_connected(true);
        let key = position_key(registration_id, setup.session_id, setup.stream_id);
        let label = |name: &str| {
            format!(
                "{name} {registration_id} {} {}",
                setup.session_id, setup.stream_id
            )
        };
        let hwm =
            match self
                .counters
                .allocate(IMAGE_HWM_POSITION, &key, &label("image.hwm_position"))
            {
                Ok(counter) => counter,
                Err(_) => {
                    drop(writer);
                    drop(log);
                    remove_image_log(&self.dir, registration_id);
                    self.statics.registry_full.increment();
                    self.receiver_request(refuse);
                    return;
                }
            };
        let rebuild = match self.counters.allocate(
            IMAGE_REBUILD_POSITION,
            &key,
            &label("image.rebuild_position"),
        ) {
            Ok(counter) => counter,
            Err(_) => {
                let _ = self.counters.free(hwm.id(), reuse);
                drop(writer);
                drop(log);
                remove_image_log(&self.dir, registration_id);
                self.statics.registry_full.increment();
                self.receiver_request(refuse);
                return;
            }
        };
        hwm.store_release(start.position);
        rebuild.store_release(start.position);
        let log_file_name = log_layout::image_file_name(registration_id)
            .to_string_lossy()
            .into_owned();
        let source_identity = source.to_string();
        let mut readers: Vec<(ReaderKey, Option<Counter>, i64)> =
            Vec::with_capacity(subscribers.len());
        let mut pairs: Vec<UdpPair> = Vec::with_capacity(subscribers.len());
        let mut announced: Vec<(i64, i32)> = Vec::with_capacity(subscribers.len());
        for subscription_id in subscribers {
            let label = format!(
                "subscription.position {registration_id} {} {}",
                setup.session_id, setup.stream_id
            );
            let position = match self.counters.allocate(SUBSCRIPTION_POSITION, &key, &label) {
                Ok(counter) => counter,
                Err(error) => {
                    self.counters_full(subscription_id, &error);
                    continue;
                }
            };
            position.store_release(start.position);
            pairs.push(UdpPair {
                subscription_id,
                record: position.id(),
                joined: start.position,
                announced: now,
            });
            announced.push((subscription_id, counter_id(&position)));
            readers.push((
                ReaderKey::Subscription(subscription_id),
                Some(position),
                start.position,
            ));
        }
        self.udp_pairs += pairs.len();
        self.udp_images.push(UdpImage {
            registration_id,
            endpoint_id,
            session_id: setup.session_id,
            stream_id: setup.stream_id,
            hwm: hwm.clone(),
            rebuild: rebuild.clone(),
            log_file_name: log_file_name.clone(),
            source_identity: source_identity.clone(),
            subscriptions: pairs,
            state: ImageState::Live,
            leaving: false,
            waiting: false,
        });
        let entry = &mut self.receive_endpoints[endpoint_index];
        entry.image_count += 1;
        entry
            .images
            .set(i64::try_from(entry.image_count).unwrap_or(i64::MAX));
        self.receiver_request(receiver::Request::AddImage(receiver::AddImage {
            registration_id,
            endpoint_id,
            session_id: setup.session_id,
            stream_id: setup.stream_id,
            source,
            log,
            writer,
            hwm,
            rebuild,
            start_position: start.position,
            readers,
        }));
        // An armed recording starts at the image's join position before the image is
        // announced, its start answered under the arming command's correlation id. A
        // refusal here is counted as failed at once; a start handed to the recorder is
        // counted where the recorder's answer is taken, as started with `recording
        // ready` (`on_ready`) or as failed (`fail_start`, `on_start_failed`) (ADR 0059).
        if let Some(index) = self.armed_recordings.iter().position(|armed| {
            armed.endpoint_id == endpoint_id && armed.stream_id == setup.stream_id
        }) {
            let armed = self.armed_recordings.swap_remove(index);
            let channel = self.receive_endpoints[endpoint_index].channel.clone();
            if !self.start_image_recording(
                armed.correlation_id,
                registration_id,
                start.position,
                channel,
                true,
                now,
            ) {
                self.statics.recording_armed_failed.increment();
            }
        }
        for (subscription_id, position_id) in announced {
            self.send(&Event::AvailableImage {
                correlation_id: registration_id,
                session_id: setup.session_id,
                stream_id: setup.stream_id,
                subscriber_registration_id: subscription_id,
                subscriber_position_counter_id: position_id,
                log_file_name: log_file_name.as_bytes(),
                source_identity: source_identity.as_bytes(),
            });
        }
    }

    /// The receiver retired an image (`spec/udp.md`, "The receiver", step 5):
    /// `unavailable image` to the subscriptions that remain and their position records
    /// freed; then, once a recording of the image has stopped, at the rebuild
    /// position the retire left (`stop_recordings_of_retired_images`), the image's two
    /// counters freed, its log unlinked, `image.removed`, and the endpoint closed when
    /// nothing holds it (`remove_udp_image`). A subscription that remains and may not
    /// have read its `available image` yet (`pair_waits`) keeps its record, and the
    /// image its log, until that subscription waits no longer
    /// (`remove_retired_images`; ADR 0078).
    fn on_image_retired(&mut self, registration_id: i64, state: ImageState, now: i64) {
        let Some(index) = self
            .udp_images
            .iter()
            .position(|image| image.registration_id == registration_id)
        else {
            return;
        };
        self.udp_images[index].state = state;
        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
        let (endpoint_id, stream_id) = (
            self.udp_images[index].endpoint_id,
            self.udp_images[index].stream_id,
        );
        let channel = self
            .receive_endpoints
            .iter()
            .find(|endpoint| endpoint.registration_id == endpoint_id)
            .map(|endpoint| endpoint.channel.clone())
            .unwrap_or_default();
        let mut pairs = std::mem::take(&mut self.udp_images[index].subscriptions);
        for pair in &pairs {
            self.send(&Event::UnavailableImage {
                correlation_id: registration_id,
                subscriber_registration_id: pair.subscription_id,
                stream_id,
                channel: channel.as_bytes(),
            });
        }
        // A record whose subscription may not have read the announcement yet stays, and
        // goes with the log; every other record is freed now, as before.
        pairs.retain(|pair| {
            let waits = self.pair_waits(pair, now);
            if !waits {
                let _ = self.counters.free(pair.record, reuse);
                self.udp_pairs = self.udp_pairs.saturating_sub(1);
            }
            waits
        });
        if !pairs.is_empty() {
            self.udp_images[index].subscriptions = pairs;
            self.udp_images[index].waiting = true;
            self.udp_waiting += 1;
        }
        // A recorded image's log waits for its recording to stop, as a recorded
        // publication's does (`spec/log-buffer.md`, "Closing"): the stop goes to the
        // recorder once it has written up to the rebuild position
        // (`stop_recordings_of_retired_images`), and `on_stopped` finishes.
        let recorded = self
            .recordings
            .iter()
            .any(|recording| recording.publication_id == registration_id);
        if !recorded && !self.udp_images[index].waiting {
            self.remove_udp_image(registration_id, now);
        }
    }

    /// A recording of a retired image stops at the image's rebuild position, where the
    /// receiver's last scan left it and nothing is inserted after, once the recorder
    /// has written up to it (`spec/udp.md`, "The receiver", step 5; ADR 0042): each
    /// cycle, a retired image with a live recording whose position has reached the
    /// rebuild position hands the recorder a stop with no correlation id, which stops
    /// at the position written, the rebuild position itself, since the recorder's
    /// reader stops at the gap or the high-water mark there; the log stays until the
    /// stop's reply (`on_stopped`, `drop_recording_reader`). The log's end of stream
    /// position is not written for it: a retire for the liveness timeout or the last
    /// subscription's removal is not the stream's end, and a subscriber reading the
    /// log must not be told one (invariant 1). An image whose stream ended has its end
    /// of stream position stored, where the recorder stops by itself; a stop asked
    /// here that finds it stopped is answered with `StopIgnored` and nothing follows.
    /// A recording still starting waits for its id. Returns how many stops went.
    fn stop_recordings_of_retired_images(&mut self) -> usize {
        let mut stops = 0;
        for index in 0..self.udp_images.len() {
            let (registration_id, rebuild) = {
                let image = &self.udp_images[index];
                if image.state == ImageState::Live {
                    continue;
                }
                (image.registration_id, image.rebuild.load_acquire())
            };
            let Some(recording) = self
                .recordings
                .iter()
                .position(|recording| recording.publication_id == registration_id)
            else {
                continue;
            };
            let entry = &self.recordings[recording];
            if entry.stop_requested || entry.state != RecordingState::Live {
                continue;
            }
            let Some(recording_id) = entry.recording_id else {
                continue;
            };
            let position = entry
                .position
                .as_ref()
                .map_or(entry.start_position, Counter::load_acquire);
            if position < rebuild {
                continue;
            }
            self.recordings[recording].stop_requested = true;
            self.request(Request::Stop {
                recording_id,
                correlation_id: None,
            });
            stops += 1;
        }
        stops
    }

    /// Finishes a retired image: its two counters freed after the reuse timeout, its
    /// log unlinked, `image.removed`, the endpoint's image count down and the endpoint
    /// closed when nothing holds it. Records the image kept for subscriptions that
    /// waited (`on_image_retired`) are freed here, with the log.
    fn remove_udp_image(&mut self, registration_id: i64, now: i64) {
        let Some(index) = self.udp_images.iter().position(|image| {
            image.registration_id == registration_id && image.state != ImageState::Live
        }) else {
            return;
        };
        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
        let mut image = self.udp_images.swap_remove(index);
        for pair in image.subscriptions.drain(..) {
            let _ = self.counters.free(pair.record, reuse);
            self.udp_pairs = self.udp_pairs.saturating_sub(1);
        }
        if image.waiting {
            image.waiting = false;
            self.udp_waiting = self.udp_waiting.saturating_sub(1);
        }
        let _ = self.counters.free(image.hwm.id(), reuse);
        let _ = self.counters.free(image.rebuild.id(), reuse);
        remove_image_log(&self.dir, registration_id);
        image.state = ImageState::Removed;
        self.statics.image_removed.increment();
        if let Some(entry) = self
            .receive_endpoints
            .iter_mut()
            .find(|endpoint| endpoint.registration_id == image.endpoint_id)
        {
            entry.image_count = entry.image_count.saturating_sub(1);
            entry
                .images
                .set(i64::try_from(entry.image_count).unwrap_or(i64::MAX));
        }
        self.release_receive_endpoint(image.endpoint_id);
    }

    /// Whether `pair`'s subscription may still be to open what its `available image`
    /// named: the subscription is registered, its announcement is less than the
    /// counter reuse timeout old, and its position stands where it joined
    /// ([`announcement_waits`]). A record that is not allocated waits for nothing.
    fn pair_waits(&self, pair: &UdpPair, now: i64) -> bool {
        self.subscriptions
            .iter()
            .any(|subscription| subscription.registration_id == pair.subscription_id)
            && self.counters.counter(pair.record).is_ok_and(|position| {
                announcement_waits(pair.announced, pair.joined, position.load_acquire(), now)
            })
    }

    /// Removes the retired images that were `waiting` and wait no longer: none of
    /// their kept subscriptions may still be to open the log, and no recording holds
    /// it. Each duty cycle while an image waits, by the duty cycle's clock, since
    /// nothing else comes back for one. Returns how many went.
    fn remove_retired_images(&mut self, now: i64) -> usize {
        let mut removed = 0;
        let mut index = 0;
        while index < self.udp_images.len() {
            let image = &self.udp_images[index];
            let registration_id = image.registration_id;
            let due = image.waiting
                && !image
                    .subscriptions
                    .iter()
                    .any(|pair| self.pair_waits(pair, now))
                && !self
                    .recordings
                    .iter()
                    .any(|recording| recording.publication_id == registration_id);
            if due {
                // The last image takes this place: look at it next.
                self.remove_udp_image(registration_id, now);
                removed += 1;
            } else {
                index += 1;
            }
        }
        removed
    }

    /// Closes a receive endpoint left with no subscription and no image: `channel.status`
    /// 2, `channel.closed`, the socket to drop with the receiver, and the counters freed
    /// when it has (`spec/udp.md`, "The receiver", the endpoints).
    fn release_receive_endpoint(&mut self, endpoint_id: i64) {
        let Some(entry) = self
            .receive_endpoints
            .iter_mut()
            .find(|endpoint| endpoint.registration_id == endpoint_id)
        else {
            return;
        };
        if entry.subscription_count > 0
            || entry.image_count > 0
            || entry.state != ChannelState::Active
        {
            return;
        }
        entry.state = ChannelState::Closing;
        entry.status.set(2);
        self.statics.channel_closed.increment();
        self.receiver_request(receiver::Request::RemoveEndpoint {
            registration_id: endpoint_id,
        });
    }

    fn remove_subscription(
        &mut self,
        client_id: i64,
        correlation_id: i64,
        registration_id: i64,
        now: i64,
    ) {
        let found = self.subscriptions.iter().position(|subscription| {
            subscription.registration_id == registration_id && subscription.client_id == client_id
        });
        let Some(index) = found else {
            self.send_error(
                correlation_id,
                ErrorCode::UnknownSubscription,
                &format!("registration {registration_id} is not a subscription of client {client_id}; see {ERRORS_DOC}"),
            );
            return;
        };
        self.drop_subscription(index, now);
        self.send(&Event::OperationSuccess { correlation_id });
    }

    /// Removes the subscription at `index` and every image it has, freeing their
    /// counters; over UDP the receiver drops its readers, whose records are freed on its
    /// replies, retires an image left with none, and the endpoint closes when nothing
    /// holds it.
    fn drop_subscription(&mut self, index: usize, now: i64) {
        let subscription = self.subscriptions.swap_remove(index);
        if let Some(endpoint_id) = subscription.endpoint {
            // The images this was the last subscription to read: the receiver retires
            // each on the request below, and until it has said so they are given to
            // nothing that arrives (ADR 0068). A reader whose removal is still on its
            // way back is of a subscription already gone from the registry.
            for image in &mut self.udp_images {
                if image.endpoint_id == endpoint_id
                    && image.stream_id == subscription.stream_id
                    && image.state == ImageState::Live
                    && image
                        .subscriptions
                        .iter()
                        .any(|pair| pair.subscription_id == subscription.registration_id)
                    && !image.subscriptions.iter().any(|pair| {
                        self.subscriptions
                            .iter()
                            .any(|held| held.registration_id == pair.subscription_id)
                    })
                {
                    image.leaving = true;
                }
            }
            // Its record on an image already retired, kept in case its client had not
            // read the announcement (ADR 0078), is freed now: the receiver holds no
            // reader for it and sends no reply.
            let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
            for image in self.udp_images.iter_mut().filter(|image| image.waiting) {
                image.subscriptions.retain(|pair| {
                    let kept = pair.subscription_id != subscription.registration_id;
                    if !kept {
                        let _ = self.counters.free(pair.record, reuse);
                        self.udp_pairs = self.udp_pairs.saturating_sub(1);
                    }
                    kept
                });
            }
            self.receiver_request(receiver::Request::RemoveInterest {
                endpoint_id,
                stream_id: subscription.stream_id,
                subscription_id: subscription.registration_id,
            });
            if let Some(entry) = self
                .receive_endpoints
                .iter_mut()
                .find(|endpoint| endpoint.registration_id == endpoint_id)
            {
                entry.subscription_count = entry.subscription_count.saturating_sub(1);
            }
            self.release_receive_endpoint(endpoint_id);
            // The stream's last subscription on the endpoint takes its armed recording
            // with it (ADR 0059).
            let stream_id = subscription.stream_id;
            if !self
                .subscriptions
                .iter()
                .any(|other| other.endpoint == Some(endpoint_id) && other.stream_id == stream_id)
            {
                self.drop_armed_recordings(|armed| {
                    armed.endpoint_id == endpoint_id && armed.stream_id == stream_id
                });
            }
        }
        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
        let mut kept = Vec::with_capacity(self.images.len());
        for image in self.images.drain(..) {
            if image.subscription_id == subscription.registration_id {
                let _ = self.counters.free(image.position.id(), reuse);
            } else {
                kept.push(image);
            }
        }
        self.images = kept;
        self.images
            .reserve(spec::MAX_IMAGES.saturating_sub(self.images.len()));
        // A publication whose last image went is not connected from here, before the
        // limit pass lowers its limit (ADR 0048).
        for index in 0..self.publications.len() {
            self.lower_connected(index);
        }
    }

    /// Client close and client timeout: every registration, subscription and counter of
    /// the client goes, as if removed, a counter with its `unavailable counter` and no
    /// `operation success`, and the client is forgotten.
    fn remove_client(&mut self, client_id: i64, now: i64) {
        self.clients.retain(|client| client.id != client_id);
        while let Some(index) = self
            .client_counters
            .iter()
            .position(|counter| counter.client_id == client_id)
        {
            let counter = self.client_counters.remove(index);
            self.release_client_counter(&counter, now);
        }
        // Its adds still waiting for a closing endpoint go with it (ADR 0049).
        self.pending_subscriptions
            .retain(|pending| pending.client_id != client_id);
        self.count_waiting_adds();
        let registrations: Vec<(usize, i64)> = self
            .publications
            .iter()
            .enumerate()
            .flat_map(|(index, publication)| {
                publication
                    .registrations
                    .iter()
                    .filter(|registration| registration.client_id == client_id)
                    .map(move |registration| (index, registration.correlation_id))
            })
            .collect();
        for (index, registration_id) in registrations {
            self.drop_registration(index, registration_id, now);
        }
        while let Some(index) = self
            .subscriptions
            .iter()
            .position(|subscription| subscription.client_id == client_id)
        {
            self.drop_subscription(index, now);
        }
    }

    /// Times out every client whose last command is older than the liveness timeout.
    fn time_out_clients(&mut self, now: i64) -> usize {
        let timeout = spec::nanos(spec::CLIENT_LIVENESS_TIMEOUT);
        let stale: Vec<i64> = self
            .clients
            .iter()
            .filter(|client| now.saturating_sub(client.last_seen) > timeout)
            .map(|client| client.id)
            .collect();
        for client_id in &stale {
            self.remove_client(*client_id, now);
            self.statics.client_timed_out.increment();
            self.send(&Event::ClientTimeout {
                client_id: *client_id,
            });
        }
        stale.len()
    }

    /// The driver's duties on every publication (`spec/log-buffer.md`, "The driver"):
    /// positions, cleaning, the limit, unblocking, and closing once the linger has passed
    /// or every subscriber has reached the end of stream. The producer position is loaded
    /// at most once every two milliseconds per publication, and its move is not work
    /// (ADR 0022, ADR 0032).
    fn keep_publications(&mut self, now: i64) -> usize {
        let mut work = 0;
        let mut index = 0;
        while index < self.publications.len() {
            if self.keep_publication(index, now) {
                work += 1;
            }
            if self.close_if_done(index, now) {
                work += 1;
            } else {
                index += 1;
            }
        }
        work
    }

    /// One publication's duties; returns whether anything moved.
    fn keep_publication(&mut self, index: usize, now: i64) -> bool {
        let max_position = self.publications[index].log.positions().max_position();
        let publication_id = self.publications[index].registration_id;
        // The subscribers' positions, each checked against its bound (ADR 0010): one
        // outside it evicts its client, told with client timeout, the event the page
        // gives for registrations gone.
        let mut min_subscriber: Option<i64> = None;
        let mut max_subscriber: Option<i64> = None;
        let mut rejected: Vec<i64> = Vec::new();
        for image in self
            .images
            .iter()
            .filter(|image| image.publication_id == publication_id)
        {
            let position = image.position.load_acquire();
            if position < 0 || position > max_position || position % FRAME_ALIGN != 0 {
                rejected.push(image.subscription_id);
                continue;
            }
            min_subscriber = Some(min_subscriber.map_or(position, |min: i64| min.min(position)));
            max_subscriber = Some(max_subscriber.map_or(position, |max: i64| max.max(position)));
        }
        for subscription_id in rejected {
            self.statics.log_buffer_rejected_position.increment();
            let client = self
                .subscriptions
                .iter()
                .find(|subscription| subscription.registration_id == subscription_id)
                .map(|subscription| subscription.client_id);
            if let Some(client_id) = client {
                self.remove_client(client_id, now);
                self.send(&Event::ClientTimeout { client_id });
            }
        }
        // The recorder's position, one more reader the limit and the cleaning respect:
        // its counter once attached, the start position until then (ADR 0025).
        let recording_floor = self
            .recordings
            .iter()
            .find(|recording| recording.publication_id == publication_id)
            .map(|recording| {
                recording
                    .position
                    .as_ref()
                    .map_or(recording.start_position, Counter::load_acquire)
            });
        let publication = &mut self.publications[index];
        // The producer position at most once every two milliseconds (ADR 0022, ADR 0032):
        // the publisher
        // stores to that line on every commit, and a load on every cycle kept the line
        // moving between the two caches, which the publisher paid at p50 and p99.99.
        // Closing loads it afresh (`drop_registration`), so the end of stream is exact.
        let due = now.saturating_sub(publication.producer_loaded_at)
            >= spec::nanos(spec::PRODUCER_POSITION_INTERVAL);
        if due {
            match publication.log.producer_position() {
                Ok(position) => {
                    publication.producer = position;
                    publication.producer_loaded_at = now;
                }
                Err(_) => {
                    self.statics.log_buffer_rejected_tail.increment();
                    if publication.closing.is_none() {
                        let end = publication.consumer_position;
                        let _ = publication.log.set_end_of_stream_position(end);
                        publication.closing = Some((end, now));
                        let registrations = publication.registrations.len();
                        publication.registrations.clear();
                        self.registration_count =
                            self.registration_count.saturating_sub(registrations);
                        let registration_id = publication.registration_id;
                        if let Some(udp) = &mut publication.udp
                            && !udp.close_sent
                        {
                            udp.close_sent = true;
                            self.sender_request(sender::Request::Close {
                                registration_id,
                                end_of_stream_position: end,
                            });
                        }
                    }
                    return true;
                }
            }
        }
        let producer = publication.producer;
        let mut moved = false;
        if publication.position.get() != producer {
            // Not work: a publisher that publishes while nothing else moves lets the
            // conductor idle, and the gauge is at most two milliseconds behind (ADR 0022,
            // ADR 0032).
            publication.position.store_release(producer);
        }
        if let Some(max) = max_subscriber
            && max > publication.consumer_position
        {
            publication.consumer_position = max;
            moved = true;
        }
        // The consumer position bounds everything below: the slowest subscriber, or the
        // recorder when it is slower (`spec/log-buffer.md`, "The driver"); with neither,
        // the largest position any subscriber reached, so that the publisher is told not
        // connected.
        let (connected, consumer, clean_to) = match &publication.udp {
            // Over UDP the sender position is the consumer position: the limit runs a
            // window ahead of it while a receiver answers, and the log is cleaned a term
            // behind it, Aeron's rule, so that the resend window stays (`spec/udp.md`,
            // "The sender"; ADR 0041).
            Some(udp) => {
                let sender_position = udp.sender_position.load_acquire();
                if sender_position > publication.consumer_position {
                    publication.consumer_position = sender_position;
                    moved = true;
                }
                (
                    udp.connected,
                    sender_position,
                    sender_position.saturating_sub(udp.term_length),
                )
            }
            None => {
                let floor = match (min_subscriber, recording_floor) {
                    (Some(subscriber), Some(recorder)) => Some(subscriber.min(recorder)),
                    (subscriber, recorder) => subscriber.or(recorder),
                };
                let consumer = floor.unwrap_or(publication.consumer_position);
                (floor.is_some(), consumer, consumer)
            }
        };
        // Cleaning behind the slowest subscriber, one term at a time; a clean that does
        // not advance is not work.
        if publication.clean_position < clean_to
            && let Ok(cleaned) = publication.log.clean(publication.clean_position, clean_to)
            && cleaned > publication.clean_position
        {
            publication.clean_position = cleaned;
            moved = true;
        }
        // The limit: consumer + window once cleaned, raised in steps of window ÷ 8; the
        // consumer position alone with neither a subscriber nor a recording, so the
        // publisher is told not connected. The window is the log's own, from its header:
        // half this conductor's term for a publication's log, and half the recording's
        // term for a replay's, which is the same number in this version and the right
        // one if the two ever differ, since rotation into a partition a subscriber still
        // reads is what the window forbids.
        let window = i64::from(publication.log.header().publication_window_length());
        let candidate = if connected {
            consumer.saturating_add(window)
        } else {
            consumer
        };
        let step = window / 8;
        let raise = candidate > publication.last_limit
            && (candidate - publication.last_limit >= step || !connected)
            && publication.clean_position >= clean_to;
        let lower = candidate < publication.last_limit && !connected;
        // The flag the publisher reads, stored only when it changes, as the limit is
        // (ADR 0048; card 187). It falls in the commands pass, where the last
        // subscriber, receiver or recording goes (`lower_connected`), so a publisher at
        // the old limit or the lowered one is told `not connected`; this 0 is a
        // backstop, and it too comes before the lowering.
        if !connected && publication.connected_flag {
            let _ = publication.log.set_connected(false);
            publication.connected_flag = false;
            moved = true;
        }
        if raise || lower {
            publication.limit.store_release(candidate);
            publication.last_limit = candidate;
            moved = true;
        }
        // It rises after the limit's raise, once the limit is past the consumer
        // position, which the first raise after a connection leaves it, and holds while
        // the publication stays connected: a subscriber that reaches the limit while
        // cleaning holds the next raise back still has its position, and a claim at the
        // limit reads `back pressured` for that cycle with the subscriber at the limit.
        // A publisher that loads the flag before the limit and reads 1 reads the raised
        // limit; a claim loads the limit first, so one whose two loads straddle these
        // two stores reads the old limit with a 1, and `back pressured`, once.
        if connected && !publication.connected_flag && publication.last_limit > consumer {
            let _ = publication.log.set_connected(true);
            publication.connected_flag = true;
            moved = true;
        }
        // Unblocking: a consumer position stalled for the timeout with the producer beyond it.
        if producer > consumer {
            match publication.stalled {
                Some((stalled_at, since)) if stalled_at == consumer => {
                    if now.saturating_sub(since) >= spec::nanos(spec::PUBLICATION_UNBLOCK_TIMEOUT) {
                        match publication.log.unblock(consumer) {
                            Ok(LogUnblock::Unblocked | LogUnblock::UnblockedToEnd) => {
                                self.statics.publication_unblocked.increment();
                            }
                            Ok(LogUnblock::Rotated | LogUnblock::NoAction) | Err(_) => {}
                        }
                        publication.stalled = Some((consumer, now));
                        moved = true;
                    }
                }
                _ => publication.stalled = Some((consumer, now)),
            }
        } else {
            publication.stalled = None;
        }
        moved
    }

    /// Closes the publication at `index` once every subscriber has reached the end of
    /// stream or the linger has passed: unavailable image events, then counters freed
    /// and the log removed, which wait for an image that a client may not have opened
    /// yet (ADR 0078). A `cavalry:udp` publication waits for the sender's closed reply
    /// first, and its `sender.position` and `sender.limit` are freed with its own two,
    /// since the sender dropped its handles before it replied. Returns whether it was
    /// removed.
    fn close_if_done(&mut self, index: usize, now: i64) -> bool {
        let Some((end, since)) = self.publications[index].closing else {
            return false;
        };
        if let Some(udp) = &self.publications[index].udp
            && !udp.sender_closed
        {
            return false;
        }
        let publication_id = self.publications[index].registration_id;
        // A recorded publication's log stays until its recording has stopped: the
        // recorder reaches the end of stream, at most a window away, and the catalog
        // holds the stop (`spec/log-buffer.md`, "Closing"; ADR 0025).
        if self
            .recordings
            .iter()
            .any(|recording| recording.publication_id == publication_id)
        {
            return false;
        }
        let all_reached = self
            .images
            .iter()
            .filter(|image| image.publication_id == publication_id)
            .all(|image| image.position.load_acquire() >= end);
        let lingered = now.saturating_sub(since) >= spec::nanos(spec::PUBLICATION_LINGER);
        if !all_reached && !lingered {
            return false;
        }
        // `unavailable image` goes now, when it always has, and once.
        if !self.publications[index].withdrawn {
            self.publications[index].withdrawn = true;
            let stream_id = self.publications[index].stream_id;
            for image in 0..self.images.len() {
                if self.images[image].publication_id == publication_id {
                    let event = Event::UnavailableImage {
                        correlation_id: publication_id,
                        subscriber_registration_id: self.images[image].subscription_id,
                        stream_id,
                        channel: IPC_CHANNEL,
                    };
                    self.send(&event);
                }
            }
        }
        // The log and the position counters stay while a client may still be to open
        // what an `available image` named: the counter reuse timeout after the event,
        // for an image whose position stands where it joined (`spec/log-buffer.md`,
        // "Closing"; ADR 0078). An image whose subscription was removed is no longer
        // listed, so a subscriber that has read, and one that leaves, make nothing
        // wait.
        if self.images.iter().any(|image| {
            image.publication_id == publication_id
                && announcement_waits(
                    image.announced,
                    image.joined,
                    image.position.load_acquire(),
                    now,
                )
        }) {
            return false;
        }
        let publication = self.publications.swap_remove(index);
        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
        let images = std::mem::take(&mut self.images);
        let mut kept = Vec::with_capacity(spec::MAX_IMAGES);
        for image in images {
            if image.publication_id == publication_id {
                let _ = self.counters.free(image.position.id(), reuse);
            } else {
                kept.push(image);
            }
        }
        self.images = kept;
        self.images
            .reserve(spec::MAX_IMAGES.saturating_sub(self.images.len()));
        let _ = self.counters.free(publication.position.id(), reuse);
        let _ = self.counters.free(publication.limit.id(), reuse);
        drop(publication.log);
        remove_log(&self.dir, publication_id);
        if let Some(udp) = publication.udp {
            let _ = self.counters.free(udp.sender_position.id(), reuse);
            let _ = self.counters.free(udp.sender_limit_id, reuse);
            let _ = self.counters.free(udp.receivers_id, reuse);
            self.release_send_endpoint(udp.endpoint_id, reuse);
        }
        true
    }

    /// One fewer publication on the endpoint; the last one's close closes the endpoint:
    /// `channel.status` 2, `channel.closed`, the socket to drop with the sender, and the
    /// counters freed when it has (`spec/udp.md`, "The receiver", the endpoints).
    fn release_send_endpoint(&mut self, endpoint_id: i64, _reuse: i64) {
        let Some(index) = self
            .send_endpoints
            .iter()
            .position(|endpoint| endpoint.registration_id == endpoint_id)
        else {
            return;
        };
        let endpoint = &mut self.send_endpoints[index];
        endpoint.publication_count = endpoint.publication_count.saturating_sub(1);
        endpoint
            .publications
            .set(i64::try_from(endpoint.publication_count).unwrap_or(i64::MAX));
        if endpoint.publication_count > 0 || endpoint.state != ChannelState::Active {
            return;
        }
        endpoint.state = ChannelState::Closing;
        endpoint.status.set(2);
        self.statics.channel_closed.increment();
        self.sender_request(sender::Request::RemoveEndpoint {
            registration_id: endpoint_id,
        });
    }

    /// The channel check every command that carries one runs first (`spec/udp.md`,
    /// "What a reader rejects"; ADR 0040): `cavalry:ipc` passes; a `cavalry:udp` channel
    /// that parses passes, every command taking one since `start replay` does (ADR
    /// 0041, ADR 0042, ADR 0043); a channel that fails the grammar, names a parameter
    /// this version does not, has a bad endpoint or a multicast address is code 1 with
    /// the reason, counted in `client_protocol.rejected_command`. Returns the channel
    /// when the command goes on.
    fn check_channel<'a>(
        &mut self,
        correlation_id: i64,
        channel: &'a [u8],
        what: &str,
    ) -> Option<Channel<'a>> {
        match Channel::parse(channel) {
            Ok(channel) => Some(channel),
            Err(error) => {
                self.statics.rejected_command.increment();
                let message = if let ChannelError::Key { .. } = error {
                    format!(
                        "{what}: {error}; the parameter is `{}`; see {ERRORS_DOC}",
                        String::from_utf8_lossy(error.key(channel))
                    )
                } else {
                    format!("{what}: {error}; see {ERRORS_DOC}")
                };
                self.send_error(correlation_id, ErrorCode::InvalidChannel, &message);
                None
            }
        }
    }

    /// `start recording` (`spec/client-protocol.md`, "Commands"): of the live
    /// publications on the stream, the one with the lowest registration id that is not
    /// yet being recorded; none live is code 20, every one recorded is code 22, the
    /// registry or the recorder's queue full is code 10. The recording starts at the
    /// producer position loaded afresh, the limit respects it from now on, and the
    /// recorder is asked for the catalog slot; `recording ready` follows its reply.
    fn start_recording(&mut self, correlation_id: i64, stream_id: i32, channel: &[u8], now: i64) {
        let Some(parsed) = self.check_channel(correlation_id, channel, "the channel") else {
            return;
        };
        // Over UDP the logs that match are the publications sending on the resolved
        // channel and the images received on it (`spec/udp.md`, "The receiver"; ADR
        // 0042), the lowest registration id not yet recorded first, as over IPC.
        let udp = match parsed {
            Channel::Ipc => None,
            Channel::Udp(udp) => match self.resolve_target(correlation_id, &udp) {
                Some(resolved) => Some(resolved),
                None => return,
            },
        };
        if let Some(target) = udp {
            self.start_udp_recording(correlation_id, stream_id, target, now);
            return;
        }
        let mut live = false;
        let mut lowest: Option<usize> = None;
        let mut chosen: Option<usize> = None;
        for (index, publication) in self.publications.iter().enumerate() {
            if publication.stream_id != stream_id
                || publication.closing.is_some()
                || publication.udp.is_some()
            {
                continue;
            }
            live = true;
            let lower = |best: Option<usize>| {
                best.is_none_or(|best| {
                    self.publications[best].registration_id > publication.registration_id
                })
            };
            if lower(lowest) {
                lowest = Some(index);
            }
            let recorded = self
                .recordings
                .iter()
                .any(|recording| recording.publication_id == publication.registration_id);
            if !recorded && lower(chosen) {
                chosen = Some(index);
            }
        }
        if !live {
            self.send_error(
                correlation_id,
                ErrorCode::NoPublicationToRecord,
                &format!(
                    "no live publication on stream {stream_id} and channel cavalry:ipc; add the \
                     publication first; see {ERRORS_DOC}"
                ),
            );
            return;
        }
        let Some(index) = chosen else {
            let recording = lowest.and_then(|index| {
                let publication_id = self.publications[index].registration_id;
                self.recordings
                    .iter()
                    .find(|recording| recording.publication_id == publication_id)
                    .and_then(|recording| {
                        recording
                            .recording_id
                            .map(|id| (id, recording.stop_requested))
                    })
            });
            let text = match recording {
                Some((id, true)) => format!(
                    "stream {stream_id} is stopping recording {id}; start again once its \
                     recording stopped arrives; see {ERRORS_DOC}"
                ),
                Some((id, false)) => format!(
                    "stream {stream_id} is already recording {id}; stop recording {id} first; \
                     see {ERRORS_DOC}"
                ),
                None => format!(
                    "stream {stream_id} is already starting a recording; wait for its \
                     recording ready; see {ERRORS_DOC}"
                ),
            };
            self.send_error(correlation_id, ErrorCode::AlreadyRecording, &text);
            return;
        };
        if self.recordings.len() >= spec::MAX_RECORDINGS {
            self.registry_full(correlation_id, "recordings", spec::MAX_RECORDINGS);
            return;
        }
        // Requests still waiting for room go first, and none of a new recording's join
        // them: this is what bounds the deferred list (`Conductor::deferred`).
        if !self.deferred.is_empty() {
            self.recorder_queue_full(correlation_id);
            return;
        }
        let publication = &self.publications[index];
        let start_position = match publication.log.producer_position() {
            Ok(position) => position,
            Err(error) => {
                self.statics.log_buffer_rejected_tail.increment();
                self.send_error(correlation_id, ErrorCode::GenericError, &format!("{error}"));
                return;
            }
        };
        let reader = match publication.log.reader(start_position) {
            Ok(reader) => reader,
            Err(error) => {
                self.send_error(correlation_id, ErrorCode::GenericError, &format!("{error}"));
                return;
            }
        };
        let header = publication.log.header();
        let (publication_id, session_id) = (publication.registration_id, publication.session_id);
        let channel = String::from_utf8_lossy(IPC_CHANNEL).into_owned();
        let start = Start {
            publication_id,
            correlation_id,
            start_timestamp: now,
            start_position,
            initial_term_id: header.initial_term_id(),
            term_length: i32::try_from(header.term_length()).unwrap_or(i32::MAX),
            mtu_length: i32::try_from(header.mtu_length()).unwrap_or(i32::MAX),
            session_id,
            stream_id,
            source_identity: channel.clone(),
            channel,
            reader,
        };
        if self.recorder.requests.offer(Request::Start(start)).is_err() {
            self.recorder_queue_full(correlation_id);
            return;
        }
        self.recordings.push(RecordingEntry {
            publication_id,
            session_id,
            stream_id,
            start_position,
            state: RecordingState::Starting,
            recording_id: None,
            position: None,
            synced: None,
            bytes: None,
            stop_requested: false,
            armed: false,
        });
    }

    /// `start recording` on a `cavalry:udp` channel: of the publications sending on the
    /// resolved channel and the images received on it, on the stream, the one with the
    /// lowest registration id not yet recorded (ADR 0042). A publication is recorded as
    /// over IPC, its channel the resolved one; an image is read through a second,
    /// read-only mapping of its log from its rebuild position, and the recording is one
    /// of the image's readers with the receiver, its start position the floor until the
    /// recorder attaches the counter.
    fn start_udp_recording(
        &mut self,
        correlation_id: i64,
        stream_id: i32,
        target: SendTarget,
        now: i64,
    ) {
        let resolved = target.resolved();
        let send_endpoints: Vec<i64> = self
            .send_endpoints
            .iter()
            .filter(|entry| entry.target == target)
            .map(|entry| entry.registration_id)
            .collect();
        // A control channel is a publication's: no receive endpoint carries it.
        let receive_endpoints: Vec<i64> = self
            .receive_endpoints
            .iter()
            .filter(|entry| {
                !target.control
                    && entry.endpoint == target.address
                    && entry.interface == target.interface
            })
            .map(|entry| entry.registration_id)
            .collect();
        // A second start while a recording is armed on the stream is code 22, whatever
        // else the stream has, a publication sending to the endpoint among them
        // (`spec/client-protocol.md`, "Commands", start recording; ADR 0059).
        if self.armed_recordings.iter().any(|armed| {
            armed.stream_id == stream_id && receive_endpoints.contains(&armed.endpoint_id)
        }) {
            self.send_error(
                correlation_id,
                ErrorCode::AlreadyRecording,
                &format!(
                    "stream {stream_id} on {resolved} has a recording armed, which starts when \
                     the stream's next image arrives; wait for its recording ready; see \
                     {ERRORS_DOC}"
                ),
            );
            return;
        }
        // The candidates: (registration id, is an image), live and on the stream.
        let mut candidates: Vec<(i64, bool)> = self
            .publications
            .iter()
            .filter(|publication| {
                publication.stream_id == stream_id
                    && publication.closing.is_none()
                    && publication
                        .udp
                        .as_ref()
                        .is_some_and(|udp| send_endpoints.contains(&udp.endpoint_id))
            })
            .map(|publication| (publication.registration_id, false))
            .collect();
        candidates.extend(
            self.udp_images
                .iter()
                .filter(|image| {
                    image.stream_id == stream_id
                        && image.state == ImageState::Live
                        && !image.leaving
                        && receive_endpoints.contains(&image.endpoint_id)
                })
                .map(|image| (image.registration_id, true)),
        );
        // Version 6: no publication and no image, but a live subscription on the
        // endpoint and stream, arms the recording for the stream's next image, one per
        // endpoint and stream, a second start while one is armed being code 22 above
        // (`spec/client-protocol.md`, "Commands", start recording; ADR 0059).
        if candidates.is_empty() {
            let subscribed = self
                .subscriptions
                .iter()
                .find(|subscription| {
                    subscription.stream_id == stream_id
                        && subscription
                            .endpoint
                            .is_some_and(|endpoint| receive_endpoints.contains(&endpoint))
                })
                .and_then(|subscription| subscription.endpoint);
            if let Some(endpoint_id) = subscribed {
                if self.armed_recordings.len() >= spec::MAX_SUBSCRIPTIONS {
                    self.registry_full(correlation_id, "armed recordings", spec::MAX_SUBSCRIPTIONS);
                    return;
                }
                self.armed_recordings.push(ArmedRecording {
                    endpoint_id,
                    stream_id,
                    correlation_id,
                });
                self.statics.recording_armed.increment();
                self.send(&Event::OperationSuccess { correlation_id });
                return;
            }
        }
        if candidates.is_empty() {
            self.send_error(
                correlation_id,
                ErrorCode::NoPublicationToRecord,
                &format!(
                    "no live publication or image on stream {stream_id} and channel {resolved}; \
                     add the publication or wait for the subscription's image first; see \
                     {ERRORS_DOC}"
                ),
            );
            return;
        }
        candidates.sort_by_key(|(id, _)| *id);
        let chosen = candidates.iter().copied().find(|(id, _)| {
            !self
                .recordings
                .iter()
                .any(|recording| recording.publication_id == *id)
        });
        let Some((registration_id, image)) = chosen else {
            let (lowest, _) = candidates[0];
            let recording = self
                .recordings
                .iter()
                .find(|recording| recording.publication_id == lowest)
                .and_then(|recording| {
                    recording
                        .recording_id
                        .map(|id| (id, recording.stop_requested))
                });
            let text = match recording {
                Some((id, true)) => format!(
                    "stream {stream_id} on {resolved} is stopping recording {id}; start again \
                     once its recording stopped arrives; see {ERRORS_DOC}"
                ),
                Some((id, false)) => format!(
                    "stream {stream_id} on {resolved} is already recording {id}; stop recording \
                     {id} first; see {ERRORS_DOC}"
                ),
                None => format!(
                    "stream {stream_id} on {resolved} is already starting a recording; wait for \
                     its recording ready; see {ERRORS_DOC}"
                ),
            };
            self.send_error(correlation_id, ErrorCode::AlreadyRecording, &text);
            return;
        };
        if self.recordings.len() >= spec::MAX_RECORDINGS {
            self.registry_full(correlation_id, "recordings", spec::MAX_RECORDINGS);
            return;
        }
        if !self.deferred.is_empty() {
            self.recorder_queue_full(correlation_id);
            return;
        }
        if image {
            let Some(start_position) = self
                .udp_images
                .iter()
                .find(|image| image.registration_id == registration_id)
                .map(|entry| entry.rebuild.load_acquire())
            else {
                return;
            };
            self.start_image_recording(
                correlation_id,
                registration_id,
                start_position,
                resolved,
                false,
                now,
            );
            return;
        }
        let (start_position, reader, header, session_id, source_identity) = {
            let Some(publication) = self
                .publications
                .iter()
                .find(|publication| publication.registration_id == registration_id)
            else {
                return;
            };
            let start_position = match publication.log.producer_position() {
                Ok(position) => position,
                Err(error) => {
                    self.statics.log_buffer_rejected_tail.increment();
                    self.send_error(correlation_id, ErrorCode::GenericError, &format!("{error}"));
                    return;
                }
            };
            let reader = match publication.log.reader(start_position) {
                Ok(reader) => reader,
                Err(error) => {
                    self.send_error(correlation_id, ErrorCode::GenericError, &format!("{error}"));
                    return;
                }
            };
            (
                start_position,
                reader,
                publication.log.header(),
                publication.session_id,
                resolved.clone(),
            )
        };
        let start = Start {
            publication_id: registration_id,
            correlation_id,
            start_timestamp: now,
            start_position,
            initial_term_id: header.initial_term_id(),
            term_length: i32::try_from(header.term_length()).unwrap_or(i32::MAX),
            mtu_length: i32::try_from(header.mtu_length()).unwrap_or(i32::MAX),
            session_id,
            stream_id,
            source_identity,
            channel: resolved,
            reader,
        };
        if self.recorder.requests.offer(Request::Start(start)).is_err() {
            self.recorder_queue_full(correlation_id);
            return;
        }
        self.recordings.push(RecordingEntry {
            publication_id: registration_id,
            session_id,
            stream_id,
            start_position,
            state: RecordingState::Starting,
            recording_id: None,
            position: None,
            synced: None,
            bytes: None,
            stop_requested: false,
            armed: false,
        });
    }

    /// Starts recording the image `registration_id` from `start_position`: for a
    /// `start recording` on a live image, from its rebuild position, and for an armed
    /// recording when the stream's next image arrives, from the image's join position,
    /// `armed` then marking the entry so that the recorder's answer counts as the armed
    /// recording's (ADR 0059). The recorder's start, the entry and the receiver's
    /// reader, every refusal answered under `correlation_id` with the code a live start
    /// gets; whether the start was handed to the recorder, which is not yet whether it
    /// started: `on_ready`, `fail_start` and `on_start_failed` say that.
    fn start_image_recording(
        &mut self,
        correlation_id: i64,
        registration_id: i64,
        start_position: i64,
        channel: String,
        armed: bool,
        now: i64,
    ) -> bool {
        if self.recordings.len() >= spec::MAX_RECORDINGS {
            self.registry_full(correlation_id, "recordings", spec::MAX_RECORDINGS);
            return false;
        }
        if !self.deferred.is_empty() {
            self.recorder_queue_full(correlation_id);
            return false;
        }
        // Never taken, so never unanswered: both callers found the image in
        // `udp_images` in the same call, `start_udp_recording` among the candidates and
        // `on_setup` where it pushed the image, and nothing between removes one.
        let Some((session_id, stream_id, source_identity)) = self
            .udp_images
            .iter()
            .find(|image| image.registration_id == registration_id)
            .map(|entry| {
                (
                    entry.session_id,
                    entry.stream_id,
                    entry.source_identity.clone(),
                )
            })
        else {
            return false;
        };
        let log = match LogBuffers::open_image(&self.dir, registration_id, false) {
            Ok(log) => log,
            Err(error) => {
                self.send_error(correlation_id, ErrorCode::GenericError, &format!("{error}"));
                return false;
            }
        };
        let reader = match log.reader(start_position) {
            Ok(reader) => reader,
            Err(error) => {
                self.send_error(correlation_id, ErrorCode::GenericError, &format!("{error}"));
                return false;
            }
        };
        let header = log.header();
        let start = Start {
            publication_id: registration_id,
            correlation_id,
            start_timestamp: now,
            start_position,
            initial_term_id: header.initial_term_id(),
            term_length: i32::try_from(header.term_length()).unwrap_or(i32::MAX),
            mtu_length: i32::try_from(header.mtu_length()).unwrap_or(i32::MAX),
            session_id,
            stream_id,
            source_identity,
            channel,
            reader,
        };
        if self.recorder.requests.offer(Request::Start(start)).is_err() {
            self.recorder_queue_full(correlation_id);
            return false;
        }
        self.recordings.push(RecordingEntry {
            publication_id: registration_id,
            session_id,
            stream_id,
            start_position,
            state: RecordingState::Starting,
            recording_id: None,
            position: None,
            synced: None,
            bytes: None,
            stop_requested: false,
            armed,
        });
        self.receiver_request(receiver::Request::AddReader {
            registration_id,
            key: ReaderKey::Recording(registration_id),
            counter: None,
            floor: start_position,
        });
        true
    }

    /// Drops every armed recording `gone` names, each answered under its arming
    /// command's correlation id with code 20 and counted in `recording.armed_dropped`:
    /// its stream's last subscription on the endpoint went, so no image will come
    /// (`spec/client-protocol.md`, "Commands", start recording; ADR 0059).
    fn drop_armed_recordings(&mut self, gone: impl Fn(&ArmedRecording) -> bool) {
        let mut index = 0;
        while index < self.armed_recordings.len() {
            if gone(&self.armed_recordings[index]) {
                let armed = self.armed_recordings.swap_remove(index);
                self.statics.recording_armed_dropped.increment();
                self.send_error(
                    armed.correlation_id,
                    ErrorCode::NoPublicationToRecord,
                    &format!(
                        "the recording armed on stream {} was dropped: the stream's last \
                         subscription on its endpoint went before an image arrived, so none \
                         will; subscribe again and start recording again; see {ERRORS_DOC}",
                        armed.stream_id
                    ),
                );
            } else {
                index += 1;
            }
        }
    }

    /// `stop recording` (`spec/client-protocol.md`, "Commands"): an id the catalog does
    /// not hold is code 21; one no live recording holds is already stopped, answered
    /// with `operation success` and nothing written; a live one is handed to the
    /// recorder, whose reply answers the command. A second stop while one is with the
    /// recorder is answered at once, since the stop is on its way.
    fn stop_recording(&mut self, correlation_id: i64, recording_id: i64) {
        if recording_id < 0 || recording_id >= self.recorder.next_recording_id {
            self.send_error(
                correlation_id,
                ErrorCode::UnknownRecording,
                &format!(
                    "recording {recording_id} is not in the catalog, whose next id is {}; \
                     `cavalry doctor` lists the recordings; see {ERRORS_DOC}",
                    self.recorder.next_recording_id
                ),
            );
            return;
        }
        let found = self
            .recordings
            .iter()
            .position(|recording| recording.recording_id == Some(recording_id));
        match found {
            Some(index) if !self.recordings[index].stop_requested => {
                self.recordings[index].stop_requested = true;
                self.request(Request::Stop {
                    recording_id,
                    correlation_id: Some(correlation_id),
                });
            }
            _ => self.send(&Event::OperationSuccess { correlation_id }),
        }
    }

    /// `sync recording` (`spec/client-protocol.md`, "Commands"; ADR 0069): an id the
    /// catalog does not hold is code 21; one no live recording holds has stopped,
    /// answered with `operation success` and nothing done; a live one is handed to the
    /// recorder, whose reply answers the command (`on_sync_done`). The request is never
    /// deferred: with the request queue full, or holding requests that still wait for
    /// room in it, the command is refused with code 10 and the client sends it again,
    /// so that a client's syncs are bounded by the queue and never take the deferred
    /// list's room from a recording's attach or stop.
    fn sync_recording(&mut self, correlation_id: i64, recording_id: i64) {
        if recording_id < 0 || recording_id >= self.recorder.next_recording_id {
            self.send_error(
                correlation_id,
                ErrorCode::UnknownRecording,
                &format!(
                    "recording {recording_id} is not in the catalog, whose next id is {}; \
                     `cavalry doctor` lists the recordings; see {ERRORS_DOC}",
                    self.recorder.next_recording_id
                ),
            );
            return;
        }
        if !self
            .recordings
            .iter()
            .any(|recording| recording.recording_id == Some(recording_id))
        {
            self.send(&Event::OperationSuccess { correlation_id });
            return;
        }
        if !self.deferred.is_empty()
            || self
                .recorder
                .requests
                .offer(Request::Sync {
                    recording_id,
                    correlation_id,
                })
                .is_err()
        {
            self.recorder_queue_full(correlation_id);
        }
    }

    /// The recorder handled a `sync recording`: `operation success` when everything it
    /// had written is covered by a sync, or the recording had stopped; code 12 when
    /// the sync failed or one had failed before, after which the recorder gives the
    /// recording up and `recording stopped` follows with the position last synced
    /// (ADR 0065, ADR 0069).
    fn on_sync_done(&mut self, correlation_id: i64, recording_id: i64, outcome: SyncOutcome) {
        match outcome {
            SyncOutcome::Done => self.send(&Event::OperationSuccess { correlation_id }),
            SyncOutcome::Failed(error) => self.send_error(
                correlation_id,
                ErrorCode::StorageSpace,
                &format!(
                    "a sync of recording {recording_id} failed, so the recording is given \
                     up at the position last synced and `recording stopped` follows; check \
                     the disk the log directory is on, and start a new recording once it \
                     is sound; the sync said: {error}; see {ERRORS_DOC}"
                ),
            ),
            SyncOutcome::AlreadyFailed => self.send_error(
                correlation_id,
                ErrorCode::StorageSpace,
                &format!(
                    "a sync of recording {recording_id} has failed before, so no sync is \
                     made and the recording is given up at the position last synced; \
                     `recording stopped` follows; check the disk the log directory is on, \
                     and start a new recording once it is sound; see {ERRORS_DOC}"
                ),
            ),
        }
    }

    /// Offers a request to the recorder, or keeps it for the next cycle when the queue
    /// is full or earlier requests are still waiting, so that the recorder sees them in
    /// order. Nothing is dropped: the deferred list never passes its capacity, by the
    /// argument on `Conductor::deferred`, which the capacity test drives to the bound.
    fn request(&mut self, request: Request) {
        let request = if self.deferred.is_empty() {
            match self.recorder.requests.offer(request) {
                Ok(()) => return,
                Err(request) => request,
            }
        } else {
            request
        };
        debug_assert!(
            self.deferred.len() < DEFERRED_CAPACITY,
            "the deferred requests passed one attach and one stop per recording"
        );
        self.deferred.push_back(request);
    }

    /// Offers the deferred requests in order until the queue is full again.
    fn flush_deferred(&mut self) -> usize {
        let mut sent = 0;
        while let Some(request) = self.deferred.pop_front() {
            match self.recorder.requests.offer(request) {
                Ok(()) => sent += 1,
                Err(request) => {
                    self.deferred.push_front(request);
                    break;
                }
            }
        }
        sent
    }

    /// Takes every reply the recorder has queued and acts on it.
    fn take_replies(&mut self, now: i64) -> usize {
        let mut taken = 0;
        while let Some(reply) = self.recorder.replies.poll() {
            match reply {
                Reply::Ready {
                    publication_id,
                    correlation_id,
                    recording_id,
                    next_recording_id,
                } => {
                    self.recorder.next_recording_id = next_recording_id;
                    self.on_ready(publication_id, correlation_id, recording_id, now);
                }
                Reply::StartFailed {
                    publication_id,
                    correlation_id,
                    failure,
                    next_recording_id,
                } => {
                    self.recorder.next_recording_id = next_recording_id;
                    self.on_start_failed(publication_id, correlation_id, failure);
                }
                Reply::Stopped {
                    recording_id,
                    publication_id,
                    stop_position,
                    correlation_id,
                } => self.on_stopped(
                    recording_id,
                    publication_id,
                    stop_position,
                    correlation_id,
                    now,
                ),
                Reply::CatalogDone {
                    correlation_id,
                    recording_id,
                    write,
                    result,
                    repaired,
                } => self.on_catalog_done(correlation_id, recording_id, write, result, repaired),
                Reply::StopIgnored { correlation_id } => {
                    if let Some(correlation_id) = correlation_id {
                        self.send(&Event::OperationSuccess { correlation_id });
                    }
                }
                Reply::SyncDone {
                    correlation_id,
                    recording_id,
                    outcome,
                } => self.on_sync_done(correlation_id, recording_id, outcome),
            }
            taken += 1;
        }
        taken
    }

    /// The recorder took the slot and created the files: the two counters are
    /// allocated, keyed by the recording id and set to the start position, the recorder
    /// gets their handles, and the client gets `recording ready`, an armed recording's
    /// counted in `recording.armed_started`. A counters file with no free record fails
    /// the start with code 10 and asks the recorder to stop the recording
    /// (`fail_start`).
    fn on_ready(&mut self, publication_id: i64, correlation_id: i64, recording_id: i64, now: i64) {
        let Some(index) = self.recordings.iter().position(|recording| {
            recording.publication_id == publication_id
                && recording.state == RecordingState::Starting
        }) else {
            return;
        };
        let (session_id, stream_id, start_position, armed) = {
            let entry = &self.recordings[index];
            (
                entry.session_id,
                entry.stream_id,
                entry.start_position,
                entry.armed,
            )
        };
        let key = recording_id.to_le_bytes();
        let label = |name: &str| format!("{name} {recording_id} {session_id} {stream_id}");
        let position =
            match self
                .counters
                .allocate(RECORDING_POSITION, &key, &label("recording.position"))
            {
                Ok(counter) => counter,
                Err(error) => {
                    self.fail_start(index, recording_id, correlation_id, &error);
                    return;
                }
            };
        let synced = match self.counters.allocate(
            RECORDING_SYNCED_POSITION,
            &key,
            &label("recording.synced_position"),
        ) {
            Ok(counter) => counter,
            Err(error) => {
                let _ = self.counters.free(
                    position.id(),
                    now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT)),
                );
                self.fail_start(index, recording_id, correlation_id, &error);
                return;
            }
        };
        let bytes = match self
            .counters
            .allocate(RECORDING_BYTES, &key, &label("recording.bytes"))
        {
            Ok(counter) => counter,
            Err(error) => {
                let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
                let _ = self.counters.free(position.id(), reuse);
                let _ = self.counters.free(synced.id(), reuse);
                self.fail_start(index, recording_id, correlation_id, &error);
                return;
            }
        };
        position.store_release(start_position);
        synced.store_release(start_position);
        bytes.store_release(0);
        let (position_id, synced_id) = (counter_id(&position), counter_id(&synced));
        {
            let entry = &mut self.recordings[index];
            entry.state = RecordingState::Live;
            entry.recording_id = Some(recording_id);
            entry.position = Some(position.clone());
            entry.synced = Some(synced.clone());
            entry.bytes = Some(bytes.clone());
        }
        self.statics.recording_started.increment();
        // A recording of an image over UDP is one of its readers: the receiver gets the
        // counter in place of the floor it held (ADR 0042).
        if self
            .udp_images
            .iter()
            .any(|image| image.registration_id == publication_id)
        {
            self.receiver_request(receiver::Request::AttachReader {
                registration_id: publication_id,
                key: ReaderKey::Recording(publication_id),
                counter: position.clone(),
            });
        }
        self.request(Request::Attach {
            recording_id,
            position,
            synced,
            bytes,
        });
        self.send(&Event::RecordingReady {
            correlation_id,
            recording_id,
            session_id,
            recording_position_counter_id: position_id,
            recording_synced_position_counter_id: synced_id,
        });
        if armed {
            self.statics.recording_armed_started.increment();
        }
    }

    /// A start the counters file refused after the slot was taken: the client gets
    /// code 10 and the entry fails, an armed recording's counted in
    /// `recording.armed_failed`, but the recorder is already reading the log and
    /// writing the slot, and goes on until it handles the stop asked for here. So the
    /// entry stays registered, a floor at its start position for the limit, the
    /// cleaning and the closing, and leaves only with the recorder's `Stopped` reply
    /// (`on_stopped`), which ends the slot at the position written and tells every
    /// client `recording stopped` (`spec/client-protocol.md`, "Commands"; invariant 1).
    fn fail_start(
        &mut self,
        index: usize,
        recording_id: i64,
        correlation_id: i64,
        error: &CountersError,
    ) {
        self.counters_full(correlation_id, error);
        if self.recordings[index].armed {
            self.statics.recording_armed_failed.increment();
        }
        let entry = &mut self.recordings[index];
        entry.state = RecordingState::Failed;
        entry.recording_id = Some(recording_id);
        entry.stop_requested = true;
        self.request(Request::Stop {
            recording_id,
            correlation_id: None,
        });
    }

    /// The recorder could not take a slot or create the files: the entry fails and the
    /// client gets code 10 (catalog full) or 12 (files), each with its counter, and an
    /// armed recording's failure in `recording.armed_failed` as well.
    fn on_start_failed(&mut self, publication_id: i64, correlation_id: i64, failure: StartFailure) {
        if let Some(index) = self.recordings.iter().position(|recording| {
            recording.publication_id == publication_id
                && recording.state == RecordingState::Starting
        }) {
            self.recordings[index].state = RecordingState::Failed;
            if self.recordings.swap_remove(index).armed {
                self.statics.recording_armed_failed.increment();
            }
        }
        // As on a stop: not connected from here if the recording connected the
        // publication alone (ADR 0048).
        if let Some(index) = self
            .publications
            .iter()
            .position(|publication| publication.registration_id == publication_id)
        {
            self.lower_connected(index);
        }
        match failure {
            StartFailure::CatalogFull => {
                self.statics.recording_catalog_full.increment();
                self.send_error(
                    correlation_id,
                    ErrorCode::ResourceTemporarilyUnavailable,
                    &format!(
                        "the catalog of the log directory holds every one of its slots; move or \
                         archive the log directory and start the driver on a new one; see \
                         {ERRORS_DOC}"
                    ),
                );
            }
            StartFailure::Files(text) => {
                self.statics.recording_start_failed.increment();
                self.send_error(correlation_id, ErrorCode::StorageSpace, &text);
            }
        }
        self.drop_recording_reader(publication_id);
    }

    /// Stores is connected 0 for the publication at `index` once nothing connects it:
    /// neither an image nor a recording of it, or over `cavalry:udp` no receiver. The
    /// commands pass calls it where the last of these goes, so that the 0 is stored
    /// before the limit pass lowers the limit, and a publisher at the old limit or the
    /// lowered one is told `not connected`, never `back pressured`. The 1 is the limit
    /// pass's, after its raise (ADR 0048; card 187).
    fn lower_connected(&mut self, index: usize) {
        let Some(publication) = self.publications.get(index) else {
            return;
        };
        if !publication.connected_flag {
            return;
        }
        let id = publication.registration_id;
        let connected = match &publication.udp {
            Some(udp) => udp.connected,
            None => {
                self.images.iter().any(|image| image.publication_id == id)
                    || self
                        .recordings
                        .iter()
                        .any(|recording| recording.publication_id == id)
            }
        };
        if !connected && let Some(publication) = self.publications.get_mut(index) {
            let _ = publication.log.set_connected(false);
            publication.connected_flag = false;
        }
    }

    /// A recording of an image over UDP ended or failed: its reader leaves the image,
    /// and an image already retired is finished now that nothing records it.
    fn drop_recording_reader(&mut self, publication_id: i64) {
        let Some(image) = self
            .udp_images
            .iter()
            .find(|image| image.registration_id == publication_id)
        else {
            return;
        };
        if image.state == ImageState::Live {
            self.receiver_request(receiver::Request::RemoveReader {
                registration_id: publication_id,
                key: ReaderKey::Recording(publication_id),
            });
        } else if !image.waiting {
            self.remove_udp_image(publication_id, now_nanos());
        }
        // A retired image that waits is `remove_retired_images`'s, on the duty cycle's
        // clock, now that nothing records it (ADR 0078).
    }

    /// The recorder stopped a recording: its entry goes, its counters are freed after
    /// the reuse timeout, a command that asked gets `operation success`, and every
    /// client gets `recording stopped` with the stop position the recorder reported, a
    /// durable one: the catalog's, or the last sync's when the stop's own writes failed,
    /// which still stopped the recording (ADR 0064).
    fn on_stopped(
        &mut self,
        recording_id: i64,
        publication_id: i64,
        stop_position: i64,
        correlation_id: Option<i64>,
        now: i64,
    ) {
        if let Some(index) = self
            .recordings
            .iter()
            .position(|recording| recording.recording_id == Some(recording_id))
        {
            self.recordings[index].state = RecordingState::Stopped;
            let entry = self.recordings.swap_remove(index);
            let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
            for counter in [entry.position, entry.synced, entry.bytes]
                .into_iter()
                .flatten()
            {
                let _ = self.counters.free(counter.id(), reuse);
            }
        }
        // Not connected from here if the recording connected the publication alone,
        // before the event and before the limit pass lowers the limit (ADR 0048).
        if let Some(index) = self
            .publications
            .iter()
            .position(|publication| publication.registration_id == publication_id)
        {
            self.lower_connected(index);
        }
        self.statics.recording_stopped.increment();
        if let Some(correlation_id) = correlation_id {
            self.send(&Event::OperationSuccess { correlation_id });
        }
        self.send(&Event::RecordingStopped {
            recording_id,
            stop_position,
        });
        self.drop_recording_reader(publication_id);
        // A replay of this recording learns the stop from the conductor, not from the
        // synced counter it holds, which is freed above and another's after the reuse
        // timeout, nor from the catalog slot it reads once a second (ADR 0036).
        if self
            .replays
            .iter()
            .any(|replay| replay.recording_id == recording_id)
        {
            self.replayer_request(replayer::Request::RecordingStopped {
                recording_id,
                stop_position,
            });
        }
    }

    /// `start replay` (`spec/client-protocol.md`, "Commands"): the channel, the position
    /// keys on it (code 1, reason `position`), the length,
    /// a retention command on the recording (code 24, counted in `replay.start_failed`
    /// as the replayer's three refusals are) and the registry are checked here; the
    /// recording's slot, the position and the segment file are the replayer's, whose
    /// reply creates the publication (`on_replay_ready`) or answers with code 21, 23
    /// or 12 (`on_replay_start_failed`). The replay session id is the command's
    /// correlation id.
    fn start_replay(
        &mut self,
        correlation_id: i64,
        recording_id: i64,
        position: i64,
        length: i64,
        replay_stream_id: i32,
        channel: &[u8],
    ) {
        let Some(channel) = self.check_channel(correlation_id, channel, "the replay channel")
        else {
            return;
        };
        // The position keys create a client's publication at a position; a replay's
        // publication starts at the replay's position under its recording's initial term
        // id, so keys on its channel would do nothing and are refused (`spec/udp.md`,
        // "The channel URI"; ADR 0059).
        if let Channel::Udp(udp) = &channel
            && udp.names_position()
        {
            self.statics.rejected_command.increment();
            self.send_error(
                correlation_id,
                ErrorCode::InvalidChannel,
                &format!(
                    "the replay channel: channel refused (position): it names init-term-id, \
                     term-id or term-offset, which create a client's publication at a \
                     position; a replay starts at the command's position under its \
                     recording's initial term id, so remove the keys and name the position in \
                     the command; see {ERRORS_DOC}"
                ),
            );
            return;
        }
        // A `cavalry:udp` replay channel is resolved once, here, and kept with its `fc`
        // for the publication the replayer's reply creates (ADR 0043, ADR 0053).
        let udp = match channel {
            Channel::Ipc => None,
            Channel::Udp(udp) => match self.resolve_target(correlation_id, &udp) {
                Some(resolved) => Some((resolved, udp.flow_control())),
                None => return,
            },
        };
        if length < NULL_LENGTH {
            self.statics.rejected_command.increment();
            self.send_error(
                correlation_id,
                ErrorCode::MalformedCommand,
                &format!(
                    "replay length {length} is below -1; -1 plays to the recording's extent, \
                     {} follows a live recording, and 0 or more is a byte count; see {ERRORS_DOC}",
                    i64::MAX
                ),
            );
            return;
        }
        if self
            .commands
            .iter()
            .any(|entry| entry.command.recording_id() == recording_id)
        {
            // The fourth of the spec's four refusals that count in `replay.start_failed`;
            // the other three come back from the replayer (`on_replay_start_failed`).
            self.statics.replay_start_failed.increment();
            self.send_error(
                correlation_id,
                ErrorCode::RecordingBusy,
                &format!(
                    "recording {recording_id} is under a retention command, whose cut reads \
                     as live until it is done; wait for that command's answer, then start the \
                     replay again; see {ERRORS_DOC}"
                ),
            );
            return;
        }
        if self.replays.len() >= spec::MAX_REPLAYS {
            self.registry_full(correlation_id, "replays", spec::MAX_REPLAYS);
            return;
        }
        // A replay's publication outlives its registry slot while it lingers, so the
        // publications of every replay, live or lingering, and the starts whose
        // publication is still to come have a bound of their own (ADR 0036).
        let starting = self
            .replays
            .iter()
            .filter(|replay| replay.state == ReplayState::Starting)
            .count();
        let replay_publications = self
            .publications
            .iter()
            .filter(|publication| publication.replay)
            .count();
        if starting + replay_publications >= spec::MAX_REPLAY_PUBLICATIONS {
            self.registry_full(
                correlation_id,
                "replay publications",
                spec::MAX_REPLAY_PUBLICATIONS,
            );
            return;
        }
        // Requests still waiting for room go first, and none of a new replay's join
        // them: this is what bounds the deferred list (`Conductor::replayer_deferred`).
        if !self.replayer_deferred.is_empty() {
            self.replayer_queue_full(correlation_id);
            return;
        }
        // A live recording's extent is its synced position, which the replayer reads
        // from the counter the conductor holds for it.
        let synced = self
            .recordings
            .iter()
            .find(|recording| recording.recording_id == Some(recording_id))
            .and_then(|recording| recording.synced.clone());
        let start = replayer::Start {
            replay_session_id: correlation_id,
            recording_id,
            position,
            length,
            synced,
        };
        if self
            .replayer
            .requests
            .offer(replayer::Request::Start(start))
            .is_err()
        {
            self.replayer_queue_full(correlation_id);
            return;
        }
        self.wake_replayer();
        self.replays.push(ReplayEntry {
            replay_session_id: correlation_id,
            recording_id,
            replay_stream_id,
            state: ReplayState::Starting,
            position: None,
            unconnected_since: None,
            stop_requested: false,
            udp,
            connected: false,
        });
    }

    /// `stop replay` (`spec/client-protocol.md`, "Commands"): a replay session id a
    /// registered replay holds is handed to the replayer, whose reply answers the
    /// command; one no replay holds is already stopped, answered with `operation
    /// success` and nothing done; a second stop while one is on its way is answered at
    /// once.
    fn stop_replay(&mut self, correlation_id: i64, replay_session_id: i64) {
        let found = self
            .replays
            .iter()
            .position(|replay| replay.replay_session_id == replay_session_id);
        match found {
            Some(index) if !self.replays[index].stop_requested => {
                self.replays[index].stop_requested = true;
                self.replayer_request(replayer::Request::Stop {
                    replay_session_id,
                    correlation_id: Some(correlation_id),
                    reason: ReplayStopReason::Stopped,
                });
            }
            _ => self.send(&Event::OperationSuccess { correlation_id }),
        }
    }

    /// Offers a request to the replayer and wakes it, or keeps the request for the
    /// next cycle, as `request` does for the recorder.
    fn replayer_request(&mut self, request: replayer::Request) {
        let request = if self.replayer_deferred.is_empty() {
            match self.replayer.requests.offer(request) {
                Ok(()) => {
                    self.wake_replayer();
                    return;
                }
                Err(request) => request,
            }
        } else {
            request
        };
        debug_assert!(
            self.replayer_deferred.len() < REPLAYER_DEFERRED_CAPACITY,
            "the replayer's deferred requests passed three per replay: an attach, a stop and \
             its recording's stop"
        );
        self.replayer_deferred.push_back(request);
    }

    /// Offers the replayer's deferred requests in order until the queue is full again,
    /// and wakes the replayer when any went.
    fn flush_replayer_deferred(&mut self) -> usize {
        let mut sent = 0;
        while let Some(request) = self.replayer_deferred.pop_front() {
            match self.replayer.requests.offer(request) {
                Ok(()) => sent += 1,
                Err(request) => {
                    self.replayer_deferred.push_front(request);
                    break;
                }
            }
        }
        if sent > 0 {
            self.wake_replayer();
        }
        sent
    }

    /// Takes every reply the replayer has queued and acts on it.
    fn take_replayer_replies(&mut self, now: i64) -> usize {
        let mut taken = 0;
        while let Some(reply) = self.replayer.replies.poll() {
            match reply {
                replayer::Reply::Ready {
                    replay_session_id,
                    recording_id,
                    start_position,
                    initial_term_id,
                    term_length,
                    mtu_length,
                    recorded_session_id,
                    recorded_stream_id,
                } => self.on_replay_ready(
                    replay_session_id,
                    recording_id,
                    start_position,
                    (initial_term_id, term_length, mtu_length),
                    (recorded_session_id, recorded_stream_id),
                    now,
                ),
                replayer::Reply::StartFailed {
                    replay_session_id,
                    failure,
                } => self.on_replay_start_failed(replay_session_id, failure),
                replayer::Reply::Stopped {
                    replay_session_id,
                    position,
                    reason,
                    correlation_id,
                } => {
                    self.on_replay_stopped(replay_session_id, position, reason, correlation_id, now)
                }
                replayer::Reply::Checked { correlation_id } => self.on_checked(correlation_id),
                replayer::Reply::CheckFailed {
                    correlation_id,
                    failure,
                } => self.on_check_failed(correlation_id, failure),
                replayer::Reply::FileWorkDone {
                    correlation_id,
                    command,
                    outcome,
                } => self.on_file_work_done(correlation_id, command, outcome),
                replayer::Reply::Repaired {
                    correlation_id,
                    recording_id,
                    outcome,
                } => self.on_repaired(correlation_id, recording_id, outcome),
            }
            taken += 1;
        }
        taken
    }

    /// The replayer read the slot and opened the segment file: the replay's publication
    /// is created at the start position with the recording's term, a new session id and
    /// the replay stream id, its three counters allocated, the replayer's writer
    /// attached, `replay ready` sent, and an image given to every subscription on the
    /// stream (`spec/client-protocol.md`, "Commands"). Over a `cavalry:udp` replay
    /// channel the publication takes the sender's path as a client's does (ADR 0043):
    /// the sender's room and the send endpoint first, opened or shared as `add
    /// publication` opens it, `sender.position` and `sender.limit` beside the three,
    /// the socket when new and the publication with a reader at the start position
    /// handed to the sender, and no image, since the images are the receiving driver's.
    /// A log that cannot be created is code 12, a counters file with no free record
    /// code 10, and over UDP the sender's queue or the endpoints registry with no room
    /// code 10 and a socket the operating system refused code 4; each removes the
    /// replay with nothing of it left.
    fn on_replay_ready(
        &mut self,
        replay_session_id: i64,
        recording_id: i64,
        start_position: i64,
        (initial_term_id, term_length, mtu_length): (i32, i32, i32),
        (recorded_session_id, recorded_stream_id): (i32, i32),
        now: i64,
    ) {
        let Some(index) = self.replays.iter().position(|replay| {
            replay.replay_session_id == replay_session_id && replay.state == ReplayState::Starting
        }) else {
            return;
        };
        debug_assert_eq!(self.replays[index].recording_id, recording_id);
        let recording_id = self.replays[index].recording_id;
        let stream_id = self.replays[index].replay_stream_id;
        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
        // Over UDP, the sender's room and the endpoint before anything is made, in the
        // order `add publication` keeps; a new endpoint is abandoned on any failure
        // below, as a client's is.
        let flow_control = self.replays[index]
            .udp
            .map_or(FlowControl::Max, |(_, flow_control)| flow_control);
        let endpoint = match self.replays[index].udp {
            None => None,
            Some((target, _)) => {
                if !self.sender_has_room() {
                    self.statics.registry_full.increment();
                    self.fail_replay(
                        index,
                        ErrorCode::ResourceTemporarilyUnavailable,
                        &sender_queue_full_message(),
                    );
                    return;
                }
                match self.open_send_endpoint(replay_session_id, target, now) {
                    Ok(opened) => Some(opened),
                    Err((code, message)) => {
                        self.fail_replay(index, code, &message);
                        return;
                    }
                }
            }
        };
        let endpoint_id = endpoint
            .as_ref()
            .map(|(endpoint_index, _)| self.send_endpoints[*endpoint_index].registration_id);
        let session_id = self.advance_session_id(stream_id);
        let term_length = u32::try_from(term_length).unwrap_or(0);
        let mtu_length = u32::try_from(mtu_length).unwrap_or(0);
        let fields = HeaderFields::new(
            LogType::Exclusive,
            replay_session_id,
            initial_term_id,
            term_length,
            mtu_length,
            term_length / 2,
            session_id,
            stream_id,
        );
        let log =
            fields.and_then(|fields| LogBuffers::create_at(&self.dir, &fields, start_position));
        let log = match log {
            Ok(log) => log,
            Err(error) => {
                self.statics.log_create_failed.increment();
                self.undo_replay_ready(
                    index,
                    Vec::new(),
                    None,
                    endpoint,
                    reuse,
                    (ErrorCode::StorageSpace, &format!("{error}")),
                );
                return;
            }
        };
        // The counters: the publication's two and `replay.position`, and over UDP the
        // sender's three, labelled as a publication's with the endpoint's registration
        // id after, and the strategy after that on the receivers gauge, so that `doctor`
        // names the endpoint and reads a `min` publication from the counters file alone;
        // one the counters file refuses frees the ones before it.
        let key = position_key(replay_session_id, session_id, stream_id);
        let replay_key = replay_session_id.to_le_bytes();
        let label = |name: &str| format!("{name} {replay_session_id} {session_id} {stream_id}");
        let mut wanted: Vec<(CounterId, &[u8], String)> = vec![
            (PUBLICATION_POSITION, &key, label("publication.position")),
            (PUBLICATION_LIMIT, &key, label("publication.limit")),
            (
                REPLAY_POSITION,
                &replay_key,
                format!(
                    "replay.position {replay_session_id} {recording_id} {session_id} {stream_id}"
                ),
            ),
        ];
        if let Some(endpoint_id) = endpoint_id {
            for (id, name, strategy) in [
                (SENDER_POSITION, "sender.position", String::new()),
                (SENDER_LIMIT, "sender.limit", String::new()),
                (
                    SENDER_FLOW_CONTROL_RECEIVERS,
                    "sender.flow_control_receivers",
                    format!(" fc={}", flow_control.name()),
                ),
            ] {
                wanted.push((id, &key, format!("{} {endpoint_id}{strategy}", label(name))));
            }
        }
        let mut allocated: Vec<Counter> = Vec::with_capacity(wanted.len());
        for (id, key, label) in &wanted {
            match self.counters.allocate(*id, key, label) {
                Ok(counter) => allocated.push(counter),
                Err(error) => {
                    self.statics.registry_full.increment();
                    self.undo_replay_ready(
                        index,
                        allocated,
                        Some(log),
                        endpoint,
                        reuse,
                        (
                            ErrorCode::ResourceTemporarilyUnavailable,
                            &format!("{error}"),
                        ),
                    );
                    return;
                }
            }
        }
        // One counter per entry of `wanted`, in its order; a count that does not match
        // cannot happen, and is undone like a refusal rather than left behind.
        let (position, limit, replay_position, sender) = if endpoint_id.is_some() {
            match <[Counter; 6]>::try_from(allocated) {
                Ok(
                    [
                        position,
                        limit,
                        replay_position,
                        sender_position,
                        sender_limit,
                        receivers,
                    ],
                ) => (
                    position,
                    limit,
                    replay_position,
                    Some((sender_position, sender_limit, receivers)),
                ),
                Err(held) => {
                    self.undo_replay_ready(
                        index,
                        held,
                        Some(log),
                        endpoint,
                        reuse,
                        (
                            ErrorCode::ResourceTemporarilyUnavailable,
                            &counters_miscounted_message(),
                        ),
                    );
                    return;
                }
            }
        } else {
            match <[Counter; 3]>::try_from(allocated) {
                Ok([position, limit, replay_position]) => (position, limit, replay_position, None),
                Err(held) => {
                    self.undo_replay_ready(
                        index,
                        held,
                        Some(log),
                        endpoint,
                        reuse,
                        (
                            ErrorCode::ResourceTemporarilyUnavailable,
                            &counters_miscounted_message(),
                        ),
                    );
                    return;
                }
            }
        };
        let writer = match log.writer(recorded_session_id, recorded_stream_id) {
            Ok(writer) => writer,
            Err(error) => {
                self.statics.log_create_failed.increment();
                self.undo_replay_ready(
                    index,
                    replay_counters(&position, &limit, &replay_position, sender.as_ref()),
                    Some(log),
                    endpoint,
                    reuse,
                    (ErrorCode::StorageSpace, &format!("{error}")),
                );
                return;
            }
        };
        // Over UDP: the sender's reader at the replay's start position, where its sender
        // position begins. The endpoint and the sender's counters go together, both from
        // the resolved channel; a pair that does not match cannot happen, and is undone
        // like a refusal rather than left behind.
        let udp_start = match (endpoint, sender) {
            (None, None) => None,
            (Some((endpoint_index, socket)), Some((sender_position, sender_limit, receivers))) => {
                match log.reader(start_position) {
                    Ok(reader) => Some((
                        endpoint_index,
                        socket,
                        sender_position,
                        sender_limit,
                        receivers,
                        reader,
                    )),
                    Err(error) => {
                        self.statics.log_create_failed.increment();
                        self.undo_replay_ready(
                            index,
                            replay_counters(
                                &position,
                                &limit,
                                &replay_position,
                                Some(&(sender_position, sender_limit, receivers)),
                            ),
                            Some(log),
                            Some((endpoint_index, socket)),
                            reuse,
                            (ErrorCode::StorageSpace, &format!("{error}")),
                        );
                        return;
                    }
                }
            }
            (endpoint, sender) => {
                self.undo_replay_ready(
                    index,
                    replay_counters(&position, &limit, &replay_position, sender.as_ref()),
                    Some(log),
                    endpoint,
                    reuse,
                    (
                        ErrorCode::ResourceTemporarilyUnavailable,
                        &counters_miscounted_message(),
                    ),
                );
                return;
            }
        };
        position.store_release(start_position);
        limit.store_release(start_position);
        replay_position.store_release(start_position);
        let log_file_name = log_layout::file_name(replay_session_id)
            .to_string_lossy()
            .into_owned();
        let limit_handle = limit.clone();
        // Over UDP: the publication's sender side, and the hand-off to the sender.
        let mut udp = None;
        if let Some((endpoint_index, socket, sender_position, sender_limit, receivers, reader)) =
            udp_start
        {
            let endpoint_id = self.send_endpoints[endpoint_index].registration_id;
            // The sender position starts where the replay does, before the sender's
            // first cycle stores it from its reader, so that the limit the conductor
            // keeps from it never reads below the start.
            sender_position.store_release(start_position);
            udp = Some(UdpPublication {
                endpoint_id,
                sender_position: sender_position.clone(),
                sender_limit_id: sender_limit.id(),
                term_length: i64::from(term_length),
                connected: false,
                close_sent: false,
                sender_closed: false,
                flow_control,
                receivers_id: receivers.id(),
                position: None,
            });
            let add = sender::AddPublication {
                registration_id: replay_session_id,
                endpoint_id,
                session_id,
                stream_id,
                initial_term_id,
                term_length,
                mtu_length,
                reader,
                position: sender_position,
                limit: sender_limit,
                flow_control,
                receivers,
                receiver_table: receiver_table(flow_control),
            };
            self.hand_to_sender(endpoint_index, socket, add);
        }
        let over_udp = udp.is_some();
        self.publications.push(Publication {
            registration_id: replay_session_id,
            session_id,
            stream_id,
            exclusive: true,
            replay: true,
            log,
            position,
            limit,
            registrations: Vec::new(),
            consumer_position: start_position,
            clean_position: start_position,
            last_limit: start_position,
            connected_flag: false,
            producer: start_position,
            producer_loaded_at: now,
            stalled: None,
            closing: None,
            withdrawn: false,
            log_file_name,
            udp,
        });
        let publication_index = self.publications.len() - 1;
        {
            let entry = &mut self.replays[index];
            entry.state = ReplayState::Live;
            entry.position = Some(replay_position.clone());
            entry.unconnected_since = Some(now);
        }
        self.statics.replay_started.increment();
        self.replayer_request(replayer::Request::Attach {
            replay_session_id,
            writer,
            limit: limit_handle,
            position: replay_position,
        });
        self.send(&Event::ReplayReady {
            correlation_id: replay_session_id,
            replay_session_id,
            recording_id,
            start_position,
            session_id,
            stream_id,
        });
        // The images over `cavalry:ipc`; over UDP they are the receiving driver's, and a
        // receiver answering connects the publication through the sender's reply.
        if over_udp {
            return;
        }
        let matching: Vec<i64> = self
            .subscriptions
            .iter()
            .filter(|subscription| subscription.stream_id == stream_id)
            .map(|subscription| subscription.registration_id)
            .collect();
        for subscription_id in matching {
            self.add_image(publication_index, subscription_id, now);
        }
        if self
            .images
            .iter()
            .any(|image| image.publication_id == replay_session_id)
        {
            self.replays[index].unconnected_since = None;
        }
    }

    /// Undoes what `on_replay_ready` made before a step failed: the counters freed, the
    /// log dropped and its file removed, a send endpoint opened for the replay
    /// abandoned, and the replay failed with `code` (ADR 0030, ADR 0043).
    fn undo_replay_ready(
        &mut self,
        index: usize,
        counters: Vec<Counter>,
        log: Option<LogBuffers>,
        endpoint: Option<(usize, Option<SendSocket>)>,
        reuse: i64,
        (code, message): (ErrorCode, &str),
    ) {
        for counter in counters {
            let _ = self.counters.free(counter.id(), reuse);
        }
        drop(log);
        remove_log(&self.dir, self.replays[index].replay_session_id);
        if let Some((endpoint_index, socket)) = endpoint {
            self.abandon_send_endpoint(endpoint_index, socket.is_some(), reuse);
        }
        self.fail_replay(index, code, message);
    }

    /// A start that failed after the replayer took it: the client gets `code`, the
    /// entry fails and goes, and the replayer is told to forget the replay.
    fn fail_replay(&mut self, index: usize, code: ErrorCode, message: &str) {
        self.replays[index].state = ReplayState::Failed;
        let entry = self.replays.swap_remove(index);
        self.send_error(entry.replay_session_id, code, message);
        self.replayer_request(replayer::Request::Stop {
            replay_session_id: entry.replay_session_id,
            correlation_id: None,
            reason: ReplayStopReason::Failed,
        });
    }

    /// The replayer could not take the replay: the entry fails and the client gets
    /// code 21, 23 or 12, each counted in `replay.start_failed`, as the conductor's own
    /// code 24 is in `start_replay`.
    fn on_replay_start_failed(&mut self, replay_session_id: i64, failure: replayer::StartFailure) {
        if let Some(index) = self.replays.iter().position(|replay| {
            replay.replay_session_id == replay_session_id && replay.state == ReplayState::Starting
        }) {
            self.replays[index].state = ReplayState::Failed;
            self.replays.swap_remove(index);
        }
        self.statics.replay_start_failed.increment();
        match failure {
            replayer::StartFailure::UnknownRecording { recording_id, why } => self.send_error(
                replay_session_id,
                ErrorCode::UnknownRecording,
                &format!(
                    "recording {recording_id} is not in the catalog as a valid recording: {why}; \
                     `cavalry doctor` lists the recordings; see {ERRORS_DOC}"
                ),
            ),
            replayer::StartFailure::PositionOutOfRange {
                position,
                start,
                extent,
            } => self.send_error(
                replay_session_id,
                ErrorCode::PositionOutOfRange,
                &format!(
                    "position {position} is not a multiple of 32 from {start} to {extent}, the \
                     recording's retained position, its start until a purge moved it, and its \
                     extent; resolve an instant through the time index, or pass -1 for the \
                     retained position; see {ERRORS_DOC}"
                ),
            ),
            replayer::StartFailure::Files(text) => {
                self.send_error(replay_session_id, ErrorCode::StorageSpace, &text);
            }
        }
    }

    /// The replayer ended a replay: a command that asked gets `operation success`, the
    /// entry goes, its counter is freed after the reuse timeout, the publication closes
    /// at the position reached, as a removed publication does, every client gets
    /// `replay stopped`, and the end is counted by its reason. A reply for a replay the
    /// conductor no longer holds, a failed start's, answers the command alone.
    fn on_replay_stopped(
        &mut self,
        replay_session_id: i64,
        position: i64,
        reason: ReplayStopReason,
        correlation_id: Option<i64>,
        now: i64,
    ) {
        if let Some(correlation_id) = correlation_id {
            self.send(&Event::OperationSuccess { correlation_id });
        }
        let Some(index) = self
            .replays
            .iter()
            .position(|replay| replay.replay_session_id == replay_session_id)
        else {
            return;
        };
        self.replays[index].state = ReplayState::Stopped;
        let entry = self.replays.swap_remove(index);
        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
        if let Some(counter) = entry.position {
            let _ = self.counters.free(counter.id(), reuse);
        }
        let mut end = position;
        let mut close = None;
        if let Some(publication) = self
            .publications
            .iter_mut()
            .find(|publication| publication.registration_id == replay_session_id)
            && publication.closing.is_none()
        {
            end = publication.log.producer_position().unwrap_or(position);
            let _ = publication.log.set_end_of_stream_position(end);
            publication.closing = Some((end, now));
            if let Some(udp) = &mut publication.udp
                && !udp.close_sent
            {
                udp.close_sent = true;
                close = Some((publication.registration_id, end));
            }
        }
        // Over UDP the sender lingers the replay's publication as it lingers any: what
        // is left goes, heartbeats carry EOS, NAKs are answered, and its reply ends the
        // close (ADR 0041, ADR 0043).
        if let Some((registration_id, end_of_stream_position)) = close {
            self.sender_request(sender::Request::Close {
                registration_id,
                end_of_stream_position,
            });
        }
        self.statics.replay_stopped.increment();
        match reason {
            ReplayStopReason::Ended => self.statics.replay_ended.increment(),
            ReplayStopReason::NotConnected => self.statics.replay_not_connected.increment(),
            ReplayStopReason::Failed => self.statics.replay_failed.increment(),
            ReplayStopReason::Stopped => {}
        }
        self.send(&Event::ReplayStopped {
            replay_session_id,
            position: end,
            reason: reason.code(),
        });
    }

    /// The replay connect timeout (`spec/client-protocol.md`, "Liveness and timeouts"):
    /// a live replay whose publication has had no image for the timeout is stopped
    /// with reason 2; returns how many stops were asked for.
    fn keep_replays(&mut self, now: i64) -> usize {
        let timeout = spec::nanos(spec::REPLAY_CONNECT_TIMEOUT);
        let mut stops = Vec::new();
        for entry in &mut self.replays {
            if entry.state != ReplayState::Live {
                continue;
            }
            let id = entry.replay_session_id;
            // Over UDP the subscriber is a receiver answering the publication's setup
            // frames, as the sender last said (ADR 0043); over IPC, an image.
            let connected = match entry.udp {
                Some(_) => entry.connected,
                None => self.images.iter().any(|image| image.publication_id == id),
            };
            if connected {
                entry.unconnected_since = None;
                continue;
            }
            match entry.unconnected_since {
                None => entry.unconnected_since = Some(now),
                Some(since) if now.saturating_sub(since) >= timeout && !entry.stop_requested => {
                    entry.stop_requested = true;
                    stops.push(id);
                }
                Some(_) => {}
            }
        }
        for replay_session_id in &stops {
            self.replayer_request(replayer::Request::Stop {
                replay_session_id: *replay_session_id,
                correlation_id: None,
                reason: ReplayStopReason::NotConnected,
            });
        }
        stops.len()
    }

    /// Answers `start replay` with `error`, code 10, when the replayer's request queue
    /// is full or requests are still waiting for room in it.
    fn replayer_queue_full(&mut self, correlation_id: i64) {
        self.statics.registry_full.increment();
        self.send_error(
            correlation_id,
            ErrorCode::ResourceTemporarilyUnavailable,
            &format!(
                "the replayer's request queue holds {} and is full; try again once the \
                 replayer has caught up; see {ERRORS_DOC}",
                spec::REPLAYER_QUEUE_CAPACITY
            ),
        );
    }

    /// Answers a command with `error`, code 10, when a registry is full.
    fn registry_full(&mut self, correlation_id: i64, registry: &str, capacity: usize) {
        self.statics.registry_full.increment();
        self.send_error(
            correlation_id,
            ErrorCode::ResourceTemporarilyUnavailable,
            &registry_full_message(registry, capacity),
        );
    }

    /// `truncate recording`, `purge segments` and `purge recording`
    /// (`spec/client-protocol.md`, "Commands"; ADR 0031): the checks the conductor holds
    /// in memory, in the spec's order, then the registry entry and the first hop, the
    /// replayer's checks; nothing is written before those pass.
    fn retention(&mut self, correlation_id: i64, command: replayer::Retention) {
        let recording_id = command.recording_id();
        if recording_id < 0 || recording_id >= self.recorder.next_recording_id {
            self.send_error(
                correlation_id,
                ErrorCode::UnknownRecording,
                &format!(
                    "recording {recording_id} is not in the catalog, whose next id is {}; \
                     `cavalry doctor` lists the recordings; see {ERRORS_DOC}",
                    self.recorder.next_recording_id
                ),
            );
            return;
        }
        if self
            .recordings
            .iter()
            .any(|recording| recording.recording_id == Some(recording_id))
        {
            self.send_error(
                correlation_id,
                ErrorCode::RecordingBusy,
                &format!(
                    "recording {recording_id} is live or stopping; stop it with `cavalry record \
                     --stop {recording_id}` and wait for `recording stopped`, then send {} \
                     again; see {ERRORS_DOC}",
                    command.name()
                ),
            );
            return;
        }
        if self
            .replays
            .iter()
            .any(|replay| replay.recording_id == recording_id)
        {
            self.send_error(
                correlation_id,
                ErrorCode::RecordingBusy,
                &format!(
                    "recording {recording_id} is being read by a replay; stop the replay with \
                     `Cavalry::stop_replay` and its replay session id, or end the `cavalry \
                     replay` process that holds it, then send {} again; see {ERRORS_DOC}",
                    command.name()
                ),
            );
            return;
        }
        if let Some(other) = self
            .commands
            .iter()
            .find(|entry| entry.command.recording_id() == recording_id)
        {
            let name = other.command.name();
            self.send_error(
                correlation_id,
                ErrorCode::RecordingBusy,
                &format!(
                    "recording {recording_id} is under a {name} that is still in progress; wait \
                     for its answer, then send {} again; see {ERRORS_DOC}",
                    command.name()
                ),
            );
            return;
        }
        if self.commands.len() >= spec::MAX_COMMANDS_IN_PROGRESS {
            self.registry_full(
                correlation_id,
                "commands in progress",
                spec::MAX_COMMANDS_IN_PROGRESS,
            );
            return;
        }
        // The first hop alone is refused at admission: nothing has been written, and
        // the registry bounds what waits for the later hops.
        if !self.replayer_deferred.is_empty() {
            self.replayer_queue_full(correlation_id);
            return;
        }
        if self
            .replayer
            .requests
            .offer(replayer::Request::Check {
                correlation_id,
                command,
            })
            .is_err()
        {
            self.replayer_queue_full(correlation_id);
            return;
        }
        self.wake_replayer();
        self.commands.push(CommandEntry {
            correlation_id,
            command,
            hop: Hop::Checking,
            pending: None,
            failure: None,
        });
    }

    /// The entry of the command `correlation_id`, if it is still in progress.
    fn command_index(&self, correlation_id: i64) -> Option<usize> {
        self.commands
            .iter()
            .position(|entry| entry.correlation_id == correlation_id)
    }

    /// Offers a command's next hop, or keeps it on the entry for the next cycle.
    fn offer_hop(&mut self, index: usize, hop: PendingHop) {
        let kept = match hop {
            PendingHop::Recorder(request) => match self.recorder.requests.offer(request) {
                Ok(()) => return,
                Err(request) => PendingHop::Recorder(request),
            },
            PendingHop::Replayer(request) => match self.replayer.requests.offer(request) {
                Ok(()) => {
                    self.wake_replayer();
                    return;
                }
                Err(request) => PendingHop::Replayer(request),
            },
        };
        self.commands[index].pending = Some(kept);
    }

    /// Offers the hops the queues had no room for, in admission order, stopping at the
    /// first the queue still refuses; returns how many went.
    fn flush_commands(&mut self) -> usize {
        let mut sent = 0;
        for index in 0..self.commands.len() {
            let Some(hop) = self.commands[index].pending.take() else {
                continue;
            };
            self.offer_hop(index, hop);
            if self.commands[index].pending.is_some() {
                break;
            }
            sent += 1;
        }
        sent
    }

    /// The replayer's checks passed: the first catalog write goes to the recorder.
    fn on_checked(&mut self, correlation_id: i64) {
        let Some(index) = self.command_index(correlation_id) else {
            return;
        };
        let command = self.commands[index].command;
        let write = match command {
            replayer::Retention::Truncate { .. } => crate::recorder::CatalogWrite::OpenCut,
            replayer::Retention::Purge { position, .. } => {
                crate::recorder::CatalogWrite::Retain { position }
            }
            replayer::Retention::Delete { .. } => crate::recorder::CatalogWrite::Delete,
        };
        self.commands[index].hop = Hop::CatalogFirst;
        self.offer_hop(
            index,
            PendingHop::Recorder(Request::Catalog {
                correlation_id,
                recording_id: command.recording_id(),
                write,
            }),
        );
    }

    /// The replayer's checks failed: the error, by the spec's code, and nothing written.
    fn on_check_failed(&mut self, correlation_id: i64, failure: replayer::CheckFailure) {
        let Some(index) = self.command_index(correlation_id) else {
            return;
        };
        let command = self.commands.swap_remove(index).command;
        match failure {
            replayer::CheckFailure::UnknownRecording { recording_id, why } => self.send_error(
                correlation_id,
                ErrorCode::UnknownRecording,
                &format!(
                    "recording {recording_id} is not in the catalog as a valid recording: {why}; \
                     `cavalry doctor` lists the recordings; see {ERRORS_DOC}"
                ),
            ),
            replayer::CheckFailure::Busy { recording_id } => self.send_error(
                correlation_id,
                ErrorCode::RecordingBusy,
                &format!(
                    "recording {recording_id} reads as live or being cut in the catalog; wait \
                     for the recording to stop or the cut to finish, then send {} again; see \
                     {ERRORS_DOC}",
                    command.name()
                ),
            ),
            replayer::CheckFailure::PositionOutOfRange {
                position,
                retained,
                stop,
                rule,
            } => self.send_error(
                correlation_id,
                ErrorCode::PositionOutOfRange,
                &format!(
                    "position {position} is not {rule}, {retained} to {stop} for recording {}; \
                     resolve an instant through the time index, or pass a position within them; \
                     see {ERRORS_DOC}",
                    command.recording_id()
                ),
            ),
            replayer::CheckFailure::NoFrame { position } => self.send_error(
                correlation_id,
                ErrorCode::PositionOutOfRange,
                &format!(
                    "no frame begins at the position {position} of recording {}; pass a position \
                     an instant resolves to, or the stop position; see {ERRORS_DOC}",
                    command.recording_id()
                ),
            ),
            replayer::CheckFailure::Files(text) => {
                self.statics.recording_retention_failed.increment();
                self.send_error(correlation_id, ErrorCode::StorageSpace, &text);
            }
        }
    }

    /// A catalog write for a command is done or failed: the next hop, the answer, or
    /// the error.
    fn on_catalog_done(
        &mut self,
        correlation_id: i64,
        recording_id: i64,
        write: crate::recorder::CatalogWrite,
        result: Result<(), String>,
        repaired: Recovery,
    ) {
        let Some(index) = self.command_index(correlation_id) else {
            return;
        };
        let command = self.commands[index].command;
        debug_assert_eq!(command.recording_id(), recording_id);
        // The catalog side of a repair is a transition the counters record, as the
        // start-up pass's are (invariant 4).
        self.add_recovery(&repaired);
        match (self.commands[index].hop, write, result) {
            (Hop::CatalogFirst, _, Ok(())) => {
                self.commands[index].hop = Hop::FileWork;
                self.offer_hop(
                    index,
                    PendingHop::Replayer(replayer::Request::FileWork {
                        correlation_id,
                        command,
                    }),
                );
            }
            (Hop::CatalogLast, _, Ok(())) => {
                self.commands.swap_remove(index);
                self.statics.recording_truncated.increment();
                self.send(&Event::OperationSuccess { correlation_id });
            }
            (Hop::RepairSlot, _, repaired) => {
                let entry = self.commands.swap_remove(index);
                let failure = entry.failure.unwrap_or_default();
                let repaired = match repaired {
                    Ok(()) => "the slot was repaired from the files and agrees with them",
                    Err(_) => {
                        "the repair of the slot from the files failed too; `cavalry doctor` \
                               names what it found"
                    }
                };
                self.send_error(
                    correlation_id,
                    ErrorCode::StorageSpace,
                    &format!(
                        "{failure}; {repaired}; look at the file and the filesystem, then send {} \
                         again; see {ERRORS_DOC}",
                        command.name()
                    ),
                );
            }
            (_, _, Err(text)) => {
                // A catalog write that fails can leave the slot between two of its
                // writes, a truncate's at "live, or being cut" with the files already
                // cut: the same repair as after a file error puts the slot and the
                // files in agreement before the refusal is answered, and the spec
                // sentence that names it is the third change's.
                self.statics.recording_retention_failed.increment();
                self.commands[index].hop = Hop::RepairFiles;
                self.commands[index].failure = Some(format!(
                    "the catalog write of {} for recording {recording_id} failed: {text}",
                    command.name()
                ));
                self.offer_hop(
                    index,
                    PendingHop::Replayer(replayer::Request::Repair {
                        correlation_id,
                        recording_id,
                    }),
                );
            }
            (hop, _, Ok(())) => {
                debug_assert!(false, "a catalog reply at hop {hop:?}");
            }
        }
    }

    /// The replayer's file work ended: a truncate's last catalog write, a purge's
    /// answer, whose files the replayer counted as each went, a delete's answer with
    /// its counter, or the repair after a file the operating system refused.
    fn on_file_work_done(
        &mut self,
        correlation_id: i64,
        command: replayer::Retention,
        outcome: Result<replayer::FileWorkDone, String>,
    ) {
        let Some(index) = self.command_index(correlation_id) else {
            return;
        };
        match outcome {
            Ok(done) => match command {
                replayer::Retention::Truncate {
                    recording_id,
                    position,
                } => {
                    self.commands[index].hop = Hop::CatalogLast;
                    self.offer_hop(
                        index,
                        PendingHop::Recorder(Request::Catalog {
                            correlation_id,
                            recording_id,
                            write: crate::recorder::CatalogWrite::CloseCut {
                                stop_position: position,
                                stop_timestamp: done.stop_timestamp,
                            },
                        }),
                    );
                }
                replayer::Retention::Purge { .. } => {
                    // The replayer counted each file in `recording.segments_purged` as
                    // it went.
                    self.commands.swap_remove(index);
                    self.send(&Event::OperationSuccess { correlation_id });
                }
                replayer::Retention::Delete { .. } => {
                    self.commands.swap_remove(index);
                    self.statics.recording_deleted.increment();
                    self.send(&Event::OperationSuccess { correlation_id });
                }
            },
            Err(text) => {
                self.statics.recording_retention_failed.increment();
                self.commands[index].hop = Hop::RepairFiles;
                self.commands[index].failure = Some(text);
                self.offer_hop(
                    index,
                    PendingHop::Replayer(replayer::Request::Repair {
                        correlation_id,
                        recording_id: command.recording_id(),
                    }),
                );
            }
        }
    }

    /// The file side of a slot's repair ran on the replayer: its counts move, and the
    /// recorder makes the write it ended in; or the files refused the repair too, and
    /// the command is answered with its file error and that.
    fn on_repaired(
        &mut self,
        correlation_id: i64,
        recording_id: i64,
        outcome: Result<(cavalry_log::dir::SlotWrite, Recovery), String>,
    ) {
        let Some(index) = self.command_index(correlation_id) else {
            return;
        };
        debug_assert_eq!(self.commands[index].command.recording_id(), recording_id);
        match outcome {
            Ok((write, recovery)) => {
                self.add_recovery(&recovery);
                self.commands[index].hop = Hop::RepairSlot;
                self.offer_hop(
                    index,
                    PendingHop::Recorder(Request::Catalog {
                        correlation_id,
                        recording_id,
                        write: crate::recorder::CatalogWrite::Repair(write),
                    }),
                );
            }
            Err(_) => {
                let entry = self.commands.swap_remove(index);
                let failure = entry.failure.unwrap_or_default();
                self.send_error(
                    correlation_id,
                    ErrorCode::StorageSpace,
                    &format!(
                        "{failure}; the repair of the slot from the files failed too; `cavalry \
                         doctor` names what it found; look at the file and the filesystem, \
                         then send {} again; see {ERRORS_DOC}",
                        entry.command.name()
                    ),
                );
            }
        }
    }

    /// Adds what a repair's two sides did to the recovery counters, which the start-up
    /// pass set: each field is a transition (invariant 4).
    fn add_recovery(&self, recovery: &Recovery) {
        let add = |id: CounterId, value: u64| {
            if value > 0
                && let Ok(counter) = self.counters.counter(id.record())
            {
                counter.add(i64::try_from(value).unwrap_or(i64::MAX));
            }
        };
        add(RECORDING_RECOVERED, recovery.recovered);
        add(RECORDING_INVALIDATED, recovery.invalidated);
        add(RECORDING_INDEX_REWRITTEN, recovery.index_rewritten);
        add(RECORDING_FILES_REMOVED, recovery.files_removed);
    }

    /// Answers `start recording` or `sync recording` with `error`, code 10, when the
    /// recorder's request queue is full or requests are still waiting for room in it.
    fn recorder_queue_full(&mut self, correlation_id: i64) {
        self.statics.registry_full.increment();
        self.send_error(
            correlation_id,
            ErrorCode::ResourceTemporarilyUnavailable,
            &format!(
                "the recorder's request queue holds {} and is full; try again once the \
                 recorder has caught up; see {ERRORS_DOC}",
                spec::RECORDER_QUEUE_CAPACITY
            ),
        );
    }

    /// Answers a command with `error`, code 10, when the counters file has no free record.
    fn counters_full(&mut self, correlation_id: i64, error: &CountersError) {
        self.statics.registry_full.increment();
        self.send_error(
            correlation_id,
            ErrorCode::ResourceTemporarilyUnavailable,
            &format!("{error}"),
        );
    }

    /// Counts a refused command and answers it with the code the spec names, when the
    /// message carried a correlation id to answer.
    fn refuse(&mut self, error: ProtocolError, message: &[u8]) {
        self.statics.rejected_command.increment();
        if let Some(code) = error.error_code() {
            let correlation_id = correlation_id_of(message);
            let text = format!("{error}");
            self.send_error(correlation_id, code, &text);
        }
    }

    /// Sends an `error` event and counts it.
    fn send_error(&mut self, correlation_id: i64, code: ErrorCode, message: &str) {
        let bytes = message.as_bytes();
        let message = &bytes[..bytes.len().min(MAX_STRING_LENGTH)];
        let event = Event::Error {
            offending_correlation_id: correlation_id,
            error_code: code.code(),
            error_message: message,
        };
        self.send(&event);
    }

    /// Transmits one event. The only transmit errors are a type below 1 and a message
    /// over capacity ÷ 8, which no event here produces; one would be a bug, and is
    /// counted among the errors rather than hidden.
    fn send(&mut self, event: &Event<'_>) {
        if matches!(event, Event::Error { .. }) {
            self.statics.errors.increment();
        }
        let Ok(length) = event.write(&mut self.scratch) else {
            self.statics.errors.increment();
            return;
        };
        if self
            .transmitter
            .transmit(event.event_type() as i32, &self.scratch[..length])
            .is_err()
        {
            self.statics.errors.increment();
        }
    }
}

/// Why a record of `length` bytes, longer than [`MAX_COMMAND_LENGTH`], is refused, decided
/// from its first [`MAX_COMMAND_LENGTH`] bytes in the spec's order
/// (`spec/client-protocol.md`, "What the driver rejects"). The prefix holds the head and
/// every fixed field and string of a legal command, so `too-short`, `type` and `string`
/// come out as they would from the whole record; what is left is `length`, reported with
/// the record's own length.
fn over_long(type_id: i32, prefix: &[u8], length: usize) -> ProtocolError {
    let prefix = &prefix[..prefix.len().min(MAX_COMMAND_LENGTH)];
    match Command::parse(type_id, prefix) {
        Ok(_) => ProtocolError::Length {
            given: length,
            expected: prefix.len(),
        },
        Err(ProtocolError::Length { expected, .. }) => ProtocolError::Length {
            given: length,
            expected,
        },
        // The bytes left after the string's length field, in the whole record.
        Err(ProtocolError::String {
            length: string,
            available,
        }) => ProtocolError::String {
            length: string,
            available: available.saturating_add(length.saturating_sub(prefix.len())),
        },
        Err(error) => error,
    }
}

/// The correlation id of a message that has at least its first 16 bytes, for an error
/// event; 0 when it has not, which never reaches an event.
fn correlation_id_of(message: &[u8]) -> i64 {
    message
        .get(8..16)
        .and_then(|bytes| bytes.try_into().ok())
        .map_or(0, i64::from_le_bytes)
}

/// The key of a position counter: correlation id, session id, stream id, little-endian.
/// Resolves `host` and `port` once, on the control plane, to the first address the
/// system gives (`spec/udp.md`, "The channel URI"); the system's reason when it gives
/// none, for error code 9.
fn resolve(host: &str, port: u16) -> Result<SocketAddr, String> {
    match (host, port).to_socket_addrs() {
        Ok(mut addresses) => addresses
            .next()
            .ok_or_else(|| "the name resolves to no address".to_string()),
        Err(error) => Err(error.to_string()),
    }
}

/// The message of error code 10 for a registry at capacity.
fn registry_full_message(registry: &str, capacity: usize) -> String {
    format!(
        "the driver's {registry} registry holds {capacity} and is full; remove what is not \
         used; see {ERRORS_DOC}"
    )
}

/// The message of error code 10 for a counters file that gave fewer records than it
/// was asked for: a case that cannot happen, answered rather than left behind.
fn counters_miscounted_message() -> String {
    format!(
        "the counters file gave fewer records than were asked for; try the command again, \
         and report it; see {ERRORS_DOC}"
    )
}

/// A replay's counters as `undo_replay_ready` takes them, cloned: the publication's
/// two, `replay.position`, and over UDP the sender's two.
fn replay_counters(
    position: &Counter,
    limit: &Counter,
    replay_position: &Counter,
    sender: Option<&(Counter, Counter, Counter)>,
) -> Vec<Counter> {
    let mut held = vec![position.clone(), limit.clone(), replay_position.clone()];
    if let Some((sender_position, sender_limit, receivers)) = sender {
        held.push(sender_position.clone());
        held.push(sender_limit.clone());
        held.push(receivers.clone());
    }
    held
}

/// A `min` publication's receivers table with its room, allocated on the control plane
/// so that the sender never grows it; no room under `max`, which keeps no table
/// (`spec/udp.md`, "The sender"; ADR 0053).
fn receiver_table(flow_control: FlowControl) -> Vec<sender::Receiver> {
    Vec::with_capacity(match flow_control {
        FlowControl::Min => MAX_FLOW_CONTROL_RECEIVERS,
        FlowControl::Max => 0,
    })
}

/// Why a destination could not be resolved to an address the control socket sends to
/// (`resolve_in_family`).
enum Unreachable {
    /// The name resolves to no address at all: code 9.
    NoAddress(String),
    /// It resolves, but to no address of the control address's family, or to none
    /// that is a loopback address beside a loopback control address: code 1, reason
    /// `destination`.
    Family(String),
}

/// A destination's endpoint resolved to the first address of the control address's
/// family, IPv4 or IPv6, that the resolver returns, a loopback one when the control
/// address is a loopback address, since a socket bound to the control address sends
/// to no other (`spec/client-protocol.md`, "Commands", add destination; ADR 0053).
fn resolve_in_family(
    host: &str,
    port: u16,
    control: SocketAddr,
) -> Result<SocketAddr, Unreachable> {
    let addresses = match (host, port).to_socket_addrs() {
        Ok(addresses) => addresses,
        Err(error) => return Err(Unreachable::NoAddress(error.to_string())),
    };
    let loopback = control.ip().is_loopback();
    let family = if control.is_ipv4() { "IPv4" } else { "IPv6" };
    let mut any = false;
    let mut of_family = false;
    for address in addresses {
        any = true;
        if address.is_ipv4() != control.is_ipv4() {
            continue;
        }
        of_family = true;
        if loopback && !address.ip().is_loopback() {
            continue;
        }
        return Ok(address);
    }
    if !any {
        Err(Unreachable::NoAddress(
            "the name resolves to no address".to_string(),
        ))
    } else if !of_family {
        Err(Unreachable::Family(format!(
            "it has no {family} address, the control address's family"
        )))
    } else {
        Err(Unreachable::Family(
            "it is not a loopback address, and a socket bound to a loopback control address \
             sends to no other host"
                .to_string(),
        ))
    }
}

/// The message of error code 10 for the sender's request queue with no room, or with
/// requests still waiting for it (ADR 0041).
fn sender_queue_full_message() -> String {
    format!(
        "the sender's request queue holds {} and is full; try again once the sender has \
         caught up; see {ERRORS_DOC}",
        spec::SENDER_QUEUE_CAPACITY
    )
}

/// The session id this driver's first publication takes: a random i32, as
/// `spec/client-protocol.md`, "Commands", says ("it starts at a random i32"), drawn
/// through the standard library's `RandomState`, whose keys come from the operating
/// system, hashing the clock; no dependency. Random and not the clock alone: two
/// drivers started in one instant, a cluster's members under one supervisor, would
/// take the same ids from a clock, and a receiver that hears two publications of one
/// stream under one session id on an endpoint holds them as one session, so the second
/// is never answered (`spec/udp.md`, "The receiver", step 1).
fn first_session_id(now: i64) -> i32 {
    use std::hash::{BuildHasher, RandomState};
    RandomState::new().hash_one(now) as i32
}

fn position_key(correlation_id: i64, session_id: i32, stream_id: i32) -> [u8; 16] {
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&correlation_id.to_le_bytes());
    key[8..12].copy_from_slice(&session_id.to_le_bytes());
    key[12..].copy_from_slice(&stream_id.to_le_bytes());
    key
}

/// A counter's id as the i32 an event carries.
fn counter_id(counter: &Counter) -> i32 {
    i32::try_from(counter.id()).unwrap_or(i32::MAX)
}

/// Removes a publication's log file; a file already gone is not an error.
fn remove_log(dir: &Path, registration_id: i64) {
    let _ = std::fs::remove_file(dir.join(log_layout::file_name(registration_id)));
}

/// Removes an image's log file; a file already gone is not an error.
fn remove_image_log(dir: &Path, registration_id: i64) {
    let _ = std::fs::remove_file(dir.join(log_layout::image_file_name(registration_id)));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary driver directory, removed on drop so a failed assert does not leave it.
    pub(super) struct TestDir(PathBuf);

    impl TestDir {
        pub(super) fn new(test: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("cavalry-driver-{}-{test}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            TestDir(path)
        }

        pub(super) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The pid a starter names when it is refused the lock: what the holder wrote, or 0
    /// for a file the holder has not written yet, or that holds no number.
    #[test]
    fn the_holders_pid_is_read_from_the_lock_file_or_is_zero() {
        let dir = TestDir::new("lock-pid");
        std::fs::create_dir_all(dir.path()).expect("the directory");
        let lock = dir.path().join(PID_FILE_NAME);
        assert_eq!(holder_pid(&lock), 0, "no file");
        std::fs::write(&lock, b"").expect("writes");
        assert_eq!(holder_pid(&lock), 0, "an empty file");
        std::fs::write(&lock, b"4242\n").expect("writes");
        assert_eq!(holder_pid(&lock), 4242);
        std::fs::write(&lock, b"not a pid\n").expect("writes");
        assert_eq!(holder_pid(&lock), 0, "no number");
    }

    /// Two starters on one directory: the second is refused while the first holds the
    /// lock, with the first's pid and nothing removed, and takes the directory once
    /// the first has let go, removing what it left but the lock.
    #[test]
    fn a_starter_is_refused_while_another_holds_the_lock_and_takes_over_when_it_lets_go() {
        let dir = TestDir::new("lock-held");
        let held = prepare_dir(dir.path()).expect("the first takes the directory");
        std::fs::write(dir.path().join("counters"), b"the first's file").expect("writes");
        match prepare_dir(dir.path()) {
            Err(DriverError::Active { pid, .. }) => {
                assert_eq!(pid, i64::from(std::process::id()));
            }
            other => panic!("refused as driver-active: {other:?}"),
        }
        assert!(dir.path().join("counters").is_file(), "nothing was removed");
        drop(held);
        let taken = prepare_dir(dir.path()).expect("the second takes the directory");
        assert!(
            !dir.path().join("counters").exists(),
            "what the first left is gone"
        );
        assert_eq!(
            holder_pid(&dir.path().join(PID_FILE_NAME)),
            i64::from(std::process::id())
        );
        drop(taken);
    }

    /// `spec/client-protocol.md`, "Commands": a driver's session ids start at a random
    /// i32. The first id came from the clock alone, so two drivers started in one
    /// instant took the same ids, and a receiver that heard one stream from both held
    /// them as one session and never answered the second. One clock value now gives a
    /// different id at every draw; sixteen draws of an i32 repeat one in 36 million
    /// runs.
    #[test]
    fn the_first_session_id_is_random_and_not_the_clock() {
        let now = now_nanos();
        let mut ids: Vec<i32> = (0..16).map(|_| first_session_id(now)).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 16, "one instant gave the same id twice: {ids:?}");
    }

    /// ADR 0022: the conductor loads a publication's producer position at most once per
    /// [`spec::PRODUCER_POSITION_INTERVAL`] by the clock it is given, and a move of that
    /// position is not work, so a publisher alone lets the conductor idle.
    #[test]
    fn the_producer_position_is_loaded_once_per_interval_and_its_move_is_not_work() {
        let dir = TestDir::new("producer-cadence");
        prepare_dir(dir.path()).expect("the directory is prepared");
        let (requests, _requests) = spsc::queue::<Request>(4);
        let (_replies, replies) = spsc::queue::<Reply>(4);
        let link = RecorderLink {
            requests,
            replies,
            next_recording_id: 0,
        };
        let (requests, _replayer_requests) = spsc::queue::<replayer::Request>(4);
        let (_replayer_replies, replies) = spsc::queue::<replayer::Reply>(4);
        let replayer_link = ReplayerLink { requests, replies };
        let (requests, _sender_requests) = spsc::queue::<sender::Request>(4);
        let (_sender_replies, replies) = spsc::queue::<sender::Reply>(4);
        let sender_link = SenderLink { requests, replies };
        let (requests, _receiver_requests) = spsc::queue::<receiver::Request>(4);
        let (_receiver_replies, replies) = spsc::queue::<receiver::Reply>(4);
        let receiver_link = ReceiverLink { requests, replies };
        let mut conductor = Conductor::create(
            dir.path(),
            IdleStrategy::Backoff,
            link,
            replayer_link,
            sender_link,
            receiver_link,
        )
        .expect("the conductor is created");
        let t0 = now_nanos();
        let interval = spec::nanos(spec::PRODUCER_POSITION_INTERVAL);

        // A subscriber, then a publication, loaded at t0; the first cycle raises the
        // limit, and after it nothing moves.
        conductor.add_subscription(1, 1, 7, IPC_CHANNEL, t0);
        conductor.add_publication(1, 2, 7, IPC_CHANNEL, false, t0);
        assert_eq!(conductor.publications.len(), 1, "the publication is added");
        assert_eq!(conductor.images.len(), 1, "the subscriber has an image");
        conductor.duty_cycle(t0);
        assert_eq!(conductor.duty_cycle(t0), 0, "nothing moves once settled");
        let joined = conductor.publications[0].position.get();

        // One frame committed through an appender, as a publisher would.
        let committed = {
            let publication = &conductor.publications[0];
            let mut appender = publication.log.appender().expect("an appender");
            appender
                .offer(b"one message", publication.limit.get(), 0)
                .expect("the message is offered")
        };
        assert!(committed > joined, "the producer moved");

        // Half the interval after the last load: no load, the gauge stays, and the
        // publication is no work.
        assert_eq!(
            conductor.duty_cycle(t0 + interval / 2),
            0,
            "no work before the interval"
        );
        assert_eq!(
            conductor.publications[0].position.get(),
            joined,
            "the producer position is not loaded before the interval has passed"
        );

        // The interval after the last load: one load, the gauge follows, and its move is
        // still no work.
        assert_eq!(
            conductor.duty_cycle(t0 + interval),
            0,
            "a moved gauge is not work"
        );
        assert_eq!(
            conductor.publications[0].position.get(),
            committed,
            "the gauge follows the publisher once the interval has passed"
        );

        // The conductor unmaps its files before the guard removes the directory.
        drop(conductor);
    }
}

#[cfg(test)]
mod recording {
    //! The conductor and the recorder driven by hand, duty cycle by duty cycle, on a
    //! clock the test advances, with a publisher appending straight into the log: the
    //! full cases of invariant 3 first, then the seams.

    use std::fs::OpenOptions;
    use std::os::unix::fs::FileExt;

    use cavalry_log::catalog::CatalogError;
    use cavalry_log::dir::LogDir;
    use cavalry_log::layout::locate;
    use cavalry_log::recorder::RecorderError;
    use cavalry_proto::frame::{DataFrameHeader, FrameType, HEADER_LENGTH as FRAME_HEADER};
    use cavalry_shm::broadcast::{BroadcastBuffer, Receiver};
    use cavalry_shm::counters::generated::{
        DRIVER_REGISTRY_FULL, LOG_BUFFER_REJECTED_FRAME, RECORDING_CATALOG_FULL,
        RECORDING_START_FAILED, RECORDING_STARTED, RECORDING_STOPPED, RECORDING_WRITE_FAILED,
    };
    use cavalry_shm::log::ClaimError;

    use super::tests::TestDir;
    use super::*;
    use crate::recorder::{PENDING_REPLIES, Recorder, start_failure};
    use crate::replayer::Replayer;

    const MS: i64 = 1_000_000;
    const WINDOW: i64 = spec::IPC_PUBLICATION_WINDOW as i64;

    /// The three threads' state, with no thread: the test calls each duty cycle.
    pub(super) struct Harness {
        pub(super) dir: TestDir,
        pub(super) log_dir: PathBuf,
        pub(super) conductor: Conductor,
        pub(super) recorder: Recorder,
        pub(super) replayer: Replayer,
        /// The sender, driven by hand as the recorder and the replayer are.
        pub(super) sender: sender::Sender,
        /// The receiver, driven by hand the same way.
        pub(super) udp_receiver: receiver::Receiver,
        /// A client's view of the events, attached before the first command.
        pub(super) receiver: Receiver,
        pub(super) now: i64,
    }

    impl Harness {
        pub(super) fn new(test: &str) -> Self {
            let dir = TestDir::new(test);
            prepare_dir(dir.path()).expect("the directory is prepared");
            let log_dir = dir.path().join("log");
            let now = now_nanos();
            let (log, recovery) = LogDir::open(&log_dir, now).expect("the log directory opens");
            assert_eq!(recovery, cavalry_log::dir::Recovery::default());
            let (requests, request_consumer) =
                spsc::queue::<Request>(spec::RECORDER_QUEUE_CAPACITY);
            let (reply_producer, replies) = spsc::queue::<Reply>(spec::RECORDER_QUEUE_CAPACITY);
            let link = RecorderLink {
                requests,
                replies,
                next_recording_id: 0,
            };
            let (replayer_requests, replayer_request_consumer) =
                spsc::queue::<replayer::Request>(spec::REPLAYER_QUEUE_CAPACITY);
            let (replayer_reply_producer, replayer_replies) =
                spsc::queue::<replayer::Reply>(spec::REPLAYER_QUEUE_CAPACITY);
            let replayer_link = ReplayerLink {
                requests: replayer_requests,
                replies: replayer_replies,
            };
            let (sender_requests, sender_request_consumer) =
                spsc::queue::<sender::Request>(spec::SENDER_QUEUE_CAPACITY);
            let (sender_reply_producer, sender_replies) =
                spsc::queue::<sender::Reply>(spec::SENDER_QUEUE_CAPACITY);
            let sender_link = SenderLink {
                requests: sender_requests,
                replies: sender_replies,
            };
            let (receiver_requests, receiver_request_consumer) =
                spsc::queue::<receiver::Request>(spec::RECEIVER_QUEUE_CAPACITY);
            let (receiver_reply_producer, receiver_replies) =
                spsc::queue::<receiver::Reply>(spec::RECEIVER_QUEUE_CAPACITY);
            let receiver_link = ReceiverLink {
                requests: receiver_requests,
                replies: receiver_replies,
            };
            let conductor = Conductor::create(
                dir.path(),
                IdleStrategy::Backoff,
                link,
                replayer_link,
                sender_link,
                receiver_link,
            )
            .expect("the conductor is created");
            let receiver = BroadcastBuffer::open(dir.path(), false)
                .expect("the buffer opens")
                .receiver()
                .expect("a receiver");
            let replayer = Replayer::new(
                &log_dir,
                replayer_request_consumer,
                replayer_reply_producer,
                conductor
                    .counter(LOG_BUFFER_REJECTED_FRAME)
                    .expect("a counter"),
                conductor
                    .counter(cavalry_shm::counters::generated::RECORDING_SEGMENTS_PURGED)
                    .expect("a counter"),
                IdleStrategy::Backoff,
            )
            .expect("the replayer opens the catalog");
            let recorder = Recorder::new(
                log,
                request_consumer,
                reply_producer,
                conductor
                    .counter(RECORDING_WRITE_FAILED)
                    .expect("a counter"),
                conductor
                    .counter(LOG_BUFFER_REJECTED_FRAME)
                    .expect("a counter"),
                IdleStrategy::Backoff,
            );
            let sender = sender::Sender::new(
                sender_request_consumer,
                sender_reply_producer,
                sender::Statics::new(|id| conductor.counter(id)).expect("the sender's counters"),
                IdleStrategy::Backoff,
            );
            let udp_receiver = receiver::Receiver::new(
                receiver_request_consumer,
                receiver_reply_producer,
                receiver::Statics::new(|id| conductor.counter(id))
                    .expect("the receiver's counters"),
                IdleStrategy::Backoff,
            );
            Harness {
                dir,
                log_dir,
                conductor,
                recorder,
                replayer,
                sender,
                udp_receiver,
                receiver,
                now,
            }
        }

        /// One cycle of each, a millisecond later.
        pub(super) fn cycle(&mut self) {
            self.now += MS;
            self.conductor.duty_cycle(self.now);
            self.recorder.duty_cycle(self.now);
            self.replayer.duty_cycle(self.now);
            self.udp_receiver.duty_cycle(self.now);
            self.conductor.duty_cycle(self.now);
        }

        /// Cycles until `condition` holds, a millisecond of the test's clock and of
        /// real time per cycle, for up to five seconds of real time: a datagram sent on
        /// the loopback lands when the kernel delivers it, not when `send_to` returns.
        /// Returns whether it held.
        pub(super) fn cycle_until(
            &mut self,
            mut condition: impl FnMut(&mut Harness) -> bool,
        ) -> bool {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                self.cycle();
                if condition(self) {
                    return true;
                }
                if std::time::Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        /// Cycles for fifty milliseconds of real time, so that a datagram that should
        /// change nothing has had its chance to arrive.
        pub(super) fn settle(&mut self) {
            for _ in 0..50 {
                self.cycle();
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        pub(super) fn counter(&self, id: CounterId) -> i64 {
            self.conductor.counter(id).expect("a counter").get()
        }

        /// The last error event with `correlation_id` among the events not yet read,
        /// as (code, message).
        pub(super) fn last_error(&mut self, correlation_id: i64) -> Option<(i32, String)> {
            let mut found = None;
            while self
                .receiver
                .receive(|type_id, message| {
                    if let Ok(Event::Error {
                        offending_correlation_id,
                        error_code,
                        error_message,
                    }) = Event::parse(type_id, message)
                        && offending_correlation_id == correlation_id
                    {
                        found = Some((
                            error_code,
                            String::from_utf8_lossy(error_message).into_owned(),
                        ));
                    }
                })
                .expect("the receiver reads")
            {}
            found
        }
    }

    // ---- cavalry:udp publications: the conductor's side of the sender (ADR 0041) ------

    use std::time::Instant;

    use cavalry_proto::udp::{
        CONNECTION_TIMEOUT, ControlFrame, Datagram, FLOW_CONTROL_RECEIVER_TIMEOUT, MAX_TERM_LENGTH,
        RTT_MEASUREMENT_REPLY, RttMeasurement, SETUP_INTERVAL, STATUS_MESSAGE_END_OF_STREAM,
        SetupFrame, StatusMessage, parse as parse_udp,
    };
    use cavalry_shm::counters::generated::{
        COUNTERS_ALLOCATED, COUNTERS_FREED, PUBLICATION_CONNECTED, PUBLICATION_DISCONNECTED,
        SENDER_BYTES_SENT, SENDER_FLOW_CONTROL_RECEIVERS_FULL,
        SENDER_FLOW_CONTROL_RECEIVERS_TIMED_OUT, SENDER_FRAMES_SENT, SENDER_RETRANSMITS_SENT,
        SENDER_RTT_REPLIES_SENT, SENDER_SETUPS_SENT, SENDER_STATUS_MESSAGES_RECEIVED,
    };

    /// A receiver by hand: a socket of its own that reads what the sender puts on the
    /// wire and answers as `spec/udp.md`, "The receiver" says a receiver answers. The
    /// replay module's tests drive a replay over UDP against it too.
    pub(super) struct TestReceiver {
        socket: std::net::UdpSocket,
        buffer: Vec<u8>,
        peer: Option<std::net::SocketAddr>,
    }

    impl TestReceiver {
        pub(super) fn bind() -> Self {
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("a loopback socket");
            socket
                .set_read_timeout(Some(Duration::from_millis(500)))
                .expect("a read timeout");
            Self {
                socket,
                buffer: vec![0u8; 2048],
                peer: None,
            }
        }

        pub(super) fn channel(&self) -> Vec<u8> {
            format!(
                "cavalry:udp?endpoint={}",
                self.socket.local_addr().expect("bound")
            )
            .into_bytes()
        }

        /// The next datagram as a control frame, or `None` at the read timeout.
        pub(super) fn next_control(&mut self) -> Option<ControlFrame> {
            let (length, from) = self.socket.recv_from(&mut self.buffer).ok()?;
            self.peer = Some(from);
            parse_udp(&self.buffer[..length], spec::UDP_TERM_LENGTH).ok()
        }

        pub(super) fn send_status_message(&self, message: &StatusMessage) {
            let bytes = message.to_bytes();
            self.socket
                .send_to(
                    &bytes[..message.frame_length()],
                    self.peer.expect("a setup frame arrived first"),
                )
                .expect("the status message is sent");
        }

        /// The next datagram as its data frames, (header, body) in order, checked
        /// against `term_length`; an empty list for a control frame or a heartbeat;
        /// `None` at the read timeout.
        pub(super) fn next_data(
            &mut self,
            term_length: u32,
        ) -> Option<Vec<(DataFrameHeader, Vec<u8>)>> {
            let (length, from) = self.socket.recv_from(&mut self.buffer).ok()?;
            self.peer = Some(from);
            let bytes = &self.buffer[..length];
            if !matches!(parse_udp(bytes, MAX_TERM_LENGTH), Ok(ControlFrame::Data)) {
                return Some(Vec::new());
            }
            let datagram = Datagram::check(bytes, term_length).expect("the datagram checks");
            Some(
                datagram
                    .frames()
                    .map(|frame| {
                        // A padding frame is its header alone (ADR 0045): no body.
                        let body = match frame.header.frame_type() {
                            FrameType::Pad => Vec::new(),
                            FrameType::Data => frame.bytes
                                [FRAME_HEADER..frame.header.frame_length() as usize]
                                .to_vec(),
                        };
                        (frame.header, body)
                    })
                    .collect(),
            )
        }

        /// Sends bytes of the test's own making to the peer the last datagram came from.
        pub(super) fn send_bytes(&self, bytes: &[u8]) {
            self.socket
                .send_to(bytes, self.peer.expect("a setup frame arrived first"))
                .expect("the bytes are sent");
        }

        /// How long a read waits for a datagram.
        pub(super) fn set_read_timeout(&self, timeout: Duration) {
            self.socket
                .set_read_timeout(Some(timeout))
                .expect("a read timeout");
        }
    }

    /// Adds a publication on the receiver's channel, takes its setup frame, answers
    /// with a status message of `window` bytes, and cycles both threads until the
    /// conductor has the sender's connected reply. Returns the setup frame.
    fn connect_udp(
        h: &mut Harness,
        receiver: &mut TestReceiver,
        correlation_id: i64,
        stream_id: i32,
        window: i32,
    ) -> SetupFrame {
        let now = h.now;
        h.conductor.add_publication(
            1,
            correlation_id,
            stream_id,
            &receiver.channel(),
            false,
            now,
        );
        assert_eq!(h.last_error(correlation_id), None);
        h.sender.duty_cycle(now);
        let setup = match receiver.next_control() {
            Some(ControlFrame::Setup(setup)) => setup,
            other => panic!("expected a setup frame, got {other:?}"),
        };
        receiver.send_status_message(&StatusMessage {
            flags: 0,
            session_id: setup.session_id,
            stream_id,
            consumption_term_id: setup.active_term_id,
            consumption_term_offset: 0,
            receiver_window_length: window,
            receiver_id: 9,
            group_tag: None,
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let now = now_nanos();
            h.sender.duty_cycle(now);
            h.conductor.duty_cycle(now);
            let connected = h
                .conductor
                .publications
                .iter()
                .find(|publication| publication.registration_id == correlation_id)
                .and_then(|publication| publication.udp.as_ref())
                .is_some_and(|udp| udp.connected);
            if connected {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the status message did not connect"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        setup
    }

    /// Two conductors made one after the other, as two drivers a supervisor starts
    /// together are, do not start at the same session id, and each goes on by one.
    #[test]
    fn two_conductors_start_at_different_session_ids() {
        let mut first = Harness::new("session-id-first");
        let mut second = Harness::new("session-id-second");
        let (a, b) = (
            first.conductor.next_session_id,
            second.conductor.next_session_id,
        );
        assert_ne!(a, b);
        assert_eq!(first.conductor.advance_session_id(7), a);
        assert_eq!(first.conductor.advance_session_id(7), a.wrapping_add(1));
        assert_eq!(second.conductor.advance_session_id(7), b);
    }

    /// `add publication` on a `cavalry:udp` channel opens the endpoint with its five
    /// counters, names `channel.status` in the event, hands the sender the socket and
    /// the publication, and the publication is connected by the first status message
    /// and not before: the limit stays at the sender position until the sender says so.
    #[test]
    fn a_udp_publication_opens_its_endpoint_and_is_connected_by_a_status_message() {
        let mut h = Harness::new("udp-publication");
        let mut receiver = TestReceiver::bind();
        let now = h.now;
        h.conductor
            .add_publication(1, 2, 7, &receiver.channel(), false, now);
        // The one event: publication ready, naming the endpoint's status counter.
        let mut ready = None;
        let mut error = None;
        while h
            .receiver
            .receive(|type_id, message| match Event::parse(type_id, message) {
                Ok(Event::PublicationReady {
                    correlation_id: 2,
                    channel_status_indicator_id,
                    ..
                }) => ready = Some(channel_status_indicator_id),
                Ok(Event::Error {
                    error_code,
                    error_message,
                    ..
                }) => {
                    error = Some((
                        error_code,
                        String::from_utf8_lossy(error_message).into_owned(),
                    ))
                }
                _ => {}
            })
            .expect("the receiver reads")
        {}
        assert_eq!(error, None);
        assert_eq!(h.conductor.send_endpoints.len(), 1);
        assert_eq!(h.conductor.send_endpoints[0].state, ChannelState::Active);
        assert_eq!(h.conductor.send_endpoints[0].status.get(), 1);
        assert_eq!(h.conductor.send_endpoints[0].publications.get(), 1);
        assert!(h.conductor.send_endpoints[0].socket_rcvbuf.get() > 0);
        assert!(h.conductor.send_endpoints[0].socket_sndbuf.get() > 0);
        assert_eq!(h.counter(CHANNEL_OPENED), 1);
        let status_id = counter_id(&h.conductor.send_endpoints[0].status);
        assert_eq!(
            ready,
            Some(status_id),
            "the event names the endpoint's status"
        );
        {
            let publication = &h.conductor.publications[0];
            assert!(publication.udp.is_some());
            assert_eq!(
                publication.limit.get(),
                0,
                "not connected before a status message"
            );
            assert!(!publication.log.is_connected());
            assert_eq!(
                publication.log.header().term_length(),
                spec::UDP_TERM_LENGTH
            );
            assert_eq!(publication.log.header().mtu_length(), spec::UDP_MTU_LENGTH);
        }

        // The sender's first cycle takes the endpoint and the publication and sends the
        // setup frame; no status message, no heartbeat.
        h.sender.duty_cycle(now);
        let setup = match receiver.next_control() {
            Some(ControlFrame::Setup(setup)) => setup,
            other => panic!("expected a setup frame, got {other:?}"),
        };
        assert_eq!(
            (setup.stream_id, setup.term_offset, setup.mtu_length),
            (7, 0, spec::UDP_MTU_LENGTH as i32)
        );
        assert_eq!(setup.term_length, spec::UDP_TERM_LENGTH as i32);
        assert_eq!(h.counter(SENDER_SETUPS_SENT), 1);
        h.sender
            .duty_cycle(now + spec::nanos(Duration::from_millis(50)));
        assert_eq!(h.counter(SENDER_SETUPS_SENT), 1, "one per setup interval");

        // A status message without the send-setup flag connects it: the sender limit
        // is the window, the conductor raises the publisher's limit by the publication
        // window, and the log says connected.
        receiver.send_status_message(&StatusMessage {
            flags: 0,
            session_id: setup.session_id,
            stream_id: 7,
            consumption_term_id: setup.active_term_id,
            consumption_term_offset: 0,
            receiver_window_length: 131072,
            receiver_id: 9,
            group_tag: None,
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !h.conductor.publications[0]
            .udp
            .as_ref()
            .is_some_and(|udp| udp.connected)
        {
            let now = now_nanos();
            h.sender.duty_cycle(now);
            h.conductor.duty_cycle(now);
            assert!(
                Instant::now() < deadline,
                "the status message did not connect"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(h.counter(PUBLICATION_CONNECTED), 1);
        assert_eq!(h.counter(SENDER_STATUS_MESSAGES_RECEIVED), 1);
        assert_eq!(h.sender.positions_of(2), Some((0, 131072)));
        let publication = &h.conductor.publications[0];
        assert!(publication.log.is_connected());
        assert_eq!(
            publication.limit.get(),
            i64::from(publication.log.header().publication_window_length()),
            "the publisher's limit: the sender position plus the publication window"
        );
    }

    /// A publication whose channel names the position keys is created at their
    /// position with their initial term id, its position counters there, its first
    /// setup frame carries that term id and term offset, and a join names the same keys
    /// or is refused with the reason `position` (ADR 0059).
    #[test]
    fn a_udp_publication_created_at_a_position_sends_its_setup_there() {
        let mut h = Harness::new("udp-publication-at-a-position");
        let mut receiver = TestReceiver::bind();
        let now = h.now;
        let channel = String::from_utf8_lossy(&receiver.channel()).into_owned();
        let keyed = format!("{channel}|init-term-id=7|term-id=9|term-offset=4096");
        h.conductor
            .add_publication(1, 2, 7, keyed.as_bytes(), false, now);
        assert_eq!(h.last_error(2), None);
        {
            let publication = &h.conductor.publications[0];
            let term_length = i64::from(publication.log.header().term_length());
            let expected = 2 * term_length + 4096;
            assert_eq!(publication.log.header().initial_term_id(), 7);
            assert_eq!(
                publication.log.producer_position().expect("a position"),
                expected
            );
            assert_eq!(publication.position.get(), expected);
            assert_eq!(publication.limit.get(), expected, "not connected: no room");
            assert_eq!(
                publication
                    .udp
                    .as_ref()
                    .and_then(|udp| udp.position)
                    .map(|keys| (keys.initial_term_id, keys.term_id, keys.term_offset)),
                Some((7, 9, 4096))
            );
            assert_eq!(
                publication
                    .udp
                    .as_ref()
                    .map(|udp| udp.sender_position.get()),
                Some(expected)
            );
        }
        h.sender.duty_cycle(now);
        let setup = expect_setup(&mut receiver);
        assert_eq!(
            (
                setup.initial_term_id,
                setup.active_term_id,
                setup.term_offset
            ),
            (7, 9, 4096)
        );
        // A join under no keys or other keys is refused; the same keys join.
        h.conductor
            .add_publication(1, 3, 7, channel.as_bytes(), false, now);
        let (code, message) = h.last_error(3).expect("an error");
        assert_eq!(code, 1, "{message}");
        assert!(message.contains("(position)"), "{message}");
        let other = format!("{channel}|init-term-id=7|term-id=9|term-offset=8192");
        h.conductor
            .add_publication(1, 4, 7, other.as_bytes(), false, now);
        assert_eq!(h.last_error(4).expect("an error").0, 1);
        h.conductor
            .add_publication(1, 5, 7, keyed.as_bytes(), false, now);
        assert_eq!(h.last_error(5), None);
        assert_eq!(h.conductor.publications.len(), 1);
        assert_eq!(h.conductor.publications[0].registrations.len(), 2);
    }

    /// The sender's disconnected reply stores the flag 0 where the conductor takes it,
    /// in the commands pass, before the limit pass lowers the limit (ADR 0048): a
    /// publisher at the old limit is told `not connected`, never `back pressured`.
    #[test]
    fn a_udp_disconnect_stores_the_flag_before_the_limit_lowers() {
        let mut h = Harness::new("udp-disconnect-flag-before-limit");
        let mut receiver = TestReceiver::bind();
        connect_udp(&mut h, &mut receiver, 2, 7, 131072);
        let window = i64::from(
            h.conductor.publications[0]
                .log
                .header()
                .publication_window_length(),
        );
        assert!(h.conductor.publications[0].log.is_connected());
        assert_eq!(h.conductor.publications[0].limit.get(), window);

        h.conductor.set_udp_connected(2, false);
        let publication = &h.conductor.publications[0];
        assert!(
            !publication.log.is_connected(),
            "the 0 is stored where the reply is taken"
        );
        assert_eq!(
            publication.limit.get(),
            window,
            "the limit waits for the limit pass"
        );
        h.conductor.keep_publications(now_nanos());
        let publication = &h.conductor.publications[0];
        assert!(!publication.log.is_connected());
        assert_eq!(publication.limit.get(), 0);
    }

    /// Two channels are the same channel when their resolved endpoint and interface
    /// are the same: a second stream shares the endpoint, the same stream joins the
    /// first publication, and an exclusive one never joins.
    #[test]
    fn publications_on_one_resolved_channel_share_its_endpoint_and_a_shared_stream_joins() {
        let mut h = Harness::new("udp-shared-endpoint");
        let receiver = TestReceiver::bind();
        let channel = receiver.channel();
        let now = h.now;
        h.conductor.add_publication(1, 2, 7, &channel, false, now);
        h.conductor.add_publication(1, 3, 8, &channel, false, now);
        h.conductor.add_publication(1, 4, 7, &channel, false, now);
        h.conductor.add_publication(1, 5, 7, &channel, true, now);
        for correlation_id in 2..=5 {
            assert_eq!(h.last_error(correlation_id), None);
        }
        assert_eq!(h.conductor.send_endpoints.len(), 1, "one endpoint");
        assert_eq!(h.counter(CHANNEL_OPENED), 1);
        assert_eq!(
            h.conductor.publications.len(),
            3,
            "two shared streams and an exclusive"
        );
        assert_eq!(
            h.conductor.publications[0].registrations.len(),
            2,
            "4 joined 2"
        );
        assert_eq!(h.conductor.send_endpoints[0].publication_count, 3);
        assert_eq!(h.conductor.send_endpoints[0].publications.get(), 3);
        assert_eq!(h.conductor.registration_count, 4);
    }

    /// The refusals: a host that does not resolve is code 9 for a publication and a
    /// subscription alike, an interface that is not this host's is code 4 with nothing
    /// registered, and a recording on a channel with nothing live on it is code 20.
    #[test]
    fn a_udp_channel_is_refused_with_the_spec_error_codes_and_nothing_registered() {
        let mut h = Harness::new("udp-refusals");
        let now = h.now;
        h.conductor.add_publication(
            1,
            2,
            7,
            b"cavalry:udp?endpoint=no-such-host.invalid:40123",
            false,
            now,
        );
        let (code, message) = h.last_error(2).expect("an error");
        assert_eq!(code, 9, "{message}");
        assert!(message.contains("no-such-host.invalid"), "{message}");
        h.conductor.add_publication(
            1,
            3,
            7,
            b"cavalry:udp?endpoint=127.0.0.1:40123|interface=203.0.113.9",
            false,
            now,
        );
        let (code, message) = h.last_error(3).expect("an error");
        assert_eq!(code, 4, "{message}");
        assert!(message.contains("203.0.113.9"), "{message}");
        assert!(h.conductor.send_endpoints.is_empty());
        assert!(h.conductor.publications.is_empty());
        assert_eq!(h.counter(CHANNEL_OPENED), 0);
        // A subscription resolves its host the same way (ADR 0042), and a recording
        // on a channel with no live publication or image on it is code 20.
        h.conductor.add_subscription(
            1,
            4,
            7,
            b"cavalry:udp?endpoint=no-such-host.invalid:40123",
            h.now,
        );
        let (code, message) = h.last_error(4).expect("an error");
        assert_eq!(code, 9, "{message}");
        assert!(h.conductor.receive_endpoints.is_empty());
        assert!(h.conductor.subscriptions.is_empty());
        h.conductor
            .start_recording(5, 7, b"cavalry:udp?endpoint=127.0.0.1:40123", now);
        let (code, message) = h.last_error(5).expect("an error");
        assert_eq!(code, 20, "{message}");
        assert!(message.contains("127.0.0.1:40123"), "{message}");
        // A replay channel's host is resolved as the command is handled (ADR 0043): code
        // 9, and no replay registered.
        h.conductor.start_replay(
            6,
            0,
            -1,
            -1,
            7,
            b"cavalry:udp?endpoint=no-such-host.invalid:40123",
        );
        let (code, message) = h.last_error(6).expect("an error");
        assert_eq!(code, 9, "{message}");
        assert!(message.contains("no-such-host.invalid"), "{message}");
        assert!(h.conductor.replays.is_empty());
    }

    /// A control channel of this test's own on a free loopback port.
    fn control_channel() -> String {
        let port = std::net::UdpSocket::bind("127.0.0.1:0")
            .expect("a loopback socket")
            .local_addr()
            .expect("bound")
            .port();
        format!("cavalry:udp?control=127.0.0.1:{port}|control-mode=manual")
    }

    /// Cycles the sender and the conductor on the real clock until `condition` holds,
    /// for up to five seconds; returns whether it held.
    fn wait_for(h: &mut Harness, mut condition: impl FnMut(&mut Harness) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let now = now_nanos();
            h.sender.duty_cycle(now);
            h.conductor.duty_cycle(now);
            if condition(h) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Cycles the sender and the conductor at one time of the sender's clock until
    /// `condition` holds, for up to five seconds of real time: what a status message
    /// sent on the loopback is timed at is the `now` the cycle that reads it carries.
    fn wait_at(h: &mut Harness, now: i64, mut condition: impl FnMut(&mut Harness) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            h.sender.duty_cycle(now);
            h.conductor.duty_cycle(now);
            if condition(h) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The next setup frame at the receiver, or a panic naming what came instead.
    fn expect_setup(receiver: &mut TestReceiver) -> SetupFrame {
        match receiver.next_control() {
            Some(ControlFrame::Setup(setup)) => setup,
            other => panic!("expected a setup frame, got {other:?}"),
        }
    }

    /// The next datagram of data frames at the receiver, heartbeats and control frames
    /// skipped; `None` at the read timeout.
    fn next_frames(receiver: &mut TestReceiver) -> Option<Vec<(DataFrameHeader, Vec<u8>)>> {
        loop {
            match receiver.next_data(spec::UDP_TERM_LENGTH) {
                Some(frames) if frames.is_empty() => continue,
                other => return other,
            }
        }
    }

    /// The label of counter record `id`, read from the counters file as `doctor` reads it.
    fn label_of(h: &Harness, id: u32) -> String {
        cavalry_shm::counters::CountersReader::open(h.dir.path())
            .expect("the counters file opens")
            .record(id)
            .expect("a record")
            .expect("allocated")
            .label()
            .to_string()
    }

    /// A status message answering `setup` from the receiver `receiver_id` with `window`.
    fn status(setup: &SetupFrame, window: i32, receiver_id: i64) -> StatusMessage {
        StatusMessage {
            flags: 0,
            session_id: setup.session_id,
            stream_id: 7,
            consumption_term_id: setup.active_term_id,
            consumption_term_offset: 0,
            receiver_window_length: window,
            receiver_id,
            group_tag: None,
        }
    }

    /// A control channel's publication (`spec/udp.md`, "The sender"; ADR 0053): the
    /// endpoint's socket binds the control address and sends to no one until a client
    /// adds a destination, then every datagram goes to each destination in turn, a
    /// setup frame, a burst, each counted once per destination; under `max` the limit
    /// is the greatest position plus window the destinations' receivers report; a
    /// duplicate add and a remove of one not held answer success and change nothing;
    /// a removed destination gets nothing more; with none left a burst is dropped, the
    /// sender position advances past it and nothing is counted as on the wire.
    #[test]
    fn a_control_channel_sends_every_datagram_to_each_destination() {
        let mut h = Harness::new("udp-mdc-fan-out");
        let mut a = TestReceiver::bind();
        let mut b = TestReceiver::bind();
        let control = control_channel();
        let now = h.now;
        h.conductor
            .add_publication(1, 2, 7, control.as_bytes(), false, now);
        assert_eq!(h.last_error(2), None);
        {
            let endpoint = &h.conductor.send_endpoints[0];
            assert!(endpoint.target.control);
            assert_eq!(endpoint.destinations.get(), 0);
            assert_eq!(endpoint.status.get(), 1);
            assert_eq!(endpoint.publications.get(), 1);
        }
        // No destination: the setup frame that is due goes nowhere and counts nothing.
        h.sender.duty_cycle(now);
        assert_eq!(h.counter(SENDER_SETUPS_SENT), 0);
        // Two destinations by the command, and a duplicate that adds nothing.
        h.conductor.change_destination(1, 3, 2, &a.channel(), true);
        h.conductor.change_destination(1, 4, 2, &b.channel(), true);
        h.conductor.change_destination(1, 5, 2, &a.channel(), true);
        for correlation_id in 3..=5 {
            assert_eq!(h.last_error(correlation_id), None);
        }
        assert_eq!(h.conductor.send_endpoints[0].destinations.get(), 2);
        assert_eq!(h.conductor.send_endpoints[0].destination_list.len(), 2);
        // The sender takes them, and the next setup frame reaches both.
        let now = now + spec::nanos(SETUP_INTERVAL);
        h.sender.duty_cycle(now);
        let setup_a = expect_setup(&mut a);
        let setup_b = expect_setup(&mut b);
        assert_eq!(setup_a.session_id, setup_b.session_id);
        assert_eq!(h.counter(SENDER_SETUPS_SENT), 2, "once per destination");
        // The receivers gauge names the strategy last, so that `doctor` reads a `max`
        // publication's 0 as no table (ADR 0053).
        let receivers_id = h.conductor.publications[0]
            .udp
            .as_ref()
            .expect("a cavalry:udp publication")
            .receivers_id;
        assert_eq!(
            label_of(&h, receivers_id),
            format!(
                "sender.flow_control_receivers 2 {} 7 2 fc=max",
                setup_a.session_id
            )
        );
        // Each answers with a receiver id of its own: under max the greater window wins.
        a.send_status_message(&status(&setup_a, 4096, 9));
        b.send_status_message(&status(&setup_b, 8192, 10));
        assert!(
            wait_for(&mut h, |h| {
                h.sender.positions_of(2) == Some((0, 8192))
                    && h.conductor.publications[0]
                        .udp
                        .as_ref()
                        .is_some_and(|udp| udp.connected)
            }),
            "both status messages arrived and the greater window set the limit"
        );
        h.conductor.duty_cycle(now_nanos());
        // One message: one burst, to each destination, counted once per destination.
        let mut appender = h.conductor.publications[0]
            .log
            .appender()
            .expect("an appender");
        let limit = h.conductor.publications[0].limit.get();
        assert!(limit > 0, "connected: the publisher's limit is up");
        appender.offer(b"cavalry!", limit, 0).expect("offered");
        h.sender.duty_cycle(now_nanos());
        for (name, receiver) in [("a", &mut a), ("b", &mut b)] {
            let frames = next_frames(receiver).unwrap_or_else(|| panic!("a datagram at {name}"));
            assert_eq!(frames.len(), 1, "{name}");
            assert_eq!(frames[0].1, b"cavalry!", "{name}");
        }
        assert_eq!(h.counter(SENDER_FRAMES_SENT), 2, "once per destination");
        assert_eq!(
            h.counter(SENDER_BYTES_SENT),
            128,
            "a 64-byte datagram to each"
        );
        assert_eq!(
            h.sender.positions_of(2).map(|(position, _)| position),
            Some(64)
        );
        // b removed: the next message reaches a alone; removing it again succeeds too.
        h.conductor.change_destination(1, 6, 2, &b.channel(), false);
        h.conductor.change_destination(1, 7, 2, &b.channel(), false);
        assert_eq!(h.last_error(6), None);
        assert_eq!(h.last_error(7), None);
        assert_eq!(h.conductor.send_endpoints[0].destinations.get(), 1);
        let limit = h.conductor.publications[0].limit.get();
        appender.offer(b"again!!!", limit, 0).expect("offered");
        h.sender.duty_cycle(now_nanos());
        assert_eq!(next_frames(&mut a).map(|frames| frames.len()), Some(1));
        b.set_read_timeout(Duration::from_millis(200));
        assert!(
            b.next_data(spec::UDP_TERM_LENGTH).is_none(),
            "nothing reaches b after its remove"
        );
        assert_eq!(h.counter(SENDER_FRAMES_SENT), 3);
        // None left: the burst is dropped, the position advances past it, nothing on
        // the wire is counted.
        h.conductor.change_destination(1, 8, 2, &a.channel(), false);
        assert_eq!(h.last_error(8), None);
        assert_eq!(h.conductor.send_endpoints[0].destinations.get(), 0);
        let limit = h.conductor.publications[0].limit.get();
        appender.offer(b"nowhere!", limit, 0).expect("offered");
        h.sender.duty_cycle(now_nanos());
        assert_eq!(
            h.sender.positions_of(2).map(|(position, _)| position),
            Some(192)
        );
        assert_eq!(
            h.counter(SENDER_FRAMES_SENT),
            3,
            "nothing counted as on the wire"
        );
        assert_eq!(h.counter(SENDER_BYTES_SENT), 192);
        a.set_read_timeout(Duration::from_millis(200));
        assert!(a.next_data(spec::UDP_TERM_LENGTH).is_none());
        drop(appender);
    }

    /// `fc=min` (`spec/udp.md`, "The sender"; ADR 0053): the sender limit follows the
    /// slowest receiver in the publication's table, by receiver id, and never falls; a
    /// receiver that sends nothing for the flow control receiver timeout is dropped and
    /// counted, and the limit follows the rest; one whose status message carries end
    /// of stream is dropped on the next cycle; a 65th receiver is accepted for the
    /// connection and not tracked, counted; the gauge says how many are in the table.
    #[test]
    fn min_flow_control_follows_the_slowest_receiver_and_drops_one_that_times_out() {
        let mut h = Harness::new("udp-mdc-min");
        let mut a = TestReceiver::bind();
        let mut b = TestReceiver::bind();
        let control = format!("{}|fc=min", control_channel());
        let now = h.now;
        h.conductor
            .add_publication(1, 2, 7, control.as_bytes(), false, now);
        assert_eq!(h.last_error(2), None);
        let udp = h.conductor.publications[0]
            .udp
            .as_ref()
            .expect("a cavalry:udp publication");
        assert_eq!(udp.flow_control, FlowControl::Min);
        let receivers_id = udp.receivers_id;
        assert!(
            label_of(&h, receivers_id).ends_with(" 2 fc=min"),
            "{}",
            label_of(&h, receivers_id)
        );
        let gauge = |h: &Harness| {
            h.conductor
                .counters
                .counter(receivers_id)
                .expect("the gauge")
                .get()
        };
        h.conductor.change_destination(1, 3, 2, &a.channel(), true);
        h.conductor.change_destination(1, 4, 2, &b.channel(), true);
        // The first cycle takes the destinations and sends the setup frame that is due.
        let at = now_nanos();
        h.sender.duty_cycle(at);
        let setup_a = expect_setup(&mut a);
        let setup_b = expect_setup(&mut b);
        a.send_status_message(&status(&setup_a, 4096, 9));
        b.send_status_message(&status(&setup_b, 8192, 10));
        assert!(wait_at(&mut h, at, |h| gauge(h) == 2));
        assert_eq!(
            h.sender.positions_of(2),
            Some((0, 4096)),
            "the slowest sets the pace"
        );
        // b runs ahead: the limit stays at a's.
        b.send_status_message(&status(&setup_b, 16384, 10));
        assert!(wait_at(&mut h, at, |h| {
            h.counter(SENDER_STATUS_MESSAGES_RECEIVED) == 3
        }));
        assert_eq!(h.sender.positions_of(2), Some((0, 4096)));
        // Three seconds on, b answers again and a does not; at the timeout a is
        // dropped and counted, and the limit follows b.
        let later = at + spec::nanos(Duration::from_secs(3));
        b.send_status_message(&status(&setup_b, 16384, 10));
        assert!(wait_at(&mut h, later, |h| {
            h.counter(SENDER_STATUS_MESSAGES_RECEIVED) == 4
        }));
        assert_eq!(h.counter(SENDER_FLOW_CONTROL_RECEIVERS_TIMED_OUT), 0);
        let timed_out =
            at + spec::nanos(FLOW_CONTROL_RECEIVER_TIMEOUT) + spec::nanos(Duration::from_millis(1));
        h.sender.duty_cycle(timed_out);
        assert_eq!(h.counter(SENDER_FLOW_CONTROL_RECEIVERS_TIMED_OUT), 1);
        assert_eq!(gauge(&h), 1);
        assert_eq!(h.sender.positions_of(2), Some((0, 16384)));
        assert!(
            h.conductor.publications[0]
                .udp
                .as_ref()
                .is_some_and(|udp| udp.connected),
            "b's status message kept the publication connected"
        );
        // The table fills with 63 more receiver ids from a's socket, each far ahead,
        // and a 65th is counted and not tracked: the limit stays b's.
        for receiver_id in 100..163 {
            a.send_status_message(&status(&setup_a, 1_000_000, receiver_id));
        }
        assert!(wait_at(&mut h, timed_out, |h| gauge(h) == 64));
        a.send_status_message(&status(&setup_a, 2_000_000, 999));
        assert!(wait_at(&mut h, timed_out, |h| {
            h.counter(SENDER_FLOW_CONTROL_RECEIVERS_FULL) == 1
        }));
        assert_eq!(gauge(&h), 64);
        assert_eq!(h.sender.positions_of(2), Some((0, 16384)));
        // The end-of-stream flag from a receiver the full table does not hold adds no
        // entry and is not counted as full: its image has ended (ADR 0054).
        let received = h.counter(SENDER_STATUS_MESSAGES_RECEIVED);
        a.send_status_message(&StatusMessage {
            flags: STATUS_MESSAGE_END_OF_STREAM,
            ..status(&setup_a, 3_000_000, 501)
        });
        assert!(wait_at(&mut h, timed_out, |h| {
            h.counter(SENDER_STATUS_MESSAGES_RECEIVED) == received + 1
        }));
        assert_eq!(h.counter(SENDER_FLOW_CONTROL_RECEIVERS_FULL), 1);
        assert_eq!(gauge(&h), 64);
        // b ends its stream: dropped on the next cycle and not counted as a timeout, and
        // the limit follows the rest (ADR 0054).
        b.send_status_message(&StatusMessage {
            flags: STATUS_MESSAGE_END_OF_STREAM,
            ..status(&setup_b, 16384, 10)
        });
        assert!(wait_at(&mut h, timed_out, |h| gauge(h) == 63));
        assert_eq!(h.counter(SENDER_FLOW_CONTROL_RECEIVERS_TIMED_OUT), 1);
        assert_eq!(h.sender.positions_of(2), Some((0, 1_000_000)));
        // b's flag again, as a receiver sends it five times: no entry, nothing counted.
        let received = h.counter(SENDER_STATUS_MESSAGES_RECEIVED);
        b.send_status_message(&StatusMessage {
            flags: STATUS_MESSAGE_END_OF_STREAM,
            ..status(&setup_b, 16384, 10)
        });
        assert!(wait_at(&mut h, timed_out, |h| {
            h.counter(SENDER_STATUS_MESSAGES_RECEIVED) == received + 1
        }));
        h.sender.duty_cycle(timed_out);
        assert_eq!(gauge(&h), 63);
        assert_eq!(h.counter(SENDER_FLOW_CONTROL_RECEIVERS_FULL), 1);
        assert_eq!(h.counter(SENDER_FLOW_CONTROL_RECEIVERS_TIMED_OUT), 1);
        assert_eq!(h.sender.positions_of(2), Some((0, 1_000_000)));
    }

    /// A clean close of a `min` publication on a control channel with two destinations
    /// (`spec/udp.md`, "The sender"; ADR 0054, ADR 0056): each receiver's five status
    /// messages with the end-of-stream flag keep its entry in the table through the
    /// linger, once and never a second time, so that what `doctor` reads until the
    /// linger ends, the gauge beside the endpoint's `channel.destinations` and a
    /// sender limit above 0, says no destination stopped answering; at the timeout
    /// the publication closes with no receiver counted in
    /// `sender.flow_control_receivers_timed_out` and no disconnection.
    #[test]
    fn a_clean_close_of_a_min_publication_counts_no_receiver_timed_out() {
        let mut h = Harness::new("udp-mdc-min-close");
        let mut a = TestReceiver::bind();
        let mut b = TestReceiver::bind();
        let control = format!("{}|fc=min", control_channel());
        let now = h.now;
        h.conductor
            .add_publication(1, 2, 7, control.as_bytes(), false, now);
        assert_eq!(h.last_error(2), None);
        let receivers_id = h.conductor.publications[0]
            .udp
            .as_ref()
            .expect("a cavalry:udp publication")
            .receivers_id;
        assert!(
            label_of(&h, receivers_id).ends_with(" fc=min"),
            "{}",
            label_of(&h, receivers_id)
        );
        let gauge = |h: &Harness| {
            h.conductor
                .counters
                .counter(receivers_id)
                .expect("the gauge")
                .get()
        };
        h.conductor.change_destination(1, 3, 2, &a.channel(), true);
        h.conductor.change_destination(1, 4, 2, &b.channel(), true);
        let at = now_nanos();
        h.sender.duty_cycle(at);
        let setup_a = expect_setup(&mut a);
        let setup_b = expect_setup(&mut b);
        a.send_status_message(&status(&setup_a, 131072, 9));
        b.send_status_message(&status(&setup_b, 131072, 10));
        assert!(wait_at(&mut h, at, |h| gauge(h) == 2));
        // The close, with nothing to send: the sender position is the end of stream.
        h.conductor.remove_publication(1, 20, 2, at);
        h.sender.duty_cycle(at);
        // Each receiver's image ends a moment after the close, and each sends its five
        // status messages with the flag.
        let ended = at + spec::nanos(Duration::from_millis(1));
        let received = h.counter(SENDER_STATUS_MESSAGES_RECEIVED);
        for (receiver, setup, receiver_id) in [(&mut a, &setup_a, 9), (&mut b, &setup_b, 10)] {
            for _ in 0..5 {
                receiver.send_status_message(&StatusMessage {
                    flags: STATUS_MESSAGE_END_OF_STREAM,
                    ..status(setup, 131072, receiver_id)
                });
            }
        }
        assert!(wait_at(&mut h, ended, |h| {
            h.counter(SENDER_STATUS_MESSAGES_RECEIVED) == received + 10
        }));
        h.sender.duty_cycle(ended);
        assert_eq!(gauge(&h), 2, "kept once each, added a second time never");
        // Until the linger ends the counters `doctor` reads show both destinations
        // answered: the gauge at the endpoint's destinations, the limit above 0,
        // nothing timed out.
        let linger = spec::nanos(cavalry_proto::udp::LINGER);
        let last = at + linger - 1;
        h.sender.duty_cycle(last);
        assert!(
            h.sender.positions_of(2).is_some_and(|(_, limit)| limit > 0),
            "still lingering, its limit above 0"
        );
        assert_eq!(gauge(&h), 2);
        assert_eq!(h.conductor.send_endpoints[0].destinations.get(), 2);
        assert_eq!(h.counter(SENDER_FLOW_CONTROL_RECEIVERS_TIMED_OUT), 0);
        // The linger ends at its timeout, a control channel's, and nothing timed out.
        h.sender.duty_cycle(at + linger);
        assert_eq!(h.sender.positions_of(2), None, "closed at the timeout");
        assert_eq!(h.counter(SENDER_FLOW_CONTROL_RECEIVERS_TIMED_OUT), 0);
        assert_eq!(h.counter(SENDER_FLOW_CONTROL_RECEIVERS_FULL), 0);
        assert_eq!(h.counter(PUBLICATION_DISCONNECTED), 0);
    }

    /// The destination commands' refusals, and the channel rules of version 1
    /// (`spec/client-protocol.md`, "Commands" and "What the driver rejects"; ADR 0053):
    /// add destination on a publication with no control address is code 1 with the
    /// reason `control`; an unknown or another client's registration is code 3; a
    /// destination that is `cavalry:ipc` or a control channel, one naming `interface`
    /// or `fc`, one with no address of the control address's family, or one beyond the
    /// loopback a loopback control address is on, is code 1 with the reason
    /// `destination`, one that fails the
    /// grammar code 1 with the reader's reason; a name with no address is code 9; a 65th
    /// destination is code 10, counted; a subscription's channel that names a
    /// publication's keys is code 1 with the reason `subscription`; a join under
    /// another `fc` is code 1 with the reason `fc`, and under the same joins. Version 2's
    /// position keys (ADR 0059) are code 1 on a subscription, a destination and a
    /// replay channel, the last with the reason `position`.
    #[test]
    fn destination_commands_and_a_control_channel_are_refused_with_the_spec_codes() {
        let mut h = Harness::new("udp-mdc-refusals");
        let receiver = TestReceiver::bind();
        let unicast = String::from_utf8(receiver.channel()).expect("utf-8");
        let now = h.now;
        h.conductor
            .add_publication(1, 2, 7, unicast.as_bytes(), false, now);
        assert_eq!(h.last_error(2), None);
        h.conductor
            .change_destination(1, 3, 2, unicast.as_bytes(), true);
        let (code, message) = h.last_error(3).expect("an error");
        assert_eq!(code, 1, "{message}");
        assert!(message.contains("(control)"), "{message}");
        h.conductor
            .change_destination(1, 4, 99, unicast.as_bytes(), true);
        assert_eq!(h.last_error(4).expect("an error").0, 3);
        h.conductor
            .change_destination(2, 5, 2, unicast.as_bytes(), true);
        assert_eq!(h.last_error(5).expect("an error").0, 3);
        let control = control_channel();
        h.conductor
            .add_publication(1, 6, 8, control.as_bytes(), false, now);
        assert_eq!(h.last_error(6), None);
        h.conductor
            .change_destination(1, 7, 6, b"cavalry:ipc", true);
        let (code, message) = h.last_error(7).expect("an error");
        assert_eq!(code, 1, "{message}");
        assert!(message.contains("(destination)"), "{message}");
        h.conductor
            .change_destination(1, 8, 6, control.as_bytes(), true);
        assert_eq!(h.last_error(8).expect("an error").0, 1);
        h.conductor.change_destination(
            1,
            17,
            6,
            b"cavalry:udp?endpoint=127.0.0.1:40123|fc=min",
            true,
        );
        let (code, message) = h.last_error(17).expect("an error");
        assert_eq!(code, 1, "{message}");
        assert!(message.contains("names nothing else"), "{message}");
        // An address of the other family, and a host beyond the loopback the control
        // address is on: code 1, reason destination; a name with no address: code 9.
        h.conductor
            .change_destination(1, 9, 6, b"cavalry:udp?endpoint=[::1]:40123", true);
        let (code, message) = h.last_error(9).expect("an error");
        assert_eq!(code, 1, "{message}");
        assert!(message.contains("(destination)"), "{message}");
        assert!(message.contains("IPv4"), "{message}");
        h.conductor
            .change_destination(1, 15, 6, b"cavalry:udp?endpoint=10.0.0.9:40123", true);
        let (code, message) = h.last_error(15).expect("an error");
        assert_eq!(code, 1, "{message}");
        assert!(message.contains("loopback"), "{message}");
        h.conductor.change_destination(
            1,
            16,
            6,
            b"cavalry:udp?endpoint=no-such-host.invalid:40123",
            true,
        );
        let (code, message) = h.last_error(16).expect("an error");
        assert_eq!(code, 9, "{message}");
        h.conductor
            .change_destination(1, 10, 6, b"cavalry:udp?endpoint=", true);
        let (code, message) = h.last_error(10).expect("an error");
        assert_eq!(code, 1, "{message}");
        assert!(message.contains("(grammar)"), "{message}");
        assert_eq!(h.conductor.send_endpoints[1].destinations.get(), 0);
        let registry_full = h.counter(DRIVER_REGISTRY_FULL);
        for i in 0..MAX_DESTINATIONS {
            let destination = format!("cavalry:udp?endpoint=127.0.0.1:{}", 40_000 + i);
            h.conductor
                .change_destination(1, 100 + i as i64, 6, destination.as_bytes(), true);
            assert_eq!(h.last_error(100 + i as i64), None, "destination {i}");
        }
        assert_eq!(h.conductor.send_endpoints[1].destinations.get(), 64);
        h.conductor
            .change_destination(1, 200, 6, b"cavalry:udp?endpoint=127.0.0.1:41000", true);
        let (code, message) = h.last_error(200).expect("an error");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("destinations"), "{message}");
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), registry_full + 1);
        h.conductor
            .change_destination(1, 201, 6, b"cavalry:udp?endpoint=127.0.0.1:41000", false);
        assert_eq!(h.last_error(201), None, "a remove of one not held");
        assert_eq!(h.conductor.send_endpoints[1].destinations.get(), 64);
        // A subscription's channel that names a publication's keys.
        h.conductor.add_subscription(
            1,
            11,
            7,
            b"cavalry:udp?endpoint=127.0.0.1:40123|fc=max",
            h.now,
        );
        let (code, message) = h.last_error(11).expect("an error");
        assert_eq!(code, 1, "{message}");
        assert!(message.contains("(subscription)"), "{message}");
        h.conductor
            .add_subscription(1, 12, 7, control.as_bytes(), h.now);
        assert_eq!(h.last_error(12).expect("an error").0, 1);
        assert!(h.conductor.receive_endpoints.is_empty());
        // Version 2's position keys: a publication's, refused on a subscription and on a
        // destination (ADR 0059).
        h.conductor.add_subscription(
            1,
            15,
            7,
            b"cavalry:udp?endpoint=127.0.0.1:40123|init-term-id=7|term-id=9|term-offset=4096",
            h.now,
        );
        let (code, message) = h.last_error(15).expect("an error");
        assert_eq!(code, 1, "{message}");
        assert!(message.contains("(subscription)"), "{message}");
        assert!(h.conductor.receive_endpoints.is_empty());
        h.conductor.change_destination(
            1,
            202,
            6,
            b"cavalry:udp?endpoint=127.0.0.1:41000|init-term-id=7|term-id=9|term-offset=4096",
            true,
        );
        let (code, message) = h.last_error(202).expect("an error");
        assert_eq!(code, 1, "{message}");
        assert!(message.contains("(destination)"), "{message}");
        // A replay's publication starts at the replay's position, so the keys on its
        // channel are refused before the replay is registered.
        h.conductor.start_replay(
            203,
            0,
            cavalry_proto::client_protocol::NULL_POSITION,
            NULL_LENGTH,
            7,
            b"cavalry:udp?endpoint=127.0.0.1:40123|init-term-id=7|term-id=9|term-offset=4096",
        );
        let (code, message) = h.last_error(203).expect("an error");
        assert_eq!(code, 1, "{message}");
        assert!(message.contains("(position)"), "{message}");
        assert!(h.conductor.replays.is_empty());
        // A publication that names them is created at their position, and a start
        // recording on a subscription with no image arms the recording: each its own
        // test (ADR 0059).
        // A join under another fc, and under the same.
        let min = format!("{unicast}|fc=min");
        h.conductor
            .add_publication(1, 13, 7, min.as_bytes(), false, now);
        let (code, message) = h.last_error(13).expect("an error");
        assert_eq!(code, 1, "{message}");
        assert!(message.contains("(fc)"), "{message}");
        let max = format!("{unicast}|fc=max");
        h.conductor
            .add_publication(1, 14, 7, max.as_bytes(), false, now);
        assert_eq!(h.last_error(14), None);
        assert_eq!(
            h.conductor.publications[0].registrations.len(),
            2,
            "14 joined 2"
        );
        assert_eq!(h.conductor.publications.len(), 2);
        // Ten of version 5's refusals, and the position keys refused on a subscription,
        // on a destination and on a replay channel; the code 11 answers count nothing,
        // and a recording channel's keys take no part (ADR 0059).
        assert_eq!(h.counter(CLIENT_PROTOCOL_REJECTED_COMMAND), 13);
    }

    /// An RTT measurement that reaches a control channel's socket is answered to the
    /// address it came from (`spec/udp.md`, "The sender", step 5; ADR 0053).
    #[test]
    fn an_rtt_measurement_on_a_control_channel_is_answered_to_its_source() {
        let mut h = Harness::new("udp-mdc-rtt");
        let mut a = TestReceiver::bind();
        let control = control_channel();
        let now = h.now;
        h.conductor
            .add_publication(1, 2, 7, control.as_bytes(), false, now);
        assert_eq!(h.last_error(2), None);
        h.conductor.change_destination(1, 3, 2, &a.channel(), true);
        h.sender.duty_cycle(now);
        let setup = expect_setup(&mut a);
        let probe = RttMeasurement {
            flags: RTT_MEASUREMENT_REPLY,
            session_id: setup.session_id,
            stream_id: 7,
            echo_timestamp: 77,
            reception_delta: 0,
            receiver_id: 9,
        };
        a.send_bytes(&probe.to_bytes());
        assert!(wait_for(&mut h, |h| h.counter(SENDER_RTT_REPLIES_SENT) == 1));
        // Setup frames keep coming while nothing answers: the answer is among them.
        let answer = loop {
            match a.next_control() {
                Some(ControlFrame::RttMeasurement(answer)) => break answer,
                Some(ControlFrame::Setup(_)) => continue,
                other => panic!("expected the RTT answer, got {other:?}"),
            }
        };
        assert_eq!(answer.flags, 0);
        assert_eq!(answer.echo_timestamp, 77);
        assert_eq!(answer.receiver_id, 9);
        assert_eq!(answer.reception_delta, 0);
    }

    /// A control channel's closed publication lingers for the linger timeout
    /// (`spec/udp.md`, "The sender"; ADR 0054): a status message with the end-of-stream
    /// flag from one destination after the close does not end it, the other
    /// destination's NAK for the tail is still answered, and the sender closes the
    /// publication once the linger timeout has passed since the close and not before.
    #[test]
    fn a_control_channel_lingers_past_one_destinations_end_of_stream() {
        let mut h = Harness::new("udp-mdc-linger");
        let mut a = TestReceiver::bind();
        let mut b = TestReceiver::bind();
        let control = control_channel();
        let now = h.now;
        h.conductor
            .add_publication(1, 2, 7, control.as_bytes(), false, now);
        assert_eq!(h.last_error(2), None);
        h.conductor.change_destination(1, 3, 2, &a.channel(), true);
        h.conductor.change_destination(1, 4, 2, &b.channel(), true);
        h.sender.duty_cycle(now_nanos());
        let setup_a = expect_setup(&mut a);
        let setup_b = expect_setup(&mut b);
        a.send_status_message(&status(&setup_a, 131072, 9));
        b.send_status_message(&status(&setup_b, 131072, 10));
        assert!(
            wait_for(&mut h, |h| {
                h.conductor.publications[0]
                    .udp
                    .as_ref()
                    .is_some_and(|udp| udp.connected)
            }),
            "the status messages connected the publication"
        );
        h.conductor.duty_cycle(now_nanos());
        // One message, the stream's tail, to both.
        let mut appender = h.conductor.publications[0]
            .log
            .appender()
            .expect("an appender");
        let limit = h.conductor.publications[0].limit.get();
        appender.offer(b"the tail", limit, 0).expect("offered");
        drop(appender);
        h.sender.duty_cycle(now_nanos());
        for (name, receiver) in [("a", &mut a), ("b", &mut b)] {
            let frames = next_frames(receiver).unwrap_or_else(|| panic!("a datagram at {name}"));
            assert_eq!(frames[0].1, b"the tail", "{name}");
        }
        // The close, taken by the sender at `closed_at`; a's receiver has the stream to
        // its end and says so.
        let closed_at = now_nanos();
        h.conductor.remove_publication(1, 20, 2, closed_at);
        h.sender.duty_cycle(closed_at);
        let received = h.counter(SENDER_STATUS_MESSAGES_RECEIVED);
        a.send_status_message(&StatusMessage {
            flags: STATUS_MESSAGE_END_OF_STREAM,
            consumption_term_offset: 64,
            ..status(&setup_a, 131072, 9)
        });
        assert!(wait_at(&mut h, closed_at, |h| {
            h.counter(SENDER_STATUS_MESSAGES_RECEIVED) > received
        }));
        assert!(
            h.sender.positions_of(2).is_some(),
            "one destination's end of stream does not end the linger"
        );
        assert_eq!(h.conductor.publications.len(), 1);
        // b lost the tail and NAKs it: answered, to both destinations, as every
        // retransmit is.
        b.send_bytes(
            &cavalry_proto::udp::Nak {
                flags: 0,
                session_id: setup_b.session_id,
                stream_id: 7,
                term_id: setup_b.active_term_id,
                term_offset: 0,
                length: 64,
            }
            .to_bytes(),
        );
        let nak_at = closed_at + spec::nanos(Duration::from_secs(1));
        assert!(wait_at(&mut h, nak_at, |h| {
            h.counter(SENDER_RETRANSMITS_SENT) == 1
        }));
        let again = next_frames(&mut b).expect("the retransmit at b");
        assert_eq!(again[0].1, b"the tail");
        // Still held just before the linger timeout, and closed at it. The connection
        // timeout is the linger's, 5 s, and a's status message came at the close, so the
        // first cycle at the timeout disconnects the publication and the next ends its
        // linger.
        let linger = spec::nanos(cavalry_proto::udp::LINGER);
        h.sender
            .duty_cycle(closed_at + linger - spec::nanos(Duration::from_millis(1)));
        assert!(h.sender.positions_of(2).is_some(), "held until the timeout");
        h.sender.duty_cycle(closed_at + linger);
        h.sender.duty_cycle(closed_at + linger);
        assert_eq!(h.sender.positions_of(2), None, "closed at the timeout");
    }

    /// An `add publication` on a control channel whose send endpoint is closing (ADR
    /// 0055): one during the last publication's linger opens its publication on the
    /// endpoint that is still active; one after the linger has ended and before the
    /// sender has dropped the socket is code 4, its message saying the endpoint is
    /// closing and to send the command again, with nothing registered; and the same add
    /// after the sender's close binds the control address again.
    #[test]
    fn an_add_publication_on_a_closing_send_endpoint_is_code_4_and_says_to_retry() {
        let mut h = Harness::new("udp-mdc-closing");
        let control = control_channel();
        let now = h.now;
        h.conductor
            .add_publication(1, 2, 7, control.as_bytes(), false, now);
        assert_eq!(h.last_error(2), None);
        let closed_at = now_nanos();
        h.sender.duty_cycle(closed_at);
        h.conductor.remove_publication(1, 20, 2, closed_at);
        // During the linger the endpoint is open: an add opens its publication there.
        h.conductor
            .add_publication(1, 3, 7, control.as_bytes(), false, now);
        assert_eq!(h.last_error(3), None);
        assert_eq!(h.conductor.publications.len(), 2);
        assert_eq!(h.conductor.send_endpoints.len(), 1);
        assert_eq!(
            h.conductor.publications[1]
                .udp
                .as_ref()
                .map(|udp| udp.endpoint_id),
            Some(2),
            "the endpoint still active"
        );
        h.conductor.remove_publication(1, 21, 3, closed_at);
        // Both lingers end at the timeout, with no destination and so no connection;
        // the conductor takes the closes and closes the endpoint, whose socket is still
        // with the sender until its next cycle.
        h.sender.duty_cycle(closed_at);
        h.sender
            .duty_cycle(closed_at + spec::nanos(cavalry_proto::udp::LINGER));
        assert_eq!(h.sender.positions_of(2), None);
        assert_eq!(h.sender.positions_of(3), None);
        h.conductor.duty_cycle(now_nanos());
        assert!(h.conductor.publications.is_empty());
        assert_eq!(h.conductor.send_endpoints.len(), 1);
        assert_eq!(h.conductor.send_endpoints[0].state, ChannelState::Closing);
        // The window: code 4, saying why and what to do, and nothing registered.
        h.conductor
            .add_publication(1, 4, 7, control.as_bytes(), false, now);
        let (code, message) = h.last_error(4).expect("an error");
        assert_eq!(code, 4, "{message}");
        assert!(
            message.contains("previous send endpoint is still closing"),
            "{message}"
        );
        assert!(message.contains("send the command again"), "{message}");
        assert!(h.conductor.publications.is_empty());
        assert_eq!(h.conductor.send_endpoints.len(), 1);
        // The sender drops the socket, the conductor frees the slot, and the add binds
        // the control address again.
        h.sender.duty_cycle(now_nanos());
        h.conductor.duty_cycle(now_nanos());
        assert!(h.conductor.send_endpoints.is_empty());
        h.conductor
            .add_publication(1, 5, 7, control.as_bytes(), false, now);
        assert_eq!(h.last_error(5), None);
        assert_eq!(h.conductor.send_endpoints.len(), 1);
        assert_eq!(h.conductor.send_endpoints[0].registration_id, 5);
        assert_eq!(h.conductor.send_endpoints[0].state, ChannelState::Active);
    }

    /// The send endpoints registry holds 64: the 65th resolved channel is code 10
    /// naming it, and a closed endpoint's slot is returned.
    #[test]
    fn the_send_endpoints_registry_fills_at_its_capacity() {
        let mut h = Harness::new("udp-endpoints-capacity");
        // Every log is made resident at creation: the smallest term for 64 of them.
        h.conductor.term_length = cavalry_shm::log::layout::MIN_TERM_LENGTH;
        let now = h.now;
        for i in 0..spec::MAX_SEND_ENDPOINTS {
            let correlation_id = 10 + i as i64;
            let channel = format!("cavalry:udp?endpoint=127.0.0.1:{}", 40_100 + i);
            h.conductor
                .add_publication(1, correlation_id, 7, channel.as_bytes(), false, now);
            assert_eq!(h.last_error(correlation_id), None, "endpoint {i}");
        }
        assert_eq!(h.conductor.send_endpoints.len(), spec::MAX_SEND_ENDPOINTS);
        h.conductor.add_publication(
            1,
            999,
            7,
            b"cavalry:udp?endpoint=127.0.0.1:40999",
            false,
            now,
        );
        let (code, message) = h.last_error(999).expect("an error");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("send endpoints"), "{message}");
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), 1);
        assert_eq!(h.conductor.publications.len(), spec::MAX_SEND_ENDPOINTS);
    }

    /// The sender's request queue full (invariant 3; `spec/client-protocol.md`,
    /// "Commands"): `add publication` on a `cavalry:udp` channel is code 10 naming the
    /// queue, counted, with no socket opened, no endpoint and no publication registered
    /// and no counter allocated; a close that finds the queue full waits in the
    /// deferred list, and an add behind it is code 10 too; once the sender drains the
    /// queue the close reaches it, the list empties, and the next add goes through.
    #[test]
    fn a_udp_add_is_refused_while_the_senders_queue_is_full_and_a_close_waits_behind_it() {
        let mut h = Harness::new("udp-sender-queue-full");
        let mut receiver = TestReceiver::bind();
        let other = TestReceiver::bind();
        connect_udp(&mut h, &mut receiver, 2, 7, 131072);
        h.now = now_nanos();
        assert!(
            h.conductor.sender.requests.is_empty(),
            "the sender took the endpoint and the publication"
        );

        // The request queue full of removes of endpoints the sender does not hold.
        let mut filler = 0;
        while h
            .conductor
            .sender
            .requests
            .offer(sender::Request::RemoveEndpoint {
                registration_id: 1_000_000 + filler,
            })
            .is_ok()
        {
            filler += 1;
        }
        assert_eq!(filler, spec::SENDER_QUEUE_CAPACITY as i64);
        let allocated = h.counter(COUNTERS_ALLOCATED);

        // An add on another channel, which needs a socket, an endpoint and a
        // publication, and one on the open channel's second stream, which needs the
        // publication alone: both code 10, counted, and the room check runs before
        // the socket is opened, so no endpoint, publication or counter is left behind.
        h.conductor
            .add_publication(1, 30, 8, &other.channel(), false, h.now);
        let (code, message) = h.last_error(30).expect("an error event");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("request queue"), "{message}");
        h.conductor
            .add_publication(1, 31, 8, &receiver.channel(), false, h.now);
        let (code, message) = h.last_error(31).expect("an error event");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("request queue"), "{message}");
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), 2);
        assert_eq!(h.conductor.send_endpoints.len(), 1, "no endpoint opened");
        assert_eq!(h.counter(CHANNEL_OPENED), 1);
        assert_eq!(h.conductor.publications.len(), 1, "nothing registered");
        assert_eq!(h.conductor.registration_count, 1);
        assert_eq!(
            h.counter(COUNTERS_ALLOCATED),
            allocated,
            "no counter allocated"
        );
        assert!(
            h.conductor.sender_deferred.is_empty(),
            "an add is refused, never deferred"
        );

        // A close waits for room; an add behind it is code 10 as well.
        h.conductor.remove_publication(1, 32, 2, h.now);
        assert_eq!(h.conductor.sender_deferred.len(), 1, "the close waits");
        assert!(h.conductor.publications[0].closing.is_some());
        h.conductor
            .add_publication(1, 33, 8, &other.channel(), false, h.now);
        let (code, message) = h.last_error(33).expect("an error event");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("request queue"), "{message}");
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), 3);
        assert_eq!(h.conductor.send_endpoints.len(), 1);

        // The sender drains the queue, answering every remove; the close reaches it
        // and the publication lingers there; the list is empty and the next add goes
        // through.
        for _ in 0..10_000 {
            h.now += MS;
            h.sender.duty_cycle(h.now);
            h.conductor.duty_cycle(h.now);
            if h.conductor.sender_deferred.is_empty() && h.conductor.sender.requests.is_empty() {
                break;
            }
        }
        assert!(
            h.conductor.sender_deferred.is_empty(),
            "the close was offered"
        );
        assert!(
            h.conductor.sender.requests.is_empty(),
            "the sender took every request"
        );
        assert_eq!(h.sender.pending_replies(), 0);
        assert!(
            h.sender.positions_of(2).is_some(),
            "the sender lingers the publication"
        );
        assert_eq!(
            h.conductor.publications.len(),
            1,
            "the conductor keeps its log until the sender's reply"
        );
        h.conductor
            .add_publication(1, 34, 8, &other.channel(), false, h.now);
        assert_eq!(h.last_error(34), None);
        assert_eq!(h.conductor.send_endpoints.len(), 2, "the add went through");
        assert_eq!(h.conductor.publications.len(), 2);
    }

    /// The sender's deferred requests at capacity (invariant 3): the publications
    /// registry full of `cavalry:udp` publications over the full registry of
    /// endpoints, every publication closed while the request queue is full, and every
    /// close completed, which releases its endpoint and the last release removes it,
    /// fills the list to exactly its bound, one close per publication and one remove
    /// per endpoint, and not past its allocation; every request reaches the sender once
    /// it catches up.
    #[test]
    fn the_senders_deferred_requests_are_bounded_by_a_close_per_publication_and_a_remove_per_endpoint()
     {
        let mut h = Harness::new("udp-sender-deferred-capacity");
        // Every log is made resident at creation: the smallest term for 1024 of them.
        h.conductor.term_length = cavalry_shm::log::layout::MIN_TERM_LENGTH;
        let now = h.now;
        let per_endpoint = spec::MAX_PUBLICATIONS / spec::MAX_SEND_ENDPOINTS;
        let mut ids: Vec<i64> = Vec::with_capacity(spec::MAX_PUBLICATIONS);
        for endpoint in 0..spec::MAX_SEND_ENDPOINTS {
            let channel = format!("cavalry:udp?endpoint=127.0.0.1:{}", 40_300 + endpoint);
            for stream in 0..per_endpoint {
                let correlation_id = 10 + ids.len() as i64;
                h.conductor.add_publication(
                    1,
                    correlation_id,
                    100 + stream as i32,
                    channel.as_bytes(),
                    false,
                    now,
                );
                assert_eq!(
                    h.last_error(correlation_id),
                    None,
                    "publication {correlation_id}"
                );
                ids.push(correlation_id);
            }
        }
        assert_eq!(h.conductor.publications.len(), spec::MAX_PUBLICATIONS);
        assert_eq!(h.conductor.send_endpoints.len(), spec::MAX_SEND_ENDPOINTS);
        assert_eq!(
            h.conductor.sender.requests.len(),
            spec::MAX_PUBLICATIONS + spec::MAX_SEND_ENDPOINTS,
            "an endpoint and a publication each, waiting for the sender"
        );

        // The rest of the request queue full of removes of endpoints the sender does
        // not hold, so that nothing the conductor offers from here finds room.
        while h
            .conductor
            .sender
            .requests
            .offer(sender::Request::RemoveEndpoint {
                registration_id: 1_000_000,
            })
            .is_ok()
        {}
        let allocated = h.conductor.sender_deferred.capacity();

        // Every publication closed: one close each, deferred.
        for (i, &id) in ids.iter().enumerate() {
            h.conductor
                .remove_publication(1, 70_000 + i as i64, id, now);
        }
        assert_eq!(
            h.conductor.sender_deferred.len(),
            spec::MAX_PUBLICATIONS,
            "one close per publication"
        );
        // Every close completing releases the publication's endpoint, and the last
        // release of each endpoint sends its remove: one per endpoint, behind the
        // closes. `close_if_done` releases once the sender has answered the close,
        // which it cannot while the queue is full, so the release is called here as
        // the completion would call it, once per publication.
        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
        for index in 0..spec::MAX_PUBLICATIONS {
            let endpoint_id = h.conductor.publications[index]
                .udp
                .as_ref()
                .expect("a cavalry:udp publication")
                .endpoint_id;
            h.conductor.release_send_endpoint(endpoint_id, reuse);
        }
        assert_eq!(
            h.conductor.sender_deferred.len(),
            SENDER_DEFERRED_CAPACITY - spec::MAX_REPLAY_PUBLICATIONS,
            "and one remove per endpoint; the rest of the bound is a close per replay \
             publication over cavalry:udp (ADR 0043)"
        );
        assert_eq!(
            h.conductor.sender_deferred.capacity(),
            allocated,
            "the list did not grow"
        );
        assert_eq!(h.counter(CHANNEL_CLOSED), spec::MAX_SEND_ENDPOINTS as i64);
        assert!(
            h.conductor
                .send_endpoints
                .iter()
                .all(|endpoint| endpoint.state == ChannelState::Closing)
        );

        for _ in 0..10_000 {
            h.now += MS;
            h.sender.duty_cycle(h.now);
            h.conductor.duty_cycle(h.now);
            if h.conductor.sender_deferred.is_empty() && h.conductor.sender.requests.is_empty() {
                break;
            }
        }
        assert!(
            h.conductor.sender_deferred.is_empty(),
            "every request was offered"
        );
        assert!(
            h.conductor.sender.requests.is_empty(),
            "the sender took every one"
        );
        assert!(
            h.conductor.send_endpoints.is_empty(),
            "every endpoint's socket is closed and its counters freed on the reply"
        );
    }

    /// The sender's pending replies at capacity (invariant 3): with the conductor
    /// taking no replies, the reply queue and then the pending replies fill; at the
    /// bound the sender handles no request and moves no publication's connection state,
    /// a connection timeout that is due included, but keeps sending; once the conductor
    /// takes replies, the held ones go through and the due timeout disconnects the
    /// publication.
    #[test]
    fn the_senders_pending_replies_are_bounded_and_it_keeps_sending() {
        let mut h = Harness::new("udp-sender-pending");
        let mut receiver = TestReceiver::bind();
        connect_udp(&mut h, &mut receiver, 2, 7, 131072);
        h.now = now_nanos();

        // Removes of endpoints the sender does not hold, each answered, until the
        // replies are at their bound.
        let fill = |h: &mut Harness| {
            while h
                .conductor
                .sender
                .requests
                .offer(sender::Request::RemoveEndpoint {
                    registration_id: 1_000_000,
                })
                .is_ok()
            {}
        };
        for _ in 0..10_000 {
            if h.sender.pending_replies() >= sender::PENDING_REPLIES {
                break;
            }
            fill(&mut h);
            h.now += MS;
            h.sender.duty_cycle(h.now);
        }
        assert_eq!(h.sender.pending_replies(), sender::PENDING_REPLIES);
        assert_eq!(
            h.conductor.sender.replies.len(),
            spec::SENDER_QUEUE_CAPACITY
        );
        fill(&mut h);
        let queued = h.conductor.sender.requests.len();
        assert_eq!(h.counter(PUBLICATION_DISCONNECTED), 0);

        // Three frames committed and the connection timeout due: the sender sends the
        // frames in one burst and moves its position, handles no request, and holds
        // the disconnect, since it could not reply.
        let mut appender = h.conductor.publications[0]
            .log
            .appender()
            .expect("an appender");
        let limit = h.conductor.publications[0].limit.get();
        for n in 0..3u8 {
            appender.offer(&[n; 32], limit, 0).expect("offered");
        }
        h.now += spec::nanos(CONNECTION_TIMEOUT) + MS;
        for _ in 0..4 {
            h.now += MS;
            h.sender.duty_cycle(h.now);
        }
        assert_eq!(
            h.sender.positions_of(2).map(|positions| positions.0),
            Some(192),
            "it kept sending"
        );
        assert_eq!(h.counter(SENDER_FRAMES_SENT), 3);
        let mut data = false;
        for _ in 0..16 {
            match receiver.next_control() {
                Some(ControlFrame::Data) => {
                    data = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(data, "the burst reached the receiver");
        assert_eq!(
            h.conductor.sender.requests.len(),
            queued,
            "it handled no request"
        );
        assert_eq!(h.sender.pending_replies(), sender::PENDING_REPLIES);
        assert_eq!(
            h.counter(PUBLICATION_DISCONNECTED),
            0,
            "it moved no connection state"
        );
        assert!(h.conductor.publications[0].log.is_connected());

        // The conductor takes replies again: the held replies go through, the sender
        // handles requests, and the due timeout disconnects the publication.
        for _ in 0..10_000 {
            h.now += MS;
            h.conductor.duty_cycle(h.now);
            h.sender.duty_cycle(h.now);
            if h.counter(PUBLICATION_DISCONNECTED) == 1
                && h.conductor.sender.requests.is_empty()
                && h.sender.pending_replies() == 0
            {
                break;
            }
        }
        h.conductor.duty_cycle(h.now);
        assert_eq!(h.counter(PUBLICATION_DISCONNECTED), 1);
        assert_eq!(h.sender.pending_replies(), 0);
        assert!(h.conductor.sender.requests.is_empty());
        assert!(
            !h.conductor.publications[0].log.is_connected(),
            "the conductor acted on the held reply"
        );
        assert!(
            !h.conductor.publications[0]
                .udp
                .as_ref()
                .expect("a cavalry:udp publication")
                .connected
        );
        drop(appender);
    }

    /// The full case first (invariant 3): every record a `cavalry:udp` publication and
    /// its endpoint held is freed when the close completes, `sender.position` and
    /// `sender.limit` with `publication.position`, `publication.limit` and the
    /// endpoint's five, and every one is allocated again once the reuse timeout has
    /// passed. A driver that kept the sender's two would lose two of the counters
    /// file's records per publication for its life.
    #[test]
    fn a_closed_udp_publication_frees_its_eleven_records_and_they_are_reused_after_the_timeout() {
        let mut h = Harness::new("udp-counters-freed");
        let mut receiver = TestReceiver::bind();
        let allocated_before = h.counter(COUNTERS_ALLOCATED);
        let freed_before = h.counter(COUNTERS_FREED);
        let setup = connect_udp(&mut h, &mut receiver, 2, 7, 131072);
        let mut held: Vec<u32> = {
            let publication = &h.conductor.publications[0];
            let udp = publication.udp.as_ref().expect("a cavalry:udp publication");
            let endpoint = &h.conductor.send_endpoints[0];
            vec![
                publication.position.id(),
                publication.limit.id(),
                udp.sender_position.id(),
                udp.sender_limit_id,
                udp.receivers_id,
                endpoint.status.id(),
                endpoint.socket_rcvbuf.id(),
                endpoint.socket_sndbuf.id(),
                endpoint.images.id(),
                endpoint.publications.id(),
                endpoint.destinations.id(),
            ]
        };
        held.sort_unstable();
        assert_eq!(h.counter(COUNTERS_ALLOCATED), allocated_before + 11);

        // The close: handed to the sender, acknowledged by the receiver with the
        // end-of-stream flag, completed by the conductor with the endpoint's.
        let now = now_nanos();
        h.conductor.remove_publication(1, 20, 2, now);
        h.sender.duty_cycle(now);
        receiver.send_status_message(&StatusMessage {
            flags: STATUS_MESSAGE_END_OF_STREAM,
            session_id: setup.session_id,
            stream_id: 7,
            consumption_term_id: setup.active_term_id,
            consumption_term_offset: 0,
            receiver_window_length: 131072,
            receiver_id: 9,
            group_tag: None,
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !h.conductor.send_endpoints.is_empty() {
            let now = now_nanos();
            h.sender.duty_cycle(now);
            h.conductor.duty_cycle(now);
            assert!(Instant::now() < deadline, "the endpoint did not close");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(h.conductor.publications.is_empty());
        for &id in &held {
            assert!(
                matches!(
                    h.conductor.counters.counter(id),
                    Err(CountersError::Unallocated { .. })
                ),
                "record {id} is freed"
            );
        }
        assert_eq!(h.counter(COUNTERS_ALLOCATED), allocated_before);
        assert_eq!(h.counter(COUNTERS_FREED), freed_before + 11);

        // Reusable once the reuse timeout has passed: the next eleven allocations take
        // exactly these records, the lowest that are free.
        std::thread::sleep(spec::COUNTER_REUSE_TIMEOUT + Duration::from_millis(100));
        let mut reused: Vec<u32> = (0..11u8)
            .map(|i| {
                h.conductor
                    .counters
                    .allocate(SENDER_POSITION, &[i], "sender.position reused")
                    .expect("a record is free again")
                    .id()
            })
            .collect();
        reused.sort_unstable();
        assert_eq!(
            reused, held,
            "the freed records are the ones allocated again"
        );
    }

    /// A closed publication lingers with the sender: the conductor keeps its log until
    /// the sender's reply, the heartbeats carry EOS once the sender position reached the
    /// end, a status message with the end-of-stream flag ends the linger, and the last
    /// publication's close closes the endpoint and frees its counters.
    #[test]
    fn a_closed_udp_publication_lingers_with_the_sender_and_its_endpoint_closes_after_it() {
        let mut h = Harness::new("udp-close");
        let mut receiver = TestReceiver::bind();
        let setup = connect_udp(&mut h, &mut receiver, 2, 7, 131072);
        let log = h.dir.path().join("publications/2.logbuffer");
        assert!(log.is_file());
        let now = now_nanos();
        h.conductor.remove_publication(1, 20, 2, now);
        assert!(h.conductor.publications[0].closing.is_some());
        h.conductor
            .duty_cycle(now + spec::nanos(spec::PUBLICATION_LINGER) * 2);
        assert_eq!(
            h.conductor.publications.len(),
            1,
            "the sender has not closed it"
        );
        assert!(log.is_file());

        // The sender took the close; a heartbeat after the interval carries EOS, since
        // the sender position is at the end of stream position. A heartbeat without it
        // may be waiting from the connect, when that took longer than the interval.
        h.sender
            .duty_cycle(now + spec::nanos(Duration::from_millis(200)));
        let heartbeat = loop {
            match receiver.next_control() {
                Some(ControlFrame::Heartbeat(heartbeat)) if heartbeat.is_end_of_stream() => {
                    break heartbeat;
                }
                Some(ControlFrame::Heartbeat(_) | ControlFrame::Setup(_)) => {
                    h.sender
                        .duty_cycle(now_nanos() + spec::nanos(Duration::from_millis(200)));
                }
                other => panic!("expected a heartbeat with EOS, got {other:?}"),
            }
        };
        assert_eq!(heartbeat.session_id, setup.session_id);

        // The receiver acknowledges with the end-of-stream flag: the sender replies
        // closed, the conductor removes the log and closes the endpoint, the sender
        // drops the socket, and the conductor frees the endpoint's counters.
        receiver.send_status_message(&StatusMessage {
            flags: STATUS_MESSAGE_END_OF_STREAM,
            session_id: setup.session_id,
            stream_id: 7,
            consumption_term_id: setup.active_term_id,
            consumption_term_offset: 0,
            receiver_window_length: 131072,
            receiver_id: 9,
            group_tag: None,
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !h.conductor.send_endpoints.is_empty() {
            let now = now_nanos();
            h.sender.duty_cycle(now);
            h.conductor.duty_cycle(now);
            assert!(Instant::now() < deadline, "the endpoint did not close");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(h.conductor.publications.is_empty());
        assert!(!log.exists(), "the log is removed");
        assert_eq!(h.counter(CHANNEL_CLOSED), 1);
        assert_eq!(h.sender.positions_of(2), None, "the sender dropped it");
    }

    /// Every frame of the recording in order, from its segment files, folded into one
    /// number the same way the publisher folded what it offered.
    fn fold_recording(log_dir: &Path, recording_id: i64) -> (i64, u64) {
        let reader = cavalry_log::catalog::CatalogReader::open(log_dir).expect("the catalog opens");
        let descriptor = reader
            .get(recording_id)
            .expect("the slot reads")
            .expect("the slot holds the recording");
        let stop = descriptor.stop_position();
        let mut position = descriptor.start_position();
        let mut fold = 0u64;
        let mut header = [0u8; FRAME_HEADER];
        while position < stop {
            let located = locate(&descriptor, position).expect("the position locates");
            let file = std::fs::File::open(log_dir.join(&located.segment_file)).expect("segment");
            file.read_exact_at(&mut header, located.offset_in_segment as u64)
                .expect("the header reads");
            let left = (descriptor.term_length() - located.term_offset) as usize;
            let parsed = DataFrameHeader::parse_within(&header, left).expect("the frame checks");
            if parsed.frame_type() == FrameType::Data {
                let mut body = vec![0u8; parsed.frame_length() as usize - FRAME_HEADER];
                file.read_exact_at(
                    &mut body,
                    located.offset_in_segment as u64 + FRAME_HEADER as u64,
                )
                .expect("the body reads");
                fold = fold_bytes(fold, &body);
            }
            position += parsed.aligned_length() as i64;
        }
        (stop, fold)
    }

    fn fold_bytes(mut fold: u64, bytes: &[u8]) -> u64 {
        for &byte in bytes {
            fold = fold
                .wrapping_mul(1_099_511_628_211)
                .wrapping_add(u64::from(byte));
        }
        fold
    }

    /// The full case first (invariant 3): a publisher that writes as fast as it can,
    /// a recording and no subscriber. The limit never passes the recorder's position
    /// plus the window, the publisher is back-pressured while the recorder does not
    /// run and released as it writes, two terms are crossed and cleaned under the
    /// recorder, a roll that fails stops the recording at the position written, and
    /// every byte up to the stop reads back from the segment files as it was offered.
    #[test]
    fn a_publisher_faster_than_the_recorder_is_back_pressured_and_nothing_is_lost() {
        let mut h = Harness::new("recorder-back-pressure");
        let now = h.now;
        h.conductor.add_publication(1, 2, 7, IPC_CHANNEL, true, now);
        h.conductor.start_recording(3, 7, IPC_CHANNEL, now);
        assert_eq!(h.conductor.recordings.len(), 1, "the entry is registered");
        assert_eq!(h.conductor.recordings[0].state, RecordingState::Starting);
        h.conductor.duty_cycle(now);
        let limit = h.conductor.publications[0].limit.get();
        assert_eq!(limit, WINDOW, "a recording alone connects the publication");

        // The recorder takes the slot; the conductor allocates the counters and attaches.
        h.recorder.duty_cycle(now);
        h.conductor.duty_cycle(now);
        h.recorder.duty_cycle(now);
        assert_eq!(h.conductor.recordings[0].state, RecordingState::Live);
        assert_eq!(h.conductor.recordings[0].recording_id, Some(0));
        assert_eq!(h.counter(RECORDING_STARTED), 1);
        let position_counter = h.conductor.recordings[0]
            .position
            .clone()
            .expect("attached");

        // The publisher fills the window while the recorder sleeps: back pressured at
        // start + window, and the limit does not move however often the conductor cycles.
        let segment = i64::from(cavalry_log::layout::SEGMENT_FILE_LENGTH);
        let mut fold = 0u64;
        let mut offered: i64 = 0;
        let mut message = [0u8; 32];
        let mut appender = h.conductor.publications[0]
            .log
            .appender()
            .expect("an appender");
        let mut offer = |appender: &mut cavalry_shm::log::TermAppender,
                         limit: i64,
                         fold: &mut u64,
                         offered: &mut i64|
         -> bool {
            message[..8].copy_from_slice(&offered.to_le_bytes());
            loop {
                match appender.offer(&message, limit, 0) {
                    Ok(_) => {
                        *fold = fold_bytes(*fold, &message);
                        *offered += 1;
                        return true;
                    }
                    Err(ClaimError::AdminAction) => continue,
                    Err(ClaimError::BackPressured) => return false,
                    Err(error) => panic!("{error}"),
                }
            }
        };
        while offer(
            &mut appender,
            h.conductor.publications[0].limit.get(),
            &mut fold,
            &mut offered,
        ) {}
        let producer = appender.position().expect("the position");
        assert_eq!(producer, WINDOW, "back pressured at the window");
        for _ in 0..3 {
            h.now += MS;
            h.conductor.duty_cycle(h.now);
            assert_eq!(
                h.conductor.publications[0].limit.get(),
                WINDOW,
                "the limit waits for the recorder"
            );
        }

        // The recorder writes a block per cycle; the limit follows it, never ahead of
        // recording.position + window; the publisher crosses two terms. A file in the
        // next segment's place makes the roll at the segment's end fail.
        let in_the_way = h
            .log_dir
            .join(cavalry_log::layout::segment_file_name(0, segment));
        std::fs::write(&in_the_way, b"").expect("a file in the way");
        let mut stopped = false;
        for _ in 0..100_000 {
            h.cycle();
            let recorded = position_counter.load_acquire();
            let limit = h.conductor.publications[0].limit.get();
            if h.conductor.recordings.is_empty() {
                stopped = true;
                break;
            }
            assert!(
                limit <= recorded + WINDOW,
                "limit {limit} past recorded {recorded} + window"
            );
            while offer(
                &mut appender,
                h.conductor.publications[0].limit.get(),
                &mut fold,
                &mut offered,
            ) {}
        }
        assert!(
            stopped,
            "the roll into the segment in the way stops the recording"
        );
        assert_eq!(h.counter(RECORDING_WRITE_FAILED), 1);
        assert_eq!(h.counter(RECORDING_STOPPED), 1);
        assert!(
            offered * 64 >= segment,
            "the publisher crossed both terms: {offered}"
        );

        // Everything up to the stop reads back as offered, in order.
        let (stop, read_back) = fold_recording(&h.log_dir, 0);
        assert_eq!(stop, segment, "stopped where the roll failed");
        let mut expected = 0u64;
        for n in 0..(segment / 64) {
            message[..8].copy_from_slice(&n.to_le_bytes());
            expected = fold_bytes(expected, &message);
        }
        assert_eq!(
            read_back, expected,
            "the recording is what the publisher offered"
        );
        assert_eq!(position_counter.load_acquire(), segment);
        // Freed from the recording, the publication with no subscriber is not connected.
        h.cycle();
        assert!(!h.conductor.publications[0].log.is_connected());
        drop(appender);
    }

    /// The recordings registry at capacity (invariant 3): live publications are at
    /// most 1024, so the registry fills only through recordings of closed publications
    /// still with the recorder; the 1025th start recording is code 10 and counted. The
    /// logs take the smallest term: each is made resident when it is created (ADR
    /// 0027), and 1,024 of the spec's 64 MiB terms would commit 192 GiB, while the bound
    /// under test is the registry's.
    #[test]
    fn the_recordings_registry_is_bounded_and_says_so() {
        let mut h = Harness::new("recordings-registry");
        h.conductor.term_length = cavalry_shm::log::layout::MIN_TERM_LENGTH;
        let now = h.now;
        let n = spec::MAX_RECORDINGS as i64;
        for i in 0..n {
            h.conductor
                .add_publication(1, 10 + i, 7, IPC_CHANNEL, true, now);
            h.conductor.start_recording(10_000 + i, 7, IPC_CHANNEL, now);
        }
        assert_eq!(h.conductor.recordings.len(), spec::MAX_RECORDINGS);
        for i in 0..n {
            h.conductor.remove_publication(1, 20_000 + i, 10 + i, now);
        }
        assert_eq!(
            h.conductor.registration_count, 0,
            "every publication is closing"
        );
        h.conductor
            .add_publication(1, 30_000, 7, IPC_CHANNEL, true, now);
        h.conductor.start_recording(30_001, 7, IPC_CHANNEL, now);
        let (code, message) = h.last_error(30_001).expect("an error event");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("recordings registry"), "{message}");
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), 1);
        assert_eq!(h.conductor.recordings.len(), spec::MAX_RECORDINGS);
    }

    /// The catalog's bound, at the seam: a full catalog is answered with code 10 and
    /// counted, files that cannot be created with code 12 and counted. The catalog's
    /// own test covers the writer at its bound; a real full catalog is a gigabyte.
    #[test]
    fn a_full_catalog_and_a_failed_start_are_answered_and_counted() {
        let mut h = Harness::new("catalog-full");
        let now = h.now;
        h.conductor.add_publication(1, 2, 7, IPC_CHANNEL, true, now);
        h.conductor.start_recording(3, 7, IPC_CHANNEL, now);
        let failure = start_failure(RecorderError::Catalog(CatalogError::Full));
        assert!(matches!(failure, StartFailure::CatalogFull));
        h.conductor.on_start_failed(2, 3, failure);
        let (code, message) = h.last_error(3).expect("an error event");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("catalog"), "{message}");
        assert_eq!(h.counter(RECORDING_CATALOG_FULL), 1);
        assert!(
            h.conductor.recordings.is_empty(),
            "the entry failed and went"
        );

        h.conductor.start_recording(4, 7, IPC_CHANNEL, now);
        let io = RecorderError::Io {
            what: "create",
            path: h.log_dir.join("0-0.rec"),
            source: std::io::Error::other("no space left on device"),
        };
        let failure = start_failure(io);
        assert!(matches!(failure, StartFailure::Files(_)));
        h.conductor.on_start_failed(2, 4, failure);
        let (code, message) = h.last_error(4).expect("an error event");
        assert_eq!(code, 12, "{message}");
        assert!(message.contains("no space left"), "{message}");
        assert_eq!(h.counter(RECORDING_START_FAILED), 1);
        // The requests the recorder never took are still queued; drop them with it.
        assert!(h.conductor.recordings.is_empty());
    }

    /// A committed frame that fails a check of spec/frame.md stops the recording at
    /// that frame, counted as a subscriber's report is; the frames before it are kept.
    #[test]
    fn a_frame_that_fails_its_check_stops_the_recording_there() {
        let mut h = Harness::new("recorder-bad-frame");
        let now = h.now;
        h.conductor.add_publication(1, 2, 7, IPC_CHANNEL, true, now);
        h.conductor.start_recording(3, 7, IPC_CHANNEL, now);
        h.cycle();
        h.cycle();
        assert_eq!(h.conductor.recordings[0].state, RecordingState::Live);
        let mut appender = h.conductor.publications[0]
            .log
            .appender()
            .expect("an appender");
        let limit = h.conductor.publications[0].limit.get();
        for n in 0..4u8 {
            appender.offer(&[n; 32], limit, 0).expect("offered");
        }
        // Frame 3, at position 192, gets a version byte no reader accepts.
        let log_file = h.dir.path().join(log_layout::file_name(2));
        let file = OpenOptions::new()
            .write(true)
            .open(&log_file)
            .expect("the log opens");
        file.write_all_at(&[0xFF], 192 + 4)
            .expect("the version byte is poisoned");
        for _ in 0..8 {
            h.cycle();
            if h.conductor.recordings.is_empty() {
                break;
            }
        }
        assert!(h.conductor.recordings.is_empty(), "the recording stopped");
        assert_eq!(h.counter(LOG_BUFFER_REJECTED_FRAME), 1);
        assert_eq!(h.counter(RECORDING_STOPPED), 1);
        let (stop, _) = fold_recording(&h.log_dir, 0);
        assert_eq!(
            stop, 192,
            "the recording ends at the last frame that checked"
        );
        drop(appender);
    }

    /// Offers 32-byte messages, each numbered from `offered`, until back pressured at
    /// `limit`, folding what went as `fold_recording` folds what was recorded.
    pub(super) fn offer_to(
        appender: &mut cavalry_shm::log::TermAppender,
        limit: i64,
        offered: &mut i64,
    ) -> u64 {
        let mut fold = 0u64;
        let mut message = [0u8; 32];
        loop {
            message[..8].copy_from_slice(&offered.to_le_bytes());
            match appender.offer(&message, limit, 0) {
                Ok(_) => {
                    fold = fold_bytes(fold, &message);
                    *offered += 1;
                }
                Err(ClaimError::AdminAction) => {}
                Err(ClaimError::BackPressured) => return fold,
                Err(error) => panic!("{error}"),
            }
        }
    }

    /// The fold of the first `count` messages `offer_to` numbers.
    fn fold_offered(count: i64) -> u64 {
        let mut message = [0u8; 32];
        let mut fold = 0u64;
        for n in 0..count {
            message[..8].copy_from_slice(&n.to_le_bytes());
            fold = fold_bytes(fold, &message);
        }
        fold
    }

    /// A start the counters file refuses once the recorder has taken the slot: the
    /// recorder goes on reading the log until it handles the stop, so the entry stays
    /// a floor at its start position until the recorder's reply. A subscriber that
    /// read everything does not lift the limit past start + window meanwhile, the
    /// recording stops at the position written, and its bytes are what was offered.
    #[test]
    fn a_start_the_counters_file_refuses_holds_the_limit_until_the_recorder_stops() {
        let mut h = Harness::new("recorder-counters-full");
        let now = h.now;
        h.conductor.add_publication(1, 2, 7, IPC_CHANNEL, true, now);
        h.conductor.add_subscription(1, 5, 7, IPC_CHANNEL, h.now);
        assert_eq!(h.conductor.images.len(), 1, "the subscriber has an image");
        h.conductor.start_recording(3, 7, IPC_CHANNEL, now);
        h.conductor.duty_cycle(now);
        assert_eq!(h.conductor.publications[0].limit.get(), WINDOW);

        // The recorder takes the slot; its Ready waits in the reply queue while every
        // free record of the counters file is taken.
        h.recorder.duty_cycle(now);
        let mut taken = 0;
        while h
            .conductor
            .counters
            .allocate(RECORDING_POSITION, &[0; 8], "filler")
            .is_ok()
        {
            taken += 1;
        }
        assert!(taken > 0, "the counters file had free records");

        // The publisher fills the window; the recorder writes a block of it, before
        // the conductor has read its Ready.
        let mut appender = h.conductor.publications[0]
            .log
            .appender()
            .expect("an appender");
        let mut offered = 0;
        offer_to(&mut appender, WINDOW, &mut offered);
        assert_eq!(appender.position().expect("the position"), WINDOW);
        h.recorder.duty_cycle(now);
        assert_eq!(h.recorder.live_recordings(), 1, "the recorder is writing");

        // The conductor finds no counters, while the subscriber reads everything: the
        // limit waits for the recorder's stop.
        h.conductor.images[0].position.store_release(WINDOW);
        h.conductor.duty_cycle(now);
        for _ in 0..3 {
            h.now += MS;
            h.conductor.duty_cycle(h.now);
            assert_eq!(
                h.conductor.publications[0].limit.get(),
                WINDOW,
                "the limit stays at start + window while the recorder reads the log"
            );
        }
        // Code 10 and counted, the entry failed but still registered.
        let (code, message) = h.last_error(3).expect("an error event");
        assert_eq!(code, 10, "{message}");
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), 1);
        assert_eq!(h.conductor.recordings.len(), 1, "the floor stays");
        assert_eq!(h.conductor.recordings[0].state, RecordingState::Failed);
        // A second start on the stream is told the recording is stopping.
        h.conductor.start_recording(4, 7, IPC_CHANNEL, h.now);
        let (code, message) = h.last_error(4).expect("an error event");
        assert_eq!(code, 22, "{message}");
        assert!(message.contains("stopping recording 0"), "{message}");

        // The recorder handles the stop at the position written; only its reply
        // removes the entry.
        h.now += MS;
        h.recorder.duty_cycle(h.now);
        h.conductor.duty_cycle(h.now);
        assert!(
            h.conductor.recordings.is_empty(),
            "the stop removed the floor"
        );
        assert_eq!(h.counter(RECORDING_STARTED), 0);
        assert_eq!(h.counter(RECORDING_STOPPED), 1);
        let (stop, read_back) = fold_recording(&h.log_dir, 0);
        assert!(stop > 0 && stop <= WINDOW, "stopped at {stop}");
        assert_eq!(
            read_back,
            fold_offered(stop / 64),
            "the recording is what the publisher offered"
        );
        drop(appender);
    }

    /// The deferred requests ahead of a start: a stop that found the request queue
    /// full waits, a start behind it is code 10 and counted and registers nothing, and
    /// once the recorder catches up the stop is answered and a start goes through.
    #[test]
    fn a_start_waits_behind_the_deferred_requests() {
        let mut h = Harness::new("recorder-deferred-order");
        let now = h.now;
        h.conductor.add_publication(1, 2, 7, IPC_CHANNEL, true, now);
        h.conductor.add_publication(1, 4, 8, IPC_CHANNEL, true, now);
        h.conductor.start_recording(3, 7, IPC_CHANNEL, now);
        h.cycle();
        h.cycle();
        assert_eq!(h.conductor.recordings[0].state, RecordingState::Live);

        // The request queue full of stops of ids no recording holds.
        let mut filler = 0;
        while h
            .conductor
            .recorder
            .requests
            .offer(Request::Stop {
                recording_id: 1_000_000 + filler,
                correlation_id: None,
            })
            .is_ok()
        {
            filler += 1;
        }
        assert_eq!(filler, spec::RECORDER_QUEUE_CAPACITY as i64);
        h.conductor.stop_recording(50, 0);
        assert_eq!(h.conductor.deferred.len(), 1, "the stop waits");
        h.conductor.start_recording(51, 8, IPC_CHANNEL, now);
        let (code, message) = h.last_error(51).expect("an error event");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("request queue"), "{message}");
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), 1);
        assert_eq!(h.conductor.recordings.len(), 1, "nothing registered");

        for _ in 0..1_000 {
            h.cycle();
            if h.conductor.recordings.is_empty() {
                break;
            }
        }
        assert!(
            h.conductor.recordings.is_empty(),
            "the stop reached the recorder"
        );
        assert!(h.conductor.deferred.is_empty());
        assert_eq!(h.counter(RECORDING_STOPPED), 1);
        h.conductor.start_recording(52, 8, IPC_CHANNEL, h.now);
        assert_eq!(h.last_error(52), None);
        assert_eq!(h.conductor.recordings.len(), 1, "the start went through");
    }

    /// The deferred requests at capacity (invariant 3): the registry full of
    /// recordings, each given its attach and its stop while the request queue is
    /// full, fills the list to exactly its bound and not past its allocation, and
    /// every request reaches the recorder once it catches up.
    #[test]
    fn the_deferred_requests_are_bounded_by_one_attach_and_one_stop_per_recording() {
        let mut h = Harness::new("recorder-deferred-capacity");
        let now = h.now;
        let allocated = h.conductor.deferred.capacity();
        while h
            .conductor
            .recorder
            .requests
            .offer(Request::Stop {
                recording_id: 1_000_000,
                correlation_id: None,
            })
            .is_ok()
        {}
        let n = spec::MAX_RECORDINGS as i64;
        for i in 0..n {
            h.conductor.recordings.push(RecordingEntry {
                publication_id: 100_000 + i,
                session_id: 0,
                stream_id: 9,
                start_position: 0,
                state: RecordingState::Starting,
                recording_id: None,
                position: None,
                synced: None,
                bytes: None,
                stop_requested: false,
                armed: false,
            });
        }
        h.conductor.recorder.next_recording_id = n;
        for i in 0..n {
            h.conductor.on_ready(100_000 + i, 60_000 + i, i, now);
            h.conductor.stop_recording(70_000 + i, i);
        }
        assert_eq!(h.conductor.deferred.len(), DEFERRED_CAPACITY);
        assert_eq!(
            h.conductor.deferred.capacity(),
            allocated,
            "the list did not grow"
        );
        assert_eq!(h.counter(RECORDING_STARTED), n);

        for _ in 0..10_000 {
            h.cycle();
            if h.conductor.deferred.is_empty() && h.conductor.recorder.requests.is_empty() {
                break;
            }
        }
        assert!(h.conductor.deferred.is_empty(), "every request was offered");
        assert!(
            h.conductor.recorder.requests.is_empty(),
            "the recorder took every one"
        );
    }

    /// The recorder's pending replies at capacity (invariant 3): with the conductor
    /// taking no replies, the reply queue and then the pending replies fill; at the
    /// bound the recorder handles no request and stops nothing, but keeps writing,
    /// and once the conductor takes replies the held stop goes through.
    #[test]
    fn the_recorders_pending_replies_are_bounded_and_it_keeps_writing() {
        let mut h = Harness::new("recorder-pending");
        let now = h.now;
        h.conductor.add_publication(1, 2, 7, IPC_CHANNEL, true, now);
        h.conductor.start_recording(3, 7, IPC_CHANNEL, now);
        h.cycle();
        h.cycle();
        assert_eq!(h.conductor.recordings[0].state, RecordingState::Live);
        let position = h.conductor.recordings[0]
            .position
            .clone()
            .expect("attached");

        // Stops of ids no recording holds, each answered, until the replies are at
        // their bound.
        let fill = |h: &mut Harness| {
            while h
                .conductor
                .recorder
                .requests
                .offer(Request::Stop {
                    recording_id: 1_000_000,
                    correlation_id: None,
                })
                .is_ok()
            {}
        };
        for _ in 0..10_000 {
            if h.recorder.pending_replies() >= PENDING_REPLIES {
                break;
            }
            fill(&mut h);
            h.now += MS;
            h.recorder.duty_cycle(h.now);
        }
        assert_eq!(h.recorder.pending_replies(), PENDING_REPLIES);
        assert_eq!(
            h.conductor.recorder.replies.len(),
            spec::RECORDER_QUEUE_CAPACITY
        );
        fill(&mut h);
        let queued = h.conductor.recorder.requests.len();

        // Four frames, the fourth with a version byte no reader accepts: the recorder
        // writes the three before it, and does not stop at it while it has no room.
        let mut appender = h.conductor.publications[0]
            .log
            .appender()
            .expect("an appender");
        let limit = h.conductor.publications[0].limit.get();
        for n in 0..4u8 {
            appender.offer(&[n; 32], limit, 0).expect("offered");
        }
        let log_file = h.dir.path().join(log_layout::file_name(2));
        let file = OpenOptions::new()
            .write(true)
            .open(&log_file)
            .expect("the log opens");
        file.write_all_at(&[0xFF], 192 + 4)
            .expect("the version byte is poisoned");
        for _ in 0..8 {
            h.now += MS;
            h.recorder.duty_cycle(h.now);
        }
        assert_eq!(position.load_acquire(), 192, "the recorder kept writing");
        assert_eq!(h.recorder.live_recordings(), 1, "it stopped nothing");
        assert_eq!(h.counter(LOG_BUFFER_REJECTED_FRAME), 0);
        assert_eq!(
            h.conductor.recorder.requests.len(),
            queued,
            "it handled no request"
        );
        assert_eq!(h.recorder.pending_replies(), PENDING_REPLIES);

        // The conductor takes replies again; the stop goes through.
        for _ in 0..10_000 {
            h.cycle();
            if h.conductor.recordings.is_empty() {
                break;
            }
        }
        assert!(h.conductor.recordings.is_empty(), "the recording stopped");
        assert_eq!(h.counter(LOG_BUFFER_REJECTED_FRAME), 1);
        assert_eq!(h.counter(RECORDING_STOPPED), 1);
        let (stop, _) = fold_recording(&h.log_dir, 0);
        assert_eq!(stop, 192);
        drop(appender);
    }
}

#[cfg(test)]
mod replay {
    //! The conductor, the recorder and the replayer driven by hand, duty cycle by duty
    //! cycle, on a clock the test advances: a recording made and stopped, then played
    //! into a subscription. The full case first, then pacing, the connect timeout, the
    //! stop, every error code, the registry's bound, a live recording followed, a
    //! damaged segment three ways, a replay across a segment roll, an end position
    //! inside a frame, and the request queue, the deferred list and the pending
    //! replies at their bounds.

    use std::fs::OpenOptions;
    use std::os::unix::fs::FileExt;
    use std::time::Instant;

    use cavalry_proto::client_protocol::{FOLLOW_LENGTH, NULL_LENGTH, NULL_POSITION};
    use cavalry_proto::frame::DataFrameHeader;
    use cavalry_proto::udp::{ControlFrame, STATUS_MESSAGE_END_OF_STREAM, StatusMessage};
    use cavalry_shm::counters::generated::{
        COUNTERS_ALLOCATED, COUNTERS_FREED, DRIVER_REGISTRY_FULL, LOG_BUFFER_REJECTED_FRAME,
        PUBLICATION_CONNECTED, RECORDING_DELETED, RECORDING_RETENTION_FAILED,
        RECORDING_SEGMENTS_PURGED, RECORDING_TRUNCATED, REPLAY_ENDED, REPLAY_FAILED,
        REPLAY_NOT_CONNECTED, REPLAY_START_FAILED, REPLAY_STARTED, REPLAY_STOPPED,
        SENDER_FRAMES_SENT, SENDER_STATUS_MESSAGES_RECEIVED, SENDER_STATUS_MESSAGES_REJECTED,
    };
    use cavalry_shm::log::ClaimError;

    use super::recording::{Harness, TestReceiver, offer_to};
    use super::*;

    const MS: i64 = 1_000_000;
    const SECOND: i64 = 1_000_000_000;
    const STREAM: i32 = 7;
    const REPLAY_STREAM: i32 = 2001;
    const CLIENT: i64 = 1;

    /// Every event not yet read, as (type id, message).
    fn events(h: &mut Harness) -> Vec<(i32, Vec<u8>)> {
        let mut out = Vec::new();
        while h
            .receiver
            .receive(|type_id, message| out.push((type_id, message.to_vec())))
            .expect("the receiver reads")
        {}
        out
    }

    /// The first `replay ready` for `correlation_id` among `events`: (replay session
    /// id, recording id, start position, session id, stream id).
    fn replay_ready(
        events: &[(i32, Vec<u8>)],
        correlation_id: i64,
    ) -> Option<(i64, i64, i64, i32, i32)> {
        events
            .iter()
            .find_map(|(type_id, message)| match Event::parse(*type_id, message) {
                Ok(Event::ReplayReady {
                    correlation_id: id,
                    replay_session_id,
                    recording_id,
                    start_position,
                    session_id,
                    stream_id,
                }) if id == correlation_id => Some((
                    replay_session_id,
                    recording_id,
                    start_position,
                    session_id,
                    stream_id,
                )),
                _ => None,
            })
    }

    /// The first `replay stopped` for `replay_session_id`: (position, reason).
    fn replay_stopped(events: &[(i32, Vec<u8>)], replay_session_id: i64) -> Option<(i64, i32)> {
        events
            .iter()
            .find_map(|(type_id, message)| match Event::parse(*type_id, message) {
                Ok(Event::ReplayStopped {
                    replay_session_id: id,
                    position,
                    reason,
                }) if id == replay_session_id => Some((position, reason)),
                _ => None,
            })
    }

    fn successes(events: &[(i32, Vec<u8>)]) -> Vec<i64> {
        events
            .iter()
            .filter_map(|(type_id, message)| match Event::parse(*type_id, message) {
                Ok(Event::OperationSuccess { correlation_id }) => Some(correlation_id),
                _ => None,
            })
            .collect()
    }

    fn errors(events: &[(i32, Vec<u8>)]) -> Vec<(i64, i32, String)> {
        events
            .iter()
            .filter_map(|(type_id, message)| match Event::parse(*type_id, message) {
                Ok(Event::Error {
                    offending_correlation_id,
                    error_code,
                    error_message,
                }) => Some((
                    offending_correlation_id,
                    error_code,
                    String::from_utf8_lossy(error_message).into_owned(),
                )),
                _ => None,
            })
            .collect()
    }

    /// A harness with the smallest term, a recorded exclusive publication on `STREAM`
    /// (registration 2, recording 0) live, and the recording's publication's appender.
    fn recorded(test: &str) -> (Harness, cavalry_shm::log::TermAppender) {
        let mut h = Harness::new(test);
        h.conductor.term_length = cavalry_shm::log::layout::MIN_TERM_LENGTH;
        let now = h.now;
        h.conductor
            .add_publication(CLIENT, 2, STREAM, IPC_CHANNEL, true, now);
        h.conductor.start_recording(3, STREAM, IPC_CHANNEL, now);
        h.cycle();
        h.cycle();
        assert_eq!(h.conductor.recordings[0].state, RecordingState::Live);
        let appender = h.conductor.publications[0]
            .log
            .appender()
            .expect("an appender");
        (h, appender)
    }

    /// The connected flag is stored after the limit, in the limit pass of the same
    /// duty cycle (ADR 0048; card 187): the commands pass that adds the image leaves
    /// the publication not connected with its limit where it was, and the limit pass
    /// raises the limit by the window and then sets the flag. A publisher that loads
    /// the flag and then the limit, as waiting for `is_connected()` and then claiming
    /// does, therefore never reads 1 with a limit at its position. A claim loads the
    /// limit first, so one whose two loads straddle the raise can still read
    /// `back pressured` once; the page says so.
    #[test]
    fn the_connected_flag_is_stored_after_the_limit_in_the_same_cycle() {
        let mut h = Harness::new("connected-after-limit");
        let now = h.now;
        h.conductor
            .add_publication(CLIENT, 2, STREAM, IPC_CHANNEL, true, now);
        h.cycle();
        let publication = &h.conductor.publications[0];
        assert!(!publication.log.is_connected());
        assert_eq!(publication.limit.get(), 0);
        let window = i64::from(publication.log.header().publication_window_length());

        // The commands pass: the image joins, and nothing the publisher reads moves.
        h.conductor
            .add_subscription(CLIENT, 3, STREAM, IPC_CHANNEL, h.now);
        assert_eq!(h.conductor.images.len(), 1);
        let publication = &h.conductor.publications[0];
        assert!(
            !publication.log.is_connected(),
            "the flag waits for the limit pass"
        );
        assert_eq!(publication.limit.get(), 0);
        // The limit pass: the limit by the window, then the flag.
        h.conductor.keep_publications(now);
        let publication = &h.conductor.publications[0];
        assert_eq!(publication.limit.get(), window);
        assert!(publication.log.is_connected());
    }

    /// The same order for a recording that connects a publication alone (ADR 0048):
    /// the commands pass admits it, and the flag waits for the limit pass of the same
    /// cycle, which raises the limit from the recording's start position first.
    #[test]
    fn a_recording_connects_a_publication_in_the_limit_pass() {
        let mut h = Harness::new("recording-connects-after-limit");
        h.conductor.term_length = cavalry_shm::log::layout::MIN_TERM_LENGTH;
        let now = h.now;
        h.conductor
            .add_publication(CLIENT, 2, STREAM, IPC_CHANNEL, true, now);
        h.cycle();
        // The commands pass admits the recording, which connects the publication alone
        // (`spec/log-buffer.md`, "The driver") from its start position until the
        // recorder's counter replaces it; nothing the publisher reads has moved.
        h.conductor.start_recording(3, STREAM, IPC_CHANNEL, now);
        assert_eq!(h.conductor.recordings[0].state, RecordingState::Starting);
        let publication = &h.conductor.publications[0];
        assert!(
            !publication.log.is_connected(),
            "the flag waits for the limit pass"
        );
        assert_eq!(publication.limit.get(), 0);
        let window = i64::from(publication.log.header().publication_window_length());
        h.conductor.keep_publications(now);
        let publication = &h.conductor.publications[0];
        assert_eq!(publication.limit.get(), window);
        assert!(publication.log.is_connected());
    }

    /// The flag holds while the publication stays connected (ADR 0048): a subscriber
    /// that reads up to the limit in a pass whose cleaning stops at the end of a term
    /// holds the raise back to the next pass, and still has its position, so the
    /// publisher at the limit reads `back pressured`, never `not connected`, and the
    /// next pass raises the limit with the flag still 1.
    #[test]
    fn the_connected_flag_holds_while_cleaning_holds_the_raise_back() {
        let mut h = Harness::new("connected-holds-while-cleaning");
        h.conductor.term_length = cavalry_shm::log::layout::MIN_TERM_LENGTH;
        let term = i64::from(cavalry_shm::log::layout::MIN_TERM_LENGTH);
        let now = h.now;
        h.conductor
            .add_publication(CLIENT, 2, STREAM, IPC_CHANNEL, true, now);
        h.conductor
            .add_subscription(CLIENT, 3, STREAM, IPC_CHANNEL, h.now);
        h.conductor.keep_publications(now);
        let window = i64::from(
            h.conductor.publications[0]
                .log
                .header()
                .publication_window_length(),
        );
        assert_eq!(window, term / 2);
        assert_eq!(h.conductor.publications[0].limit.get(), window);
        assert!(h.conductor.publications[0].log.is_connected());
        let mut appender = h.conductor.publications[0]
            .log
            .appender()
            .expect("an appender");
        let mut offered = 0;

        // The publisher fills the window, the subscriber reads it, and the limit runs a
        // window ahead: to a term. The subscriber then reads 40 KiB, inside the first
        // term, and the limit is 72 KiB, inside the second.
        offer_to(&mut appender, window, &mut offered);
        h.conductor.images[0].position.store_release(window);
        h.conductor.keep_publications(now);
        assert_eq!(h.conductor.publications[0].limit.get(), term);
        offer_to(&mut appender, term, &mut offered);
        let read = window + window / 4;
        h.conductor.images[0].position.store_release(read);
        h.conductor.keep_publications(now);
        let limit = read + window;
        assert_eq!(h.conductor.publications[0].limit.get(), limit);
        offer_to(&mut appender, limit, &mut offered);
        assert_eq!(appender.position().expect("the position"), limit);

        // The subscriber reads up to the limit. Cleaning behind it stops at the end of
        // the first term, so this pass holds the raise back; the subscriber still has
        // its position, and the publisher reads connected.
        h.conductor.images[0].position.store_release(limit);
        h.conductor.keep_publications(now);
        let publication = &h.conductor.publications[0];
        assert_eq!(
            publication.clean_position, term,
            "cleaning stops at the term's end"
        );
        assert_eq!(
            publication.limit.get(),
            limit,
            "the raise waits for cleaning"
        );
        assert!(
            publication.log.is_connected(),
            "a subscriber with a position keeps the publication connected"
        );
        assert!(matches!(
            appender.offer(&[0u8; 32], limit, 0),
            Err(ClaimError::BackPressured)
        ));

        // The next pass finishes cleaning and raises the limit; the flag stays 1.
        h.conductor.keep_publications(now);
        let publication = &h.conductor.publications[0];
        assert_eq!(publication.clean_position, limit);
        assert_eq!(publication.limit.get(), limit + window);
        assert!(publication.log.is_connected());
        drop(appender);
    }

    /// On a disconnect the flag falls in the commands pass, before the limit lowers
    /// (ADR 0048): removing the last subscription stores 0 where the subscriber goes
    /// and leaves the limit where it was, so a publisher a slow subscriber held at the
    /// limit is told `not connected` at once, never `back pressured`. The limit pass of
    /// the same cycle then lowers the limit to the largest position the subscriber
    /// reached, and the flag stays 0.
    #[test]
    fn a_disconnect_stores_the_flag_in_the_commands_pass_before_the_limit_lowers() {
        let mut h = Harness::new("disconnect-flag-before-limit");
        h.conductor.term_length = cavalry_shm::log::layout::MIN_TERM_LENGTH;
        let now = h.now;
        h.conductor
            .add_publication(CLIENT, 2, STREAM, IPC_CHANNEL, true, now);
        h.conductor
            .add_subscription(CLIENT, 3, STREAM, IPC_CHANNEL, h.now);
        h.conductor.keep_publications(now);
        let window = i64::from(
            h.conductor.publications[0]
                .log
                .header()
                .publication_window_length(),
        );
        let mut appender = h.conductor.publications[0]
            .log
            .appender()
            .expect("an appender");
        let mut offered = 0;

        // The publisher fills the window and the subscriber reads half of it; the limit
        // runs a window ahead of the subscriber, and the publisher fills up to it.
        offer_to(&mut appender, window, &mut offered);
        let read = window / 2;
        h.conductor.images[0].position.store_release(read);
        h.conductor.keep_publications(now);
        let limit = read + window;
        assert_eq!(h.conductor.publications[0].limit.get(), limit);
        offer_to(&mut appender, limit, &mut offered);
        assert_eq!(appender.position().expect("the position"), limit);
        assert!(h.conductor.publications[0].log.is_connected());
        assert!(matches!(
            appender.offer(&[0u8; 32], limit, 0),
            Err(ClaimError::BackPressured)
        ));

        // The commands pass: the last subscription goes, and the flag with it, before
        // the limit moves.
        h.conductor.remove_subscription(CLIENT, 4, 3, now);
        assert!(h.conductor.images.is_empty());
        let publication = &h.conductor.publications[0];
        assert!(
            !publication.log.is_connected(),
            "the 0 is stored where the last subscriber goes"
        );
        assert_eq!(
            publication.limit.get(),
            limit,
            "the limit waits for the limit pass"
        );
        assert!(matches!(
            appender.offer(&[0u8; 32], limit, 0),
            Err(ClaimError::NotConnected)
        ));
        // The limit pass lowers the limit to the subscriber's last position, below the
        // publisher's, and the flag stays 0.
        h.conductor.keep_publications(now);
        let publication = &h.conductor.publications[0];
        assert!(!publication.log.is_connected());
        assert_eq!(publication.limit.get(), read);
        assert!(matches!(
            appender.offer(&[0u8; 32], read, 0),
            Err(ClaimError::NotConnected)
        ));
        drop(appender);
    }

    /// The same for a recording that connected a publication alone (ADR 0048): the
    /// recorder's refusal to start it, read in the commands pass, stores 0 before the
    /// limit pass lowers the limit, as the recorder's stop does.
    #[test]
    fn a_recording_that_fails_to_start_stores_the_flag_before_the_limit_lowers() {
        let mut h = Harness::new("recording-fails-flag-before-limit");
        h.conductor.term_length = cavalry_shm::log::layout::MIN_TERM_LENGTH;
        let now = h.now;
        h.conductor
            .add_publication(CLIENT, 2, STREAM, IPC_CHANNEL, true, now);
        h.cycle();
        h.conductor.start_recording(3, STREAM, IPC_CHANNEL, now);
        h.conductor.keep_publications(now);
        let window = i64::from(
            h.conductor.publications[0]
                .log
                .header()
                .publication_window_length(),
        );
        assert_eq!(h.conductor.publications[0].limit.get(), window);
        assert!(h.conductor.publications[0].log.is_connected());

        h.conductor.on_start_failed(
            2,
            3,
            crate::recorder::StartFailure::Files("no space".to_owned()),
        );
        assert!(h.conductor.recordings.is_empty());
        let publication = &h.conductor.publications[0];
        assert!(
            !publication.log.is_connected(),
            "the 0 is stored where the recording goes"
        );
        assert_eq!(
            publication.limit.get(),
            window,
            "the limit waits for the limit pass"
        );
        h.conductor.keep_publications(now);
        let publication = &h.conductor.publications[0];
        assert!(!publication.log.is_connected());
        assert_eq!(publication.limit.get(), 0);
    }

    /// The recorder's stop of a recording that connected a publication alone stores 0
    /// where the conductor takes the stop, before the limit pass lowers the limit
    /// (ADR 0048). The entry is given the id the recorder's reply would have given it.
    #[test]
    fn a_recording_stop_stores_the_flag_before_the_limit_lowers() {
        let mut h = Harness::new("recording-stop-flag-before-limit");
        h.conductor.term_length = cavalry_shm::log::layout::MIN_TERM_LENGTH;
        let now = h.now;
        h.conductor
            .add_publication(CLIENT, 2, STREAM, IPC_CHANNEL, true, now);
        h.cycle();
        h.conductor.start_recording(3, STREAM, IPC_CHANNEL, now);
        h.conductor.recordings[0].recording_id = Some(0);
        h.conductor.keep_publications(now);
        let window = i64::from(
            h.conductor.publications[0]
                .log
                .header()
                .publication_window_length(),
        );
        assert_eq!(h.conductor.publications[0].limit.get(), window);
        assert!(h.conductor.publications[0].log.is_connected());

        h.conductor.on_stopped(0, 2, 0, None, now);
        assert!(h.conductor.recordings.is_empty());
        let publication = &h.conductor.publications[0];
        assert!(
            !publication.log.is_connected(),
            "the 0 is stored where the stop is taken"
        );
        assert_eq!(
            publication.limit.get(),
            window,
            "the limit waits for the limit pass"
        );
        h.conductor.keep_publications(now);
        let publication = &h.conductor.publications[0];
        assert!(!publication.log.is_connected());
        assert_eq!(publication.limit.get(), 0);
    }

    /// Offers `count` messages of 32 bytes, the first eight the message's number from
    /// `from`, with reserved value number × 7, cycling the harness while the limit
    /// waits for the recorder. Returns the position after.
    fn offer(
        h: &mut Harness,
        appender: &mut cavalry_shm::log::TermAppender,
        from: i64,
        count: i64,
    ) -> i64 {
        let mut message = [0u8; 32];
        let mut after = 0;
        for n in from..from + count {
            message[..8].copy_from_slice(&n.to_le_bytes());
            loop {
                let limit = h.conductor.publications[0].limit.get();
                match appender.offer(&message, limit, n * 7) {
                    Ok(position) => {
                        after = position;
                        break;
                    }
                    Err(ClaimError::AdminAction) => {}
                    Err(ClaimError::BackPressured) => h.cycle(),
                    Err(error) => panic!("{error}"),
                }
            }
        }
        after
    }

    /// Cycles until the recorder has written and synced everything: a second on.
    fn sync(h: &mut Harness) {
        for _ in 0..50 {
            h.cycle();
        }
        h.now += SECOND;
        h.cycle();
        h.cycle();
    }

    /// Stops recording 0 by command and cycles until the conductor forgot it.
    fn stop_recording(h: &mut Harness, correlation_id: i64) {
        h.conductor.stop_recording(correlation_id, 0);
        for _ in 0..100 {
            h.cycle();
            if h.conductor.recordings.is_empty() {
                return;
            }
        }
        panic!("the recording did not stop");
    }

    /// `start replay` with a subscription (registration `subscription`) on the replay
    /// stream when one is asked for; cycles until the replay is live or failed.
    fn start_replay(
        h: &mut Harness,
        correlation_id: i64,
        position: i64,
        length: i64,
        subscription: Option<i64>,
    ) {
        if let Some(registration) = subscription {
            h.conductor
                .add_subscription(CLIENT, registration, REPLAY_STREAM, IPC_CHANNEL, h.now);
        }
        h.conductor.start_replay(
            correlation_id,
            0,
            position,
            length,
            REPLAY_STREAM,
            IPC_CHANNEL,
        );
        for _ in 0..10 {
            h.cycle();
            let starting = h.conductor.replays.iter().any(|replay| {
                replay.replay_session_id == correlation_id && replay.state == ReplayState::Starting
            });
            if !starting {
                return;
            }
        }
        panic!("the replay is still starting");
    }

    /// Cycles until the replay with `replay_session_id` is gone from the registry.
    fn wait_replay_end(h: &mut Harness, replay_session_id: i64) {
        for _ in 0..10_000 {
            h.cycle();
            if !h
                .conductor
                .replays
                .iter()
                .any(|replay| replay.replay_session_id == replay_session_id)
            {
                return;
            }
        }
        panic!("the replay did not end");
    }

    /// Every committed frame of the replay log from `from`: (body, header).
    fn read_replay(
        h: &Harness,
        replay_session_id: i64,
        from: i64,
    ) -> Vec<(Vec<u8>, cavalry_proto::frame::DataFrameHeader)> {
        let log =
            LogBuffers::open(h.dir.path(), replay_session_id, false).expect("the replay log opens");
        let mut reader = log.reader(from).expect("a reader");
        let mut frames = Vec::new();
        loop {
            let before = reader.position();
            let poll = reader
                .poll(64, |body, header| frames.push((body.to_vec(), *header)))
                .expect("the poll reads");
            if poll.position == before {
                return frames;
            }
        }
    }

    /// The empty case (invariant 1): a recording stopped at its start position has no
    /// segment file, since the recorder removes the one it never wrote to; a replay of
    /// it from the start to the extent is `replay ready` and ends at once with `replay
    /// stopped` at the start position, reason 0, with nothing in its log and no error,
    /// where it was refused with code 12, storage space (ADR 0035, card 165).
    #[test]
    fn a_replay_of_an_empty_recording_ends_at_once_at_its_start_position() {
        let (mut h, appender) = recorded("replay-empty");
        stop_recording(&mut h, 4);
        assert!(
            !h.log_dir.join("0-0.rec").exists(),
            "the recorder removed the empty segment file"
        );
        let _ = events(&mut h);

        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, Some(5));
        let mut got = events(&mut h);
        wait_replay_end(&mut h, 6);
        for _ in 0..3 {
            h.cycle();
        }
        got.extend(events(&mut h));
        assert_eq!(errors(&got), vec![], "no error event");
        let (replay_session_id, recording_id, start, _, stream_id) =
            replay_ready(&got, 6).expect("replay ready, not an error");
        assert_eq!(
            (replay_session_id, recording_id, start, stream_id),
            (6, 0, 0, REPLAY_STREAM)
        );
        assert_eq!(replay_stopped(&got, 6), Some((0, 0)));
        assert_eq!(h.counter(REPLAY_STARTED), 1);
        assert_eq!(h.counter(REPLAY_START_FAILED), 0);
        assert_eq!(h.counter(REPLAY_STOPPED), 1);
        assert_eq!(h.counter(REPLAY_ENDED), 1);
        assert_eq!(h.counter(REPLAY_FAILED), 0);
        // The subscriber stood at the end from the start, so the publication is closed
        // and drained at once, and the subscriber hears the image go, as before. The
        // image was announced less than the counter reuse timeout ago and its position
        // stands where it joined, so the log and the position counter stay: a client
        // whose conductor has not read `available image` yet still finds both (ADR
        // 0078).
        let went = |got: &[(i32, Vec<u8>)]| {
            got.iter().any(|(type_id, message)| {
                matches!(
                    Event::parse(*type_id, message),
                    Ok(Event::UnavailableImage {
                        correlation_id: 6,
                        subscriber_registration_id: 5,
                        ..
                    })
                )
            })
        };
        let (position, announced) = h
            .conductor
            .images
            .iter()
            .find(|image| image.publication_id == 6)
            .map(|image| (image.position.id(), image.announced))
            .expect("the image is still listed");
        assert!(
            h.conductor.publications.iter().any(
                |publication| publication.registration_id == 6 && publication.closing.is_some()
            ),
            "the publication is closing, and waits"
        );
        assert!(h.dir.path().join("publications/6.logbuffer").exists());
        assert!(h.conductor.counters.counter(position).is_ok());
        assert!(went(&got), "the subscriber heard the image go");
        // One cycle short of the time, still there; once it has passed, the counter is
        // freed and the empty log is removed. A cycle adds a millisecond.
        h.now = announced + spec::nanos(spec::COUNTER_REUSE_TIMEOUT) - 2 * MS;
        h.cycle();
        assert!(h.dir.path().join("publications/6.logbuffer").exists());
        assert!(h.conductor.counters.counter(position).is_ok());
        h.cycle();
        assert!(
            h.conductor
                .publications
                .iter()
                .all(|publication| publication.registration_id != 6)
        );
        assert!(!h.dir.path().join("publications/6.logbuffer").exists());
        assert!(h.conductor.counters.counter(position).is_err());
        assert!(!went(&events(&mut h)), "and heard it once");
        drop(appender);
    }

    /// The full case (invariant 1): 100 messages recorded and stopped; a replay from
    /// the start to the extent into a subscription that joined first; `replay ready`
    /// and `available image` with the replay's publication at the start position;
    /// every frame read from the replay's log with the recording's position, flags and
    /// reserved value and the replay's session and stream ids; `replay stopped` with
    /// reason 0 at the stop position; the counters; and the image and the log going
    /// once the subscriber reached the end.
    #[test]
    fn a_replay_of_a_stopped_recording_delivers_every_frame_at_the_recordings_positions() {
        let (mut h, mut appender) = recorded("replay-full");
        let end = offer(&mut h, &mut appender, 0, 100);
        assert_eq!(end, 6400);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let _ = events(&mut h);

        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, Some(5));
        assert_eq!(h.conductor.replays.len(), 1);
        assert_eq!(h.conductor.replays[0].state, ReplayState::Live);
        let replay_position = h.conductor.replays[0].position.clone().expect("allocated");
        let got = events(&mut h);
        let (replay_session_id, recording_id, start, session_id, stream_id) =
            replay_ready(&got, 6).expect("replay ready");
        assert_eq!(
            (replay_session_id, recording_id, start, stream_id),
            (6, 0, 0, REPLAY_STREAM)
        );
        let publication = h
            .conductor
            .publications
            .iter()
            .find(|publication| publication.registration_id == 6)
            .expect("the replay's publication");
        assert_eq!(publication.session_id, session_id);
        assert!(publication.exclusive && publication.registrations.is_empty());
        let image =
            got.iter()
                .find_map(|(type_id, message)| match Event::parse(*type_id, message) {
                    Ok(Event::AvailableImage {
                        correlation_id: 6,
                        session_id,
                        stream_id,
                        subscriber_registration_id,
                        subscriber_position_counter_id,
                        log_file_name,
                        ..
                    }) => Some((
                        session_id,
                        stream_id,
                        subscriber_registration_id,
                        subscriber_position_counter_id,
                        String::from_utf8_lossy(log_file_name).into_owned(),
                    )),
                    _ => None,
                });
        let (image_session, image_stream, subscriber, position_counter, log_file_name) =
            image.expect("the subscription got an image of the replay");
        assert_eq!(
            (image_session, image_stream, subscriber),
            (session_id, REPLAY_STREAM, 5)
        );
        assert_eq!(log_file_name, "publications/6.logbuffer");
        assert_eq!(h.counter(REPLAY_STARTED), 1);
        let record = h
            .conductor
            .counters
            .counter(replay_position.id())
            .expect("the counter");
        assert_eq!(record.id(), replay_position.id());

        wait_replay_end(&mut h, 6);
        let got = events(&mut h);
        assert_eq!(replay_stopped(&got, 6), Some((6400, 0)));
        assert_eq!(h.counter(REPLAY_STOPPED), 1);
        assert_eq!(h.counter(REPLAY_ENDED), 1);
        assert_eq!(replay_position.load_acquire(), 6400);
        let frames = read_replay(&h, 6, 0);
        assert_eq!(frames.len(), 100);
        for (n, (body, header)) in frames.iter().enumerate() {
            assert_eq!(&body[..8], &(n as i64).to_le_bytes(), "frame {n}: body");
            assert_eq!(
                header.term_offset(),
                n as i32 * 64,
                "frame {n}: the recording's position"
            );
            assert_eq!(
                header.reserved_value(),
                n as i64 * 7,
                "frame {n}: reserved value"
            );
            assert_eq!(
                header.session_id(),
                session_id,
                "frame {n}: the replay's session"
            );
            assert_eq!(
                header.stream_id(),
                REPLAY_STREAM,
                "frame {n}: the replay stream"
            );
        }
        let publication = h
            .conductor
            .publications
            .iter()
            .find(|publication| publication.registration_id == 6)
            .expect("the publication lingers for its subscriber");
        assert_eq!(publication.closing.map(|(end, _)| end), Some(6400));
        assert_eq!(publication.log.end_of_stream_position(), 6400);

        // The subscriber reaches the end: unavailable image, the log removed.
        let image = h
            .conductor
            .images
            .iter()
            .find(|image| image.publication_id == 6)
            .expect("the image");
        assert_eq!(counter_id(&image.position), position_counter);
        image.position.store_release(6400);
        h.cycle();
        assert!(
            h.conductor
                .publications
                .iter()
                .all(|publication| publication.registration_id != 6)
        );
        assert!(!h.dir.path().join("publications/6.logbuffer").exists());
        let got = events(&mut h);
        let unavailable = got.iter().any(|(type_id, message)| {
            matches!(
                Event::parse(*type_id, message),
                Ok(Event::UnavailableImage {
                    correlation_id: 6,
                    subscriber_registration_id: 5,
                    ..
                })
            )
        });
        assert!(unavailable, "the subscriber heard the image go");
        drop(appender);
    }

    /// Pacing (invariants 2 and 3): a replay longer than the window moves only as far
    /// as the slowest subscriber's position plus the window, continues as the
    /// subscriber reads, rotates through the replay log's terms, and delivers every
    /// byte in order.
    #[test]
    fn a_replay_is_paced_by_its_subscriber_through_the_terms() {
        let (mut h, mut appender) = recorded("replay-paced");
        let count = 1600; // 100 KiB of 64-byte frames: a term and a half
        let end = offer(&mut h, &mut appender, 0, count);
        sync(&mut h);
        stop_recording(&mut h, 4);
        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, Some(5));
        let replay_position = h.conductor.replays[0].position.clone().expect("allocated");
        let window = i64::from(h.conductor.term_length / 2);
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(
            replay_position.load_acquire(),
            window,
            "the replay waits at the window with the subscriber at 0"
        );
        let log = LogBuffers::open(h.dir.path(), 6, false).expect("the replay log opens");
        let mut reader = log.reader(0).expect("a reader");
        let subscriber = h
            .conductor
            .images
            .iter()
            .find(|image| image.publication_id == 6)
            .map(|image| image.position.clone())
            .expect("the image");
        let mut next = 0i64;
        let mut delivered = 0;
        for _ in 0..10_000 {
            let poll = reader
                .poll(64, |body, header| {
                    assert_eq!(&body[..8], &next.to_le_bytes(), "frame {next} in order");
                    assert_eq!(header.reserved_value(), next * 7);
                    next += 1;
                    delivered += 1;
                })
                .expect("the poll reads");
            subscriber.store_release(poll.position);
            h.cycle();
            if h.conductor.replays.is_empty() && poll.position == end {
                break;
            }
        }
        assert_eq!(delivered, count, "every frame delivered in order");
        assert_eq!(reader.position(), end);
        assert_eq!(log.active_term_count(), 1, "the replay log rotated once");
        let got = events(&mut h);
        assert_eq!(replay_stopped(&got, 6), Some((end, 0)));
        drop(appender);
    }

    /// A replay with no subscriber does not move, and ends with reason 2 once the
    /// connect timeout has passed; one whose subscriber leaves ends the same way.
    #[test]
    fn a_replay_with_no_subscriber_waits_then_ends_with_reason_2() {
        let (mut h, mut appender) = recorded("replay-not-connected");
        offer(&mut h, &mut appender, 0, 10);
        sync(&mut h);
        stop_recording(&mut h, 4);
        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, None);
        let replay_position = h.conductor.replays[0].position.clone().expect("allocated");
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(
            replay_position.load_acquire(),
            0,
            "nothing moves without a subscriber"
        );
        assert_eq!(h.conductor.replays.len(), 1);
        h.now += spec::nanos(spec::REPLAY_CONNECT_TIMEOUT);
        wait_replay_end(&mut h, 6);
        let got = events(&mut h);
        assert_eq!(replay_stopped(&got, 6), Some((0, 2)));
        assert_eq!(h.counter(REPLAY_NOT_CONNECTED), 1);
        assert_eq!(h.counter(REPLAY_STOPPED), 1);

        // A subscriber that leaves: the clock starts then.
        start_replay(&mut h, 8, NULL_POSITION, NULL_LENGTH, Some(7));
        wait_replay_end(&mut h, 8);
        let got = events(&mut h);
        assert_eq!(
            replay_stopped(&got, 8),
            Some((640, 0)),
            "ended with its subscriber"
        );
        start_replay(&mut h, 10, 0, 0, Some(9));
        assert_eq!(
            h.conductor.replays.len(),
            1,
            "a zero-length replay is live until it is read"
        );
        wait_replay_end(&mut h, 10);
        let got = events(&mut h);
        assert_eq!(
            replay_stopped(&got, 10),
            Some((0, 0)),
            "nothing to play ends at once"
        );
        drop(appender);
    }

    /// The other way to reason 2: a paced replay whose last subscriber leaves. The
    /// clock starts when the image goes, and the replay ends where it stood, at the
    /// window the subscriber at 0 allowed.
    #[test]
    fn a_replay_whose_last_subscriber_leaves_ends_with_reason_2_where_it_stood() {
        let (mut h, mut appender) = recorded("replay-subscriber-leaves");
        offer(&mut h, &mut appender, 0, 1600);
        sync(&mut h);
        stop_recording(&mut h, 4);
        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, Some(5));
        let replay_position = h.conductor.replays[0].position.clone().expect("allocated");
        let window = i64::from(h.conductor.term_length / 2);
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(
            replay_position.load_acquire(),
            window,
            "paced at the window"
        );
        let _ = events(&mut h);
        h.conductor.remove_subscription(CLIENT, 7, 5, h.now);
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(
            h.conductor.replays.len(),
            1,
            "the clock starts when the image goes"
        );
        h.now += spec::nanos(spec::REPLAY_CONNECT_TIMEOUT);
        wait_replay_end(&mut h, 6);
        let got = events(&mut h);
        assert_eq!(replay_stopped(&got, 6), Some((window, 2)));
        assert_eq!(h.counter(REPLAY_NOT_CONNECTED), 1);
        drop(appender);
    }

    /// `stop replay` ends a paced replay at the position reached: `operation success`
    /// to the command, `replay stopped` with reason 1 to everyone; a second stop and a
    /// stop of an unknown id are `operation success` with nothing done.
    #[test]
    fn stop_replay_ends_a_replay_where_it_is_and_answers() {
        let (mut h, mut appender) = recorded("replay-stop");
        offer(&mut h, &mut appender, 0, 1600);
        sync(&mut h);
        stop_recording(&mut h, 4);
        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, Some(5));
        let window = i64::from(h.conductor.term_length / 2);
        for _ in 0..20 {
            h.cycle();
        }
        let _ = events(&mut h);
        h.conductor.stop_replay(9, 6);
        h.conductor.stop_replay(11, 6);
        wait_replay_end(&mut h, 6);
        let got = events(&mut h);
        assert_eq!(
            successes(&got),
            vec![11, 9],
            "the second stop at once, the first on the reply"
        );
        assert_eq!(replay_stopped(&got, 6), Some((window, 1)));
        assert_eq!(h.counter(REPLAY_STOPPED), 1);
        assert_eq!(h.counter(REPLAY_ENDED), 0);
        h.conductor.stop_replay(13, 6);
        h.conductor.stop_replay(15, 99);
        h.cycle();
        let got = events(&mut h);
        assert_eq!(successes(&got), vec![13, 15]);
        assert_eq!(replay_stopped(&got, 6), None, "nothing else happened");
        drop(appender);
    }

    /// Every error code of `start replay`, each counted: the channel (1), a length
    /// below −1 (7), a recording the catalog does not hold (21), a position that is not
    /// a multiple of 32, below the start position or above the extent (23), and a
    /// segment file that is gone (12).
    #[test]
    fn start_replay_errors_get_the_spec_codes_and_count() {
        let mut h = Harness::new("replay-errors");
        h.conductor.term_length = cavalry_shm::log::layout::MIN_TERM_LENGTH;
        let now = h.now;
        // The recording starts after 10 frames, so its start position is 640.
        h.conductor
            .add_publication(CLIENT, 2, STREAM, IPC_CHANNEL, true, now);
        h.conductor
            .add_subscription(CLIENT, 20, STREAM, IPC_CHANNEL, h.now);
        h.cycle();
        let mut appender = h.conductor.publications[0]
            .log
            .appender()
            .expect("an appender");
        offer(&mut h, &mut appender, 0, 10);
        h.conductor.start_recording(3, STREAM, IPC_CHANNEL, h.now);
        h.cycle();
        h.cycle();
        assert_eq!(h.conductor.recordings[0].start_position, 640);
        offer(&mut h, &mut appender, 10, 10);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let _ = events(&mut h);

        h.conductor.start_replay(
            30,
            0,
            NULL_POSITION,
            NULL_LENGTH,
            REPLAY_STREAM,
            b"cavalry:udp?endpoint=x",
        );
        h.conductor
            .start_replay(31, 0, NULL_POSITION, -2, REPLAY_STREAM, IPC_CHANNEL);
        h.conductor.start_replay(
            32,
            1,
            NULL_POSITION,
            NULL_LENGTH,
            REPLAY_STREAM,
            IPC_CHANNEL,
        );
        h.conductor
            .start_replay(33, 0, 700, NULL_LENGTH, REPLAY_STREAM, IPC_CHANNEL);
        h.conductor
            .start_replay(34, 0, 0, NULL_LENGTH, REPLAY_STREAM, IPC_CHANNEL);
        h.conductor
            .start_replay(35, 0, 1312, NULL_LENGTH, REPLAY_STREAM, IPC_CHANNEL);
        for _ in 0..5 {
            h.cycle();
        }
        let got = errors(&events(&mut h));
        let codes: Vec<(i64, i32)> = got.iter().map(|(id, code, _)| (*id, *code)).collect();
        assert_eq!(
            codes,
            vec![(30, 1), (31, 7), (32, 21), (33, 23), (34, 23), (35, 23)]
        );
        for (_, _, message) in &got {
            assert!(message.contains(ERRORS_DOC), "{message}");
        }
        assert!(got[3].2.contains("from 640 to 1280"), "{}", got[3].2);
        assert_eq!(h.counter(REPLAY_START_FAILED), 4);
        assert!(h.conductor.replays.is_empty());

        // The extent itself is a position: a zero-length replay from there.
        start_replay(&mut h, 36, 1280, NULL_LENGTH, Some(37));
        wait_replay_end(&mut h, 36);
        assert_eq!(replay_stopped(&events(&mut h), 36), Some((1280, 0)));

        // The segment file gone: code 12, counted, and the replayer forgot the start.
        std::fs::remove_file(h.log_dir.join("0-0.rec")).expect("the segment is removed");
        h.conductor.start_replay(
            38,
            0,
            NULL_POSITION,
            NULL_LENGTH,
            REPLAY_STREAM,
            IPC_CHANNEL,
        );
        for _ in 0..5 {
            h.cycle();
        }
        let got = errors(&events(&mut h));
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].0, got[0].1), (38, 12));
        assert!(got[0].2.contains("0-0.rec"), "{}", got[0].2);
        assert_eq!(h.counter(REPLAY_START_FAILED), 5);
        assert_eq!(h.replayer.live_replays(), 0);
        drop(appender);
    }

    /// The replays registry at capacity (invariant 3): the 65th start is code 10 and
    /// counted, with the replays' logs at the smallest term.
    #[test]
    fn the_replays_registry_is_bounded_and_says_so() {
        let (mut h, mut appender) = recorded("replays-registry");
        offer(&mut h, &mut appender, 0, 10);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let n = spec::MAX_REPLAYS as i64;
        for i in 0..n {
            h.conductor.start_replay(
                100 + i,
                0,
                NULL_POSITION,
                NULL_LENGTH,
                REPLAY_STREAM,
                IPC_CHANNEL,
            );
        }
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(h.conductor.replays.len(), spec::MAX_REPLAYS);
        assert!(
            h.conductor
                .replays
                .iter()
                .all(|replay| replay.state == ReplayState::Live)
        );
        assert_eq!(h.counter(REPLAY_STARTED), n);
        h.conductor.start_replay(
            200,
            0,
            NULL_POSITION,
            NULL_LENGTH,
            REPLAY_STREAM,
            IPC_CHANNEL,
        );
        let got = errors(&events(&mut h));
        let (id, code, message) = got.last().expect("an error event");
        assert_eq!((*id, *code), (200, 10), "{message}");
        assert!(message.contains("replays registry"), "{message}");
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), 1);
        assert_eq!(h.conductor.replays.len(), spec::MAX_REPLAYS);
        drop(appender);
    }

    /// The replay publications' bound (invariant 3; card 160): a replay leaves the
    /// registry when it ends, but its publication lingers until its subscriber reaches
    /// the end or the linger passes, so replays that start and end under the linger
    /// leave publications behind with no registry slot to count them. Twice the
    /// registry's capacity, 128, is the bound on them: the 129th `start replay` is
    /// refused with code 10 naming replay publications, counted in
    /// `driver.registry_full`, and admitted again once the lingers have passed. The
    /// `publications` list, built for both registries, does not grow for them.
    #[test]
    fn the_replay_publications_are_bounded_at_twice_the_registry() {
        let (mut h, mut appender) = recorded("replay-publications-bound");
        offer(&mut h, &mut appender, 0, 100);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let _ = events(&mut h);
        // One subscription that never polls: every replay's publication lingers.
        h.conductor
            .add_subscription(CLIENT, 5, REPLAY_STREAM, IPC_CHANNEL, h.now);
        let allocated = h.conductor.publications.capacity();
        let bound = spec::MAX_REPLAY_PUBLICATIONS as i64;
        for n in 0..bound {
            let id = 100 + n;
            start_replay(&mut h, id, NULL_POSITION, NULL_LENGTH, None);
            wait_replay_end(&mut h, id);
        }
        let _ = events(&mut h);
        let lingering = h
            .conductor
            .publications
            .iter()
            .filter(|publication| publication.replay)
            .count();
        assert_eq!(lingering, spec::MAX_REPLAY_PUBLICATIONS);
        assert_eq!(
            h.conductor.publications.capacity(),
            allocated,
            "the list did not grow"
        );
        assert!(h.conductor.replays.is_empty(), "every replay ended");
        let full_before = h.counter(DRIVER_REGISTRY_FULL);

        let refused = 100 + bound;
        h.conductor.start_replay(
            refused,
            0,
            NULL_POSITION,
            NULL_LENGTH,
            REPLAY_STREAM,
            IPC_CHANNEL,
        );
        h.cycle();
        let (code, message) = h.last_error(refused).expect("an error event");
        assert_eq!(code, 10);
        assert!(
            message.contains("replay publications registry holds 128"),
            "{message}"
        );
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), full_before + 1);
        assert!(h.conductor.replays.is_empty(), "nothing was registered");

        // The lingers pass: the publications go, and a replay starts again.
        h.now += 6 * SECOND;
        for _ in 0..5 {
            h.cycle();
        }
        assert!(
            h.conductor
                .publications
                .iter()
                .all(|publication| !publication.replay),
            "the lingering replay publications closed"
        );
        let admitted = refused + 1;
        start_replay(&mut h, admitted, NULL_POSITION, NULL_LENGTH, None);
        wait_replay_end(&mut h, admitted);
        assert_eq!(replay_stopped(&events(&mut h), admitted), Some((6400, 0)));
        drop(appender);
    }

    /// A replay that follows a live recording (invariant 1): it plays what is synced
    /// and no more, moves when the recorder syncs, and ends at the recording's stop
    /// position with reason 0 as soon as the conductor hands the replayer the stop the
    /// recorder reported, within a cycle and not within the second the catalog is read
    /// again (ADR 0036); a replay with length −1 of the same live recording ends at the
    /// extent it started with.
    #[test]
    fn a_replay_follows_a_live_recording_to_its_stop_and_plays_only_synced_bytes() {
        let (mut h, mut appender) = recorded("replay-follow");
        offer(&mut h, &mut appender, 0, 10);
        sync(&mut h);
        start_replay(&mut h, 6, NULL_POSITION, FOLLOW_LENGTH, Some(5));
        start_replay(&mut h, 8, NULL_POSITION, NULL_LENGTH, Some(7));
        let follow = h.conductor.replays[0].position.clone().expect("allocated");
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(follow.load_acquire(), 640, "the synced bytes are played");
        assert_eq!(read_replay(&h, 6, 0).len(), 10);
        wait_replay_end(&mut h, 8);
        assert_eq!(
            replay_stopped(&events(&mut h), 8),
            Some((640, 0)),
            "-1 ends at the extent at its start"
        );

        // More is written but not synced: the follow waits.
        offer(&mut h, &mut appender, 10, 10);
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(follow.load_acquire(), 640, "unsynced bytes are not played");
        assert_eq!(h.conductor.replays.len(), 1);
        h.now += SECOND;
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(follow.load_acquire(), 1280, "synced, played");
        assert_eq!(read_replay(&h, 6, 0).len(), 20);

        // The recording stops: the conductor hands the replayer the stop as it
        // forgets the recording, and the replay ends within the next cycles, long
        // before the catalog slot would be read again.
        stop_recording(&mut h, 4);
        for _ in 0..5 {
            h.cycle();
        }
        assert!(
            h.conductor.replays.is_empty(),
            "the replay ended on the stop the conductor handed over"
        );
        assert_eq!(replay_stopped(&events(&mut h), 6), Some((1280, 0)));
        assert_eq!(h.counter(REPLAY_ENDED), 2);
        drop(appender);
    }

    /// A recording stopped exactly at a segment boundary while a replay follows it
    /// (invariant 1; card 160): the recorder removes the segment file it had just
    /// rolled into, and a replay standing at the boundary that trusted its synced
    /// counter past the stop, a counter the conductor frees at the stop and another
    /// may hold after the reuse timeout, would roll into the removed file and fail with
    /// reason 3. The conductor hands the replayer the stop instead, and the replay ends
    /// with reason 0 at the boundary, nothing failed, within the stop's own cycles
    /// (ADR 0036).
    #[test]
    fn a_recording_stopped_at_a_segment_boundary_ends_its_live_replay_with_reason_0() {
        let (mut h, mut appender) = recorded("replay-boundary-stop");
        let segment = i64::from(cavalry_log::layout::SEGMENT_FILE_LENGTH);
        // 64-byte frames tile the 64 KiB terms, so the segment ends on a frame.
        let end = offer(&mut h, &mut appender, 0, segment / 64);
        assert_eq!(end, segment, "the recording stands at the segment boundary");
        sync(&mut h);
        assert!(
            h.log_dir
                .join(cavalry_log::layout::segment_file_name(0, segment))
                .exists(),
            "the recorder rolled into the second segment file"
        );

        start_replay(&mut h, 6, NULL_POSITION, FOLLOW_LENGTH, Some(5));
        let subscriber = h
            .conductor
            .images
            .iter()
            .find(|image| image.publication_id == 6)
            .map(|image| image.position.clone())
            .expect("the image");
        let replay_position = h.conductor.replays[0].position.clone().expect("allocated");
        for _ in 0..100_000 {
            subscriber.store_release(replay_position.load_acquire());
            h.cycle();
            if replay_position.load_acquire() == end {
                break;
            }
        }
        assert_eq!(
            replay_position.load_acquire(),
            end,
            "the replay caught up to the boundary"
        );
        assert_eq!(
            h.conductor.replays.len(),
            1,
            "and follows the live recording there"
        );

        stop_recording(&mut h, 4);
        assert!(
            !h.log_dir
                .join(cavalry_log::layout::segment_file_name(0, segment))
                .exists(),
            "the stop removed the empty second segment file"
        );
        for _ in 0..5 {
            h.cycle();
        }
        assert!(
            h.conductor.replays.is_empty(),
            "the replay ended on the stop"
        );
        assert_eq!(replay_stopped(&events(&mut h), 6), Some((end, 0)));
        assert_eq!(h.counter(REPLAY_FAILED), 0);
        assert_eq!(h.counter(LOG_BUFFER_REJECTED_FRAME), 0);
        assert_eq!(h.counter(REPLAY_ENDED), 1);
        drop(appender);
    }

    /// A frame in the segment file that fails a check ends the replay at the frame
    /// before it with reason 3, counted in `replay.failed` and
    /// `log_buffer.rejected_frame`; the frames before it were delivered.
    #[test]
    fn a_damaged_frame_ends_the_replay_with_reason_3_at_the_frame_before_it() {
        let (mut h, mut appender) = recorded("replay-damaged");
        offer(&mut h, &mut appender, 0, 10);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let segment = OpenOptions::new()
            .write(true)
            .open(h.log_dir.join("0-0.rec"))
            .expect("the segment opens");
        segment
            .write_all_at(&[0xFF], 3 * 64 + 4)
            .expect("the fourth frame's version byte is poisoned");
        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, Some(5));
        wait_replay_end(&mut h, 6);
        assert_eq!(replay_stopped(&events(&mut h), 6), Some((192, 3)));
        assert_eq!(h.counter(REPLAY_FAILED), 1);
        assert_eq!(h.counter(LOG_BUFFER_REJECTED_FRAME), 1);
        assert_eq!(read_replay(&h, 6, 0).len(), 3);
        drop(appender);
    }

    /// A recorded frame whose length runs past the recording's extent (invariants 1
    /// and 4; spec, "start replay"): the header passes `spec/frame.md`'s checks, since
    /// the frame fits its term, but the block the replayer reads ends at the extent,
    /// so the writer has no whole frame and writes nothing. That is the `length`
    /// reason, damage, not a block to read again for ever: `replay stopped` reason 3 at
    /// the frame before it, `replay.failed` and `log_buffer.rejected_frame` at 1, and
    /// the frames before it delivered.
    #[test]
    fn a_frame_length_past_the_extent_ends_the_replay_with_reason_3_at_the_frame_before_it() {
        let (mut h, mut appender) = recorded("replay-length-past-extent");
        offer(&mut h, &mut appender, 0, 10);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let segment = OpenOptions::new()
            .write(true)
            .open(h.log_dir.join("0-0.rec"))
            .expect("the segment opens");
        // The tenth frame, at 576, says it is 128 bytes long: 64 past the stop at 640.
        segment
            .write_all_at(&128i32.to_le_bytes(), 9 * 64)
            .expect("the tenth frame's length is poisoned");
        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, Some(5));
        let replay_position = h.conductor.replays[0].position.clone().expect("allocated");
        wait_replay_end(&mut h, 6);
        assert_eq!(replay_stopped(&events(&mut h), 6), Some((576, 3)));
        assert_eq!(replay_position.load_acquire(), 576);
        assert_eq!(h.counter(REPLAY_FAILED), 1);
        assert_eq!(h.counter(LOG_BUFFER_REJECTED_FRAME), 1);
        assert_eq!(read_replay(&h, 6, 0).len(), 9, "the frames before it");
        drop(appender);
    }

    /// A recorded frame whose length is over the MTU (invariants 1 and 4): 65472 bytes
    /// at 64 in a 64 KiB term, which is exactly the term's remaining bytes, so the
    /// header passes `spec/frame.md`'s checks, while the recording is a term and a
    /// half, so the extent does not cut the block; the publication's limit does, at
    /// the window plus an MTU with the subscriber at 0, the one bound that may end a
    /// block inside a frame and does so only for a frame over the MTU. The writer
    /// writes nothing of it and the replayer calls it damage: `replay stopped` reason
    /// 3 at 64, `replay.failed` and `log_buffer.rejected_frame` at 1, the frame before
    /// it delivered.
    #[test]
    fn a_frame_length_over_the_mtu_ends_the_replay_with_reason_3_at_the_frame_before_it() {
        let (mut h, mut appender) = recorded("replay-length-over-mtu");
        offer(&mut h, &mut appender, 0, 1600);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let term = i64::from(h.conductor.term_length);
        let window = term / 2;
        let mtu = i64::from(spec::IPC_MTU_LENGTH);
        let poisoned = i32::try_from(term - 64).expect("fits");
        assert!(i64::from(poisoned) > mtu, "over the MTU");
        assert!(
            i64::from(poisoned) > window + mtu - 64,
            "longer than the block"
        );
        let segment = OpenOptions::new()
            .write(true)
            .open(h.log_dir.join("0-0.rec"))
            .expect("the segment opens");
        segment
            .write_all_at(&poisoned.to_le_bytes(), 64)
            .expect("the second frame's length is poisoned");
        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, Some(5));
        let replay_position = h.conductor.replays[0].position.clone().expect("allocated");
        wait_replay_end(&mut h, 6);
        assert_eq!(replay_stopped(&events(&mut h), 6), Some((64, 3)));
        assert_eq!(replay_position.load_acquire(), 64);
        assert_eq!(h.counter(REPLAY_FAILED), 1);
        assert_eq!(h.counter(LOG_BUFFER_REJECTED_FRAME), 1);
        assert_eq!(read_replay(&h, 6, 0).len(), 1, "the frame before it");
        drop(appender);
    }

    /// A replay across a segment roll (invariant 1): a recording of a segment and a
    /// bit, replayed in full to a subscriber that keeps up, delivers every byte in
    /// order across the roll into the second segment file.
    #[test]
    fn a_replay_rolls_into_the_next_segment_file() {
        let (mut h, mut appender) = recorded("replay-roll");
        let segment = i64::from(cavalry_log::layout::SEGMENT_FILE_LENGTH);
        // 1000-byte bodies: 62 frames and a padding frame per 64 KiB term.
        let mut message = [0u8; 1000];
        let mut n = 0i64;
        let mut position = 0;
        while position < segment + 4096 {
            message[..8].copy_from_slice(&n.to_le_bytes());
            let limit = h.conductor.publications[0].limit.get();
            match appender.offer(&message, limit, 0) {
                Ok(after) => {
                    position = after;
                    n += 1;
                }
                Err(ClaimError::AdminAction) => {}
                Err(ClaimError::BackPressured) => h.cycle(),
                Err(error) => panic!("{error}"),
            }
        }
        let count = n;
        let end = position;
        sync(&mut h);
        stop_recording(&mut h, 4);
        assert!(
            h.log_dir
                .join(cavalry_log::layout::segment_file_name(0, segment))
                .exists()
        );

        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, Some(5));
        let log = LogBuffers::open(h.dir.path(), 6, false).expect("the replay log opens");
        let mut reader = log.reader(0).expect("a reader");
        let subscriber = h
            .conductor
            .images
            .iter()
            .find(|image| image.publication_id == 6)
            .map(|image| image.position.clone())
            .expect("the image");
        let mut next = 0i64;
        for _ in 0..1_000_000 {
            let poll = reader
                .poll(1024, |body, _| {
                    assert_eq!(&body[..8], &next.to_le_bytes(), "frame {next} in order");
                    assert_eq!(body.len(), 1000);
                    next += 1;
                })
                .expect("the poll reads");
            subscriber.store_release(poll.position);
            h.cycle();
            if h.conductor.replays.is_empty() && poll.position == end {
                break;
            }
        }
        assert_eq!(next, count, "every frame delivered across the roll");
        assert_eq!(reader.position(), end);
        assert_eq!(replay_stopped(&events(&mut h), 6), Some((end, 0)));
        drop(appender);
    }

    /// An end position inside a frame (invariant 1; spec, "start replay"): the frame
    /// that begins below it is played whole and the replay ends at the first frame
    /// boundary at or after it, `replay stopped` reason 0 there, so a length that cuts
    /// a frame moves and ends instead of waiting for ever at the cut. Over 64-byte
    /// frames a length of 100 from 0 plays two frames and ends at 128, a length of 1
    /// from 64 plays the frame at 64 and ends at 128, and a length of 32 from 0 ends
    /// at 64.
    #[test]
    fn an_end_position_inside_a_frame_plays_that_frame_whole_and_ends_at_its_end() {
        let (mut h, mut appender) = recorded("replay-end-inside-frame");
        offer(&mut h, &mut appender, 0, 10);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let _ = events(&mut h);

        start_replay(&mut h, 6, NULL_POSITION, 100, Some(5));
        let replay_position = h.conductor.replays[0].position.clone().expect("allocated");
        wait_replay_end(&mut h, 6);
        assert_eq!(replay_stopped(&events(&mut h), 6), Some((128, 0)));
        assert_eq!(replay_position.load_acquire(), 128);
        let frames = read_replay(&h, 6, 0);
        assert_eq!(
            frames.len(),
            2,
            "the frame at 64 begins below 100 and is played whole"
        );
        assert_eq!(&frames[1].0[..8], &1i64.to_le_bytes());

        start_replay(&mut h, 8, 64, 1, Some(7));
        wait_replay_end(&mut h, 8);
        assert_eq!(replay_stopped(&events(&mut h), 8), Some((128, 0)));
        assert_eq!(read_replay(&h, 8, 64).len(), 1);

        start_replay(&mut h, 10, NULL_POSITION, 32, Some(9));
        wait_replay_end(&mut h, 10);
        assert_eq!(replay_stopped(&events(&mut h), 10), Some((64, 0)));
        assert_eq!(h.counter(REPLAY_ENDED), 3);
        assert_eq!(h.counter(REPLAY_STOPPED), 3);
        drop(appender);
    }

    // ---- replay over a cavalry:udp channel: the sender's path (ADR 0043) ---------------

    /// `start replay` with a `cavalry:udp` channel, cycled until the replay is live or
    /// failed, as `start_replay` does for `cavalry:ipc`.
    fn start_udp_replay(h: &mut Harness, correlation_id: i64, position: i64, channel: &[u8]) {
        h.conductor.start_replay(
            correlation_id,
            0,
            position,
            NULL_LENGTH,
            REPLAY_STREAM,
            channel,
        );
        for _ in 0..10 {
            h.cycle();
            let starting = h.conductor.replays.iter().any(|replay| {
                replay.replay_session_id == correlation_id && replay.state == ReplayState::Starting
            });
            if !starting {
                return;
            }
        }
        panic!("the replay is still starting");
    }

    /// One cycle of the sender, the conductor and the replayer on the real clock: the
    /// sender's timers run on it, and the replayer paces against the limit alone.
    fn cycle_with_sender(h: &mut Harness) {
        let now = now_nanos();
        h.sender.duty_cycle(now);
        h.conductor.duty_cycle(now);
        h.replayer.duty_cycle(now);
        h.conductor.duty_cycle(now);
    }

    /// The full cases first (invariant 3): a replay over `cavalry:udp` refused at the
    /// sender's bounds leaves nothing of itself behind. The send endpoints registry
    /// full of a client's publications: a replay on a 65th channel fails with code 10
    /// naming the registry when the replayer answers ready, with no endpoint, no
    /// publication, no log and no counter left, the replay gone and `replay.started`
    /// unmoved. The sender's request queue full: a replay on an open channel, which
    /// shares the endpoint and needs the publication's request alone, fails with code
    /// 10 naming the queue the same way. Before either, a channel whose interface is
    /// not this host's: the socket is refused with code 4 at the same point, no
    /// endpoint opened. And once the sender has drained its queue, a replay on the open
    /// channel goes live and shares its endpoint.
    #[test]
    fn a_replay_over_udp_refused_at_the_senders_bounds_leaves_nothing_behind() {
        let (mut h, mut appender) = recorded("replay-udp-bounds");
        offer(&mut h, &mut appender, 0, 10);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let _ = events(&mut h);
        let allocated = h.counter(COUNTERS_ALLOCATED);

        // The socket refused: code 4 from the replayer's ready reply, nothing left.
        start_udp_replay(
            &mut h,
            601,
            NULL_POSITION,
            b"cavalry:udp?endpoint=127.0.0.1:40998|interface=203.0.113.9",
        );
        let got = events(&mut h);
        let error = errors(&got)
            .into_iter()
            .find(|(id, _, _)| *id == 601)
            .expect("the replay was refused");
        assert_eq!(error.1, 4, "{}", error.2);
        assert!(error.2.contains("203.0.113.9"), "{}", error.2);
        assert!(replay_ready(&got, 601).is_none());
        assert!(h.conductor.replays.is_empty(), "the replay is gone");
        assert!(h.conductor.send_endpoints.is_empty(), "no endpoint");
        assert_eq!(h.conductor.publications.len(), 1, "no publication");
        assert!(!h.dir.path().join("publications/601.logbuffer").exists());
        assert_eq!(h.counter(COUNTERS_ALLOCATED), allocated, "no counter");
        assert_eq!(h.counter(CHANNEL_OPENED), 0);
        assert_eq!(h.counter(REPLAY_STARTED), 0);

        let now = h.now;
        for i in 0..spec::MAX_SEND_ENDPOINTS {
            let correlation_id = 10 + i as i64;
            let channel = format!("cavalry:udp?endpoint=127.0.0.1:{}", 40_200 + i);
            h.conductor.add_publication(
                CLIENT,
                correlation_id,
                STREAM,
                channel.as_bytes(),
                false,
                now,
            );
            assert_eq!(h.last_error(correlation_id), None, "endpoint {i}");
        }
        assert_eq!(h.conductor.send_endpoints.len(), spec::MAX_SEND_ENDPOINTS);
        let publications = h.conductor.publications.len();
        let _ = events(&mut h);
        let allocated = h.counter(COUNTERS_ALLOCATED);
        let registry_full = h.counter(DRIVER_REGISTRY_FULL);

        // The registry full: code 10 naming it, from the replayer's ready reply.
        start_udp_replay(
            &mut h,
            600,
            NULL_POSITION,
            b"cavalry:udp?endpoint=127.0.0.1:40999",
        );
        let got = events(&mut h);
        let error = errors(&got)
            .into_iter()
            .find(|(id, _, _)| *id == 600)
            .expect("the replay was refused");
        assert_eq!(error.1, 10, "{}", error.2);
        assert!(error.2.contains("send endpoints"), "{}", error.2);
        assert!(replay_ready(&got, 600).is_none());
        assert!(h.conductor.replays.is_empty(), "the replay is gone");
        assert_eq!(h.conductor.send_endpoints.len(), spec::MAX_SEND_ENDPOINTS);
        assert_eq!(
            h.conductor.publications.len(),
            publications,
            "no publication"
        );
        assert!(!h.dir.path().join("publications/600.logbuffer").exists());
        assert_eq!(h.counter(COUNTERS_ALLOCATED), allocated, "no counter");
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), registry_full + 1);
        assert_eq!(h.counter(REPLAY_STARTED), 0);

        // The sender's request queue full: a replay on the open channel shares its
        // endpoint and is refused for the queue, nothing left.
        h.now = now_nanos();
        for _ in 0..10 {
            h.sender.duty_cycle(h.now);
            h.conductor.duty_cycle(h.now);
        }
        assert!(h.conductor.sender.requests.is_empty());
        let mut filler = 0;
        while h
            .conductor
            .sender
            .requests
            .offer(sender::Request::RemoveEndpoint {
                registration_id: 1_000_000 + filler,
            })
            .is_ok()
        {
            filler += 1;
        }
        assert_eq!(filler, spec::SENDER_QUEUE_CAPACITY as i64);
        let allocated = h.counter(COUNTERS_ALLOCATED);
        let registry_full = h.counter(DRIVER_REGISTRY_FULL);
        start_udp_replay(
            &mut h,
            602,
            NULL_POSITION,
            b"cavalry:udp?endpoint=127.0.0.1:40200",
        );
        let got = events(&mut h);
        let error = errors(&got)
            .into_iter()
            .find(|(id, _, _)| *id == 602)
            .expect("the replay was refused");
        assert_eq!(error.1, 10, "{}", error.2);
        assert!(error.2.contains("request queue"), "{}", error.2);
        assert!(h.conductor.replays.is_empty());
        assert_eq!(h.conductor.publications.len(), publications);
        assert_eq!(
            h.conductor.send_endpoints[0].publications.get(),
            1,
            "not shared"
        );
        assert!(!h.dir.path().join("publications/602.logbuffer").exists());
        assert_eq!(h.counter(COUNTERS_ALLOCATED), allocated);
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), registry_full + 1);
        assert_eq!(h.counter(REPLAY_STARTED), 0);

        // The sender drains the queue; a replay on the open channel then goes live,
        // shares the endpoint, and its five counters are allocated.
        for _ in 0..10_000 {
            h.now += MS;
            h.sender.duty_cycle(h.now);
            h.conductor.duty_cycle(h.now);
            if h.conductor.sender.requests.is_empty() {
                break;
            }
        }
        assert!(h.conductor.sender.requests.is_empty());
        let shared = h
            .conductor
            .send_endpoints
            .iter()
            .position(|endpoint| endpoint.target.address.port() == 40_200)
            .expect("the first endpoint");
        start_udp_replay(
            &mut h,
            603,
            NULL_POSITION,
            b"cavalry:udp?endpoint=127.0.0.1:40200",
        );
        let got = events(&mut h);
        assert_eq!(errors(&got), Vec::new());
        assert!(replay_ready(&got, 603).is_some());
        assert_eq!(h.conductor.replays.len(), 1);
        assert_eq!(h.conductor.replays[0].state, ReplayState::Live);
        assert_eq!(
            h.conductor.send_endpoints.len(),
            spec::MAX_SEND_ENDPOINTS,
            "shared"
        );
        assert_eq!(h.conductor.send_endpoints[shared].publications.get(), 2);
        assert_eq!(
            h.counter(COUNTERS_ALLOCATED),
            allocated + 6,
            "the publication's two, replay.position and the sender's three"
        );
        assert_eq!(h.counter(REPLAY_STARTED), 1);
        let publication = h
            .conductor
            .publications
            .iter()
            .find(|publication| publication.registration_id == 603)
            .expect("the replay's publication");
        assert_eq!(
            publication.udp.as_ref().map(|udp| udp.endpoint_id),
            Some(h.conductor.send_endpoints[shared].registration_id)
        );
        drop(appender);
    }

    /// The counters file at its capacity (invariant 3; ADR 0043), the one bound a
    /// replay over `cavalry:udp` meets after its endpoint is opened: with seven records
    /// free, the endpoint takes its five and the publication's third is refused, so the
    /// replay fails with code 10 when the replayer answers ready and `undo_replay_ready`
    /// abandons the new endpoint with the log it made, the endpoint's five and the
    /// publication's two freed, the socket dropped, nothing handed to the sender, the
    /// log file gone, `channel.opened` unmoved, the replay gone, `driver.registry_full`
    /// one more and `replay.started` unmoved. Then, the records returned, a replay on
    /// the same channel opens a fresh endpoint and goes live.
    #[test]
    fn a_replay_over_udp_the_counters_file_refuses_abandons_its_new_endpoint() {
        let (mut h, mut appender) = recorded("replay-udp-counters-full");
        offer(&mut h, &mut appender, 0, 10);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let _ = events(&mut h);

        // Every free record taken, then seven returned, reusable at once: five for
        // the endpoint and two of the publication's five.
        let mut filler: Vec<Counter> = Vec::new();
        while let Ok(counter) = h
            .conductor
            .counters
            .allocate(RECORDING_POSITION, &[0; 8], "filler")
        {
            filler.push(counter);
        }
        assert!(filler.len() > 8, "the counters file had free records");
        for counter in filler.split_off(filler.len() - 8) {
            h.conductor
                .counters
                .free(counter.id(), 0)
                .expect("the filler is freed");
        }
        let allocated = h.counter(COUNTERS_ALLOCATED);
        let freed = h.counter(COUNTERS_FREED);
        let registry_full = h.counter(DRIVER_REGISTRY_FULL);

        // Code 10 from the replayer's ready reply, the endpoint abandoned with its
        // six records and the publication's two, nothing left.
        start_udp_replay(
            &mut h,
            604,
            NULL_POSITION,
            b"cavalry:udp?endpoint=127.0.0.1:40997",
        );
        let got = events(&mut h);
        let error = errors(&got)
            .into_iter()
            .find(|(id, _, _)| *id == 604)
            .expect("the replay was refused");
        assert_eq!(error.1, 10, "{}", error.2);
        assert!(error.2.contains("records is in use"), "{}", error.2);
        assert!(replay_ready(&got, 604).is_none());
        assert!(h.conductor.replays.is_empty(), "the replay is gone");
        assert!(
            h.conductor.send_endpoints.is_empty(),
            "the new endpoint is abandoned"
        );
        assert!(
            h.conductor.sender.requests.is_empty(),
            "nothing is handed to the sender"
        );
        assert_eq!(h.conductor.publications.len(), 1, "no publication");
        assert!(!h.dir.path().join("publications/604.logbuffer").exists());
        assert_eq!(
            h.counter(COUNTERS_ALLOCATED),
            allocated,
            "every record the command took is back"
        );
        assert_eq!(
            h.counter(COUNTERS_FREED),
            freed + 8,
            "the endpoint's six and the publication's two"
        );
        assert_eq!(h.counter(CHANNEL_OPENED), 0);
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), registry_full + 1);
        assert_eq!(h.counter(REPLAY_STARTED), 0);

        // The records returned: a replay on the same channel opens a fresh endpoint,
        // its five and the publication's five allocated, and goes live.
        for counter in filler {
            h.conductor
                .counters
                .free(counter.id(), 0)
                .expect("the filler is freed");
        }
        let allocated = h.counter(COUNTERS_ALLOCATED);
        start_udp_replay(
            &mut h,
            605,
            NULL_POSITION,
            b"cavalry:udp?endpoint=127.0.0.1:40997",
        );
        let got = events(&mut h);
        assert_eq!(errors(&got), Vec::new());
        assert!(replay_ready(&got, 605).is_some());
        assert_eq!(h.conductor.replays.len(), 1);
        assert_eq!(h.conductor.replays[0].state, ReplayState::Live);
        assert_eq!(h.conductor.send_endpoints.len(), 1);
        assert_eq!(h.conductor.send_endpoints[0].registration_id, 605);
        assert_eq!(h.conductor.send_endpoints[0].publications.get(), 1);
        assert_eq!(h.counter(COUNTERS_ALLOCATED), allocated + 12, "six and six");
        assert_eq!(h.counter(CHANNEL_OPENED), 1);
        assert_eq!(h.counter(REPLAY_STARTED), 1);
        drop(appender);
    }

    /// The happy path (invariant 1; ADR 0043): a replay over `cavalry:udp` from a
    /// position inside the recording. The conductor opens the channel's endpoint with
    /// its five counters and the publication's five, keyed by the replay session id,
    /// answers `replay ready`, gives a `cavalry:ipc` subscription on the replay stream
    /// no image, and holds the limit at the start position; the sender's setup frame
    /// carries the recording's term and the start position's term offset; a status
    /// message whose offset the replay's 64 KiB term does not hold is rejected where
    /// the 16 MiB UDP term would have passed it; the status message that connects the
    /// publication lets the replayer move, and every frame reaches the receiver by hand
    /// in order at the recording's positions with the replay's session id; the replay
    /// ends at the recording's stop, the publication lingers with the sender, a
    /// heartbeat carries EOS at the end position, and the receiver's end-of-stream
    /// status message closes the publication and its endpoint, every record freed.
    #[test]
    fn a_replay_over_a_udp_channel_is_sent_by_the_sender_in_order_and_lingers() {
        let (mut h, mut appender) = recorded("replay-udp");
        let end = offer(&mut h, &mut appender, 0, 100);
        assert_eq!(end, 6400);
        sync(&mut h);
        stop_recording(&mut h, 4);
        h.conductor
            .add_subscription(CLIENT, 5, REPLAY_STREAM, IPC_CHANNEL, h.now);
        let _ = events(&mut h);
        let term = cavalry_shm::log::layout::MIN_TERM_LENGTH;
        let mut receiver = TestReceiver::bind();
        let allocated = h.counter(COUNTERS_ALLOCATED);
        start_udp_replay(&mut h, 6, 3200, &receiver.channel());
        assert_eq!(h.conductor.replays.len(), 1);
        assert_eq!(h.conductor.replays[0].state, ReplayState::Live);
        assert!(h.conductor.replays[0].udp.is_some());
        let replay_position = h.conductor.replays[0].position.clone().expect("allocated");
        let got = events(&mut h);
        assert_eq!(errors(&got), Vec::new());
        let (replay_session_id, recording_id, start, session_id, stream_id) =
            replay_ready(&got, 6).expect("replay ready");
        assert_eq!(
            (replay_session_id, recording_id, start, stream_id),
            (6, 0, 3200, REPLAY_STREAM)
        );
        let image = got.iter().any(|(type_id, message)| {
            matches!(
                Event::parse(*type_id, message),
                Ok(Event::AvailableImage { .. })
            )
        });
        assert!(
            !image,
            "a cavalry:ipc subscription gets no image of a UDP replay"
        );
        assert_eq!(h.conductor.send_endpoints.len(), 1);
        assert_eq!(h.conductor.send_endpoints[0].registration_id, 6);
        assert_eq!(h.conductor.send_endpoints[0].state, ChannelState::Active);
        assert_eq!(h.conductor.send_endpoints[0].status.get(), 1);
        assert_eq!(h.conductor.send_endpoints[0].publications.get(), 1);
        assert_eq!(h.counter(CHANNEL_OPENED), 1);
        assert_eq!(h.counter(COUNTERS_ALLOCATED), allocated + 12, "six and six");
        assert_eq!(h.counter(REPLAY_STARTED), 1);
        {
            let publication = h
                .conductor
                .publications
                .iter()
                .find(|publication| publication.registration_id == 6)
                .expect("the replay's publication");
            let udp = publication.udp.as_ref().expect("over UDP");
            assert_eq!(udp.endpoint_id, 6);
            assert!(!udp.connected);
            assert_eq!(
                publication.limit.get(),
                3200,
                "the limit stands at the start"
            );
            // The receivers gauge's label names the endpoint and the replay channel's
            // strategy, as a client's publication's does.
            let label = cavalry_shm::counters::CountersReader::open(h.dir.path())
                .expect("the counters file opens")
                .record(udp.receivers_id)
                .expect("a record")
                .expect("allocated")
                .label()
                .to_string();
            assert_eq!(
                label,
                format!("sender.flow_control_receivers 6 {session_id} {REPLAY_STREAM} 6 fc=max")
            );
            assert_eq!(publication.log.header().term_length(), term);
        }

        // The sender's first cycle: the setup frame carries the replay's session id, the
        // recording's term and the start position's term offset.
        h.sender.duty_cycle(now_nanos());
        let setup = match receiver.next_control() {
            Some(ControlFrame::Setup(setup)) => setup,
            other => panic!("expected a setup frame, got {other:?}"),
        };
        assert_eq!(setup.session_id, session_id);
        assert_eq!(setup.stream_id, REPLAY_STREAM);
        assert_eq!(setup.term_length as u32, term);
        assert_eq!(setup.term_offset, 3200);
        assert_eq!(h.sender.positions_of(6), Some((3200, 0)));

        // A status message whose offset the replay's term does not hold: rejected.
        let status = |offset: i32, flags: u8| StatusMessage {
            flags,
            session_id,
            stream_id: REPLAY_STREAM,
            consumption_term_id: setup.active_term_id,
            consumption_term_offset: offset,
            receiver_window_length: 131072,
            receiver_id: 9,
            group_tag: None,
        };
        receiver.send_status_message(&status(term as i32, 0));
        let deadline = Instant::now() + Duration::from_secs(5);
        while h.counter(SENDER_STATUS_MESSAGES_REJECTED) == 0 {
            cycle_with_sender(&mut h);
            assert!(
                Instant::now() < deadline,
                "the status message was not rejected"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(h.counter(SENDER_STATUS_MESSAGES_RECEIVED), 1);
        assert_eq!(h.counter(PUBLICATION_CONNECTED), 0, "not connected by it");
        assert_eq!(
            h.sender.positions_of(6),
            Some((3200, 0)),
            "the limit stands"
        );

        // The status message at the start position connects the publication; the
        // replayer moves, and the frames arrive in order at the recording's positions.
        receiver.send_status_message(&status(3200, 0));
        receiver.set_read_timeout(Duration::from_millis(10));
        let mut frames: Vec<(DataFrameHeader, Vec<u8>)> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while frames.len() < 50 {
            cycle_with_sender(&mut h);
            if let Some(batch) = receiver.next_data(term) {
                frames.extend(batch);
            }
            assert!(
                Instant::now() < deadline,
                "{} of 50 frames arrived",
                frames.len()
            );
        }
        assert_eq!(frames.len(), 50);
        for (i, (header, body)) in frames.iter().enumerate() {
            let n = 50 + i as i64;
            assert_eq!(&body[..8], &n.to_le_bytes(), "frame {n}: body");
            assert_eq!(header.term_offset(), n as i32 * 64, "frame {n}: position");
            assert_eq!(header.reserved_value(), n * 7, "frame {n}: reserved value");
            assert_eq!(header.session_id(), session_id, "frame {n}: session");
            assert_eq!(header.stream_id(), REPLAY_STREAM, "frame {n}: stream");
        }
        assert_eq!(h.counter(PUBLICATION_CONNECTED), 1);
        assert_eq!(h.counter(SENDER_FRAMES_SENT), 50);

        // The replay ends at the recording's stop: `replay stopped` with reason 0, the
        // publication closing at 6400 and lingering with the sender.
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while replay_stopped(&got, 6).is_none() {
            cycle_with_sender(&mut h);
            got.extend(events(&mut h));
            assert!(Instant::now() < deadline, "the replay did not end");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(replay_stopped(&got, 6), Some((6400, 0)));
        assert_eq!(replay_position.load_acquire(), 6400);
        assert_eq!(h.counter(REPLAY_STOPPED), 1);
        assert_eq!(h.counter(REPLAY_ENDED), 1);
        assert!(h.conductor.replays.is_empty());
        {
            let publication = h
                .conductor
                .publications
                .iter()
                .find(|publication| publication.registration_id == 6)
                .expect("the publication lingers");
            assert_eq!(publication.closing.map(|(end, _)| end), Some(6400));
            assert_eq!(publication.log.end_of_stream_position(), 6400);
            let udp = publication.udp.as_ref().expect("over UDP");
            assert!(udp.close_sent && !udp.sender_closed);
        }
        h.conductor
            .duty_cycle(now_nanos() + spec::nanos(spec::PUBLICATION_LINGER) * 2);
        assert_eq!(
            h.conductor.publications.len(),
            2,
            "the conductor waits for the sender's reply, whatever its clock says"
        );

        // A heartbeat after the interval carries EOS at the end position.
        receiver.set_read_timeout(Duration::from_millis(500));
        let heartbeat = loop {
            h.sender
                .duty_cycle(now_nanos() + spec::nanos(Duration::from_millis(200)));
            match receiver.next_control() {
                Some(ControlFrame::Heartbeat(heartbeat)) if heartbeat.is_end_of_stream() => {
                    break heartbeat;
                }
                Some(_) => {}
                None => panic!("no heartbeat with EOS"),
            }
        };
        assert_eq!(heartbeat.session_id, session_id);
        assert_eq!(heartbeat.term_offset, 6400);

        // The receiver acknowledges with the end-of-stream flag: the sender replies
        // closed, the conductor removes the log and closes the endpoint, the sender
        // drops the socket, and the endpoint's and the publication's records are freed.
        let freed = h.counter(COUNTERS_FREED);
        receiver.send_status_message(&status(6400, STATUS_MESSAGE_END_OF_STREAM));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !h.conductor.send_endpoints.is_empty() {
            let now = now_nanos();
            h.sender.duty_cycle(now);
            h.conductor.duty_cycle(now);
            assert!(Instant::now() < deadline, "the endpoint did not close");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            h.conductor
                .publications
                .iter()
                .all(|publication| publication.registration_id != 6)
        );
        assert!(!h.dir.path().join("publications/6.logbuffer").exists());
        assert_eq!(h.counter(CHANNEL_CLOSED), 1);
        assert_eq!(
            h.counter(COUNTERS_FREED),
            freed + 11,
            "the endpoint's six and the publication's five; replay.position went at the stop"
        );
        assert_eq!(h.sender.positions_of(6), None, "the sender dropped it");
        drop(appender);
    }

    /// The replayer's request queue full, and its deferred requests ahead of a start
    /// (invariant 3; spec, "start replay"): a start that finds the queue full is code
    /// 10, counted, and registers nothing; a stop that finds it full waits in the
    /// deferred list; a start behind that is code 10 too; once the replayer catches up
    /// the stop is answered, the replay ends with reason 1, and a start goes through.
    #[test]
    fn a_replay_start_waits_behind_a_full_queue_and_the_deferred_requests() {
        let (mut h, mut appender) = recorded("replayer-deferred-order");
        offer(&mut h, &mut appender, 0, 1600);
        sync(&mut h);
        stop_recording(&mut h, 4);
        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, Some(5));
        let window = i64::from(h.conductor.term_length / 2);
        for _ in 0..20 {
            h.cycle();
        }
        let _ = events(&mut h);

        // The request queue full of stops of ids no replay holds, which the replayer
        // drops without an answer.
        let mut filler = 0;
        while h
            .conductor
            .replayer
            .requests
            .offer(replayer::Request::Stop {
                replay_session_id: 1_000_000 + filler,
                correlation_id: None,
                reason: ReplayStopReason::Stopped,
            })
            .is_ok()
        {
            filler += 1;
        }
        assert_eq!(filler, spec::REPLAYER_QUEUE_CAPACITY as i64);

        // A start that finds the queue full: code 10, counted, nothing registered.
        h.conductor.start_replay(
            49,
            0,
            NULL_POSITION,
            NULL_LENGTH,
            REPLAY_STREAM,
            IPC_CHANNEL,
        );
        let (code, message) = h.last_error(49).expect("an error event");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("request queue"), "{message}");
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), 1);
        assert_eq!(h.conductor.replays.len(), 1, "nothing registered");
        assert!(h.conductor.replayer_deferred.is_empty());

        // A stop waits for room; a start behind it is code 10 as well.
        h.conductor.stop_replay(50, 6);
        assert_eq!(h.conductor.replayer_deferred.len(), 1, "the stop waits");
        h.conductor.start_replay(
            51,
            0,
            NULL_POSITION,
            NULL_LENGTH,
            REPLAY_STREAM,
            IPC_CHANNEL,
        );
        let (code, message) = h.last_error(51).expect("an error event");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("request queue"), "{message}");
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), 2);
        assert_eq!(h.conductor.replays.len(), 1, "nothing registered");

        for _ in 0..1_000 {
            h.cycle();
            if h.conductor.replays.is_empty() {
                break;
            }
        }
        assert!(
            h.conductor.replays.is_empty(),
            "the stop reached the replayer"
        );
        assert!(h.conductor.replayer_deferred.is_empty());
        let got = events(&mut h);
        assert_eq!(successes(&got), vec![50], "the stop was answered");
        assert_eq!(replay_stopped(&got, 6), Some((window, 1)));
        assert_eq!(h.counter(REPLAY_STOPPED), 1);
        start_replay(&mut h, 52, NULL_POSITION, NULL_LENGTH, Some(53));
        assert_eq!(h.last_error(52), None);
        assert_eq!(h.conductor.replays.len(), 1, "the start went through");
        drop(appender);
    }

    /// The replayer's deferred requests at capacity (invariant 3): the registry full of
    /// replays, each of a recording of its own and each given its attach, its stop and
    /// its recording's stop while the request queue is full, fills the list to exactly
    /// its bound, three per registered replay, and not past its allocation, and every
    /// request reaches the replayer once it catches up. A recording's stop is one
    /// request for all its live replays, so replays sharing a recording stop short of
    /// the bound; one recording each is the case that reaches it (ADR 0036).
    #[test]
    fn the_replayers_deferred_requests_are_bounded_by_three_per_replay() {
        let (mut h, mut appender) = recorded("replayer-deferred-capacity");
        offer(&mut h, &mut appender, 0, 10);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let now = h.now;
        let allocated = h.conductor.replayer_deferred.capacity();
        while h
            .conductor
            .replayer
            .requests
            .offer(replayer::Request::Stop {
                replay_session_id: 1_000_000,
                correlation_id: None,
                reason: ReplayStopReason::Stopped,
            })
            .is_ok()
        {}
        let n = spec::MAX_REPLAYS as i64;
        for i in 0..n {
            h.conductor.replays.push(ReplayEntry {
                replay_session_id: 100_000 + i,
                recording_id: i,
                replay_stream_id: REPLAY_STREAM,
                state: ReplayState::Starting,
                position: None,
                unconnected_since: None,
                stop_requested: false,
                udp: None,
                connected: false,
            });
        }
        let term = cavalry_shm::log::layout::MIN_TERM_LENGTH as i32;
        for i in 0..n {
            h.conductor
                .on_replay_ready(100_000 + i, i, 0, (0, term, 1408), (0, STREAM), now);
            h.conductor.stop_replay(70_000 + i, 100_000 + i);
        }
        assert_eq!(
            h.conductor.replayer_deferred.len(),
            2 * spec::MAX_REPLAYS,
            "one attach and one stop per replay"
        );
        // Every recording stops, each with one registered replay of it: one request
        // per recording, the third per replay, and the list stands at its bound.
        for i in 0..n {
            h.conductor.on_stopped(i, 2, 640, None, now);
        }
        assert_eq!(
            h.conductor.replayer_deferred.len(),
            REPLAYER_DEFERRED_CAPACITY,
            "three per replay"
        );
        assert_eq!(
            h.conductor.replayer_deferred.capacity(),
            allocated,
            "the list did not grow"
        );
        assert_eq!(h.counter(REPLAY_STARTED), n);
        assert_eq!(h.conductor.replays.len(), spec::MAX_REPLAYS);

        for _ in 0..10_000 {
            h.cycle();
            if h.conductor.replayer_deferred.is_empty() && h.conductor.replayer.requests.is_empty()
            {
                break;
            }
        }
        assert!(
            h.conductor.replayer_deferred.is_empty(),
            "every request was offered"
        );
        assert!(
            h.conductor.replayer.requests.is_empty(),
            "the replayer took every one"
        );
        // The replayer held none of these replays, so each stop is answered with
        // nothing written, and the conductor ends every one.
        for _ in 0..100 {
            h.cycle();
            if h.conductor.replays.is_empty() {
                break;
            }
        }
        assert!(h.conductor.replays.is_empty(), "every stop was answered");
        assert_eq!(h.counter(REPLAY_STOPPED), n);
        drop(appender);
    }

    /// The replayer's pending replies at capacity (invariant 3): with the conductor
    /// taking no replies, the reply queue and then the pending replies fill; at the
    /// bound the replayer handles no request and ends no replay, but keeps writing,
    /// across a term; and once the conductor takes replies again the held end goes
    /// through with reason 0.
    #[test]
    fn the_replayers_pending_replies_are_bounded_and_it_keeps_writing() {
        let (mut h, mut appender) = recorded("replayer-pending");
        let count = 1600;
        let end = offer(&mut h, &mut appender, 0, count);
        sync(&mut h);
        stop_recording(&mut h, 4);
        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, Some(5));
        let replay_position = h.conductor.replays[0].position.clone().expect("allocated");
        let window = i64::from(h.conductor.term_length / 2);
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(
            replay_position.load_acquire(),
            window,
            "paced at the window"
        );

        // Stops of an id no replay holds, each answered, until the replies are at
        // their bound.
        let fill = |h: &mut Harness| {
            while h
                .conductor
                .replayer
                .requests
                .offer(replayer::Request::Stop {
                    replay_session_id: 1_000_000,
                    correlation_id: Some(1),
                    reason: ReplayStopReason::Stopped,
                })
                .is_ok()
            {}
        };
        for _ in 0..10_000 {
            if h.replayer.pending_replies() >= replayer::PENDING_REPLIES {
                break;
            }
            fill(&mut h);
            h.now += MS;
            h.replayer.duty_cycle(h.now);
        }
        assert_eq!(h.replayer.pending_replies(), replayer::PENDING_REPLIES);
        assert_eq!(
            h.conductor.replayer.replies.len(),
            spec::REPLAYER_QUEUE_CAPACITY
        );
        fill(&mut h);
        let queued = h.conductor.replayer.requests.len();

        // Room to the end, stored by hand since the conductor is not cycling: the
        // replayer writes everything, across the first term, and does not end the
        // replay while it has no room for the reply.
        h.conductor
            .publications
            .iter()
            .find(|publication| publication.registration_id == 6)
            .expect("the replay's publication")
            .limit
            .store_release(end);
        for _ in 0..8 {
            h.now += MS;
            h.replayer.duty_cycle(h.now);
        }
        assert_eq!(
            replay_position.load_acquire(),
            end,
            "the replayer kept writing"
        );
        assert_eq!(h.replayer.live_replays(), 1, "it ended nothing");
        assert_eq!(
            h.conductor.replayer.requests.len(),
            queued,
            "it handled no request"
        );
        assert_eq!(h.replayer.pending_replies(), replayer::PENDING_REPLIES);

        // The conductor takes replies again; the end goes through.
        wait_replay_end(&mut h, 6);
        let got = events(&mut h);
        assert_eq!(replay_stopped(&got, 6), Some((end, 0)));
        assert_eq!(h.counter(REPLAY_ENDED), 1);
        assert_eq!(h.counter(REPLAY_STOPPED), 1);
        assert_eq!(read_replay(&h, 6, 0).len(), count as usize);
        drop(appender);
    }

    // Retention (ADR 0031; card 95): truncate recording, purge segments and purge
    // recording on the conductor, the recorder and the replayer driven by hand: the
    // full cases first, then every error code, the registry's bound, and a file the
    // operating system refused.

    /// Cycles until the command `correlation_id` is answered: `Ok(())` for `operation
    /// success`, `Err((code, message))` for `error`.
    fn wait_answer(h: &mut Harness, correlation_id: i64) -> Result<(), (i32, String)> {
        for _ in 0..10_000 {
            h.cycle();
            let got = events(h);
            if successes(&got).contains(&correlation_id) {
                return Ok(());
            }
            if let Some((_, code, message)) = errors(&got)
                .into_iter()
                .find(|(id, _, _)| *id == correlation_id)
            {
                return Err((code, message));
            }
        }
        panic!("command {correlation_id} was not answered");
    }

    fn slot(h: &Harness, recording_id: i64) -> cavalry_log::layout::Descriptor {
        cavalry_log::catalog::CatalogReader::open(&h.log_dir)
            .expect("the catalog opens")
            .get(recording_id)
            .expect("the slot reads")
            .expect("the slot holds a recording")
    }

    fn segment(h: &Harness, recording_id: i64, base: i64) -> std::path::PathBuf {
        h.log_dir
            .join(cavalry_log::layout::segment_file_name(recording_id, base))
    }

    fn index_count(h: &Harness, recording_id: i64) -> usize {
        let bytes = std::fs::read(
            h.log_dir
                .join(cavalry_log::layout::index_file_name(recording_id)),
        )
        .expect("the index reads");
        (bytes.len() - cavalry_log::layout::HEADER_LENGTH) / cavalry_log::layout::ENTRY_LENGTH
    }

    /// The full truncate (invariants 1 and 3): 100 frames recorded and stopped, cut to
    /// 3200; the frames before stay and replay, the segment file is cut and extended
    /// with zeros past the position, the index holds the entries at or before it, the
    /// stop is the position with the last entry's timestamp, the counter moves, and a
    /// replay from the start ends at the new stop with reason 0.
    #[test]
    fn truncate_keeps_the_frames_before_the_position_and_rewrites_the_stop() {
        let (mut h, mut appender) = recorded("retention-truncate");
        let end = offer(&mut h, &mut appender, 0, 100);
        assert_eq!(end, 6400);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let entries_before = index_count(&h, 0);
        let _ = events(&mut h);

        h.conductor.retention(
            20,
            replayer::Retention::Truncate {
                recording_id: 0,
                position: 3200,
            },
        );
        assert_eq!(h.conductor.commands.len(), 1, "registered");
        wait_answer(&mut h, 20).expect("the truncate is answered with success");
        assert!(
            h.conductor.commands.is_empty(),
            "the entry left the registry"
        );
        assert_eq!(h.counter(RECORDING_TRUNCATED), 1);
        assert_eq!(h.counter(RECORDING_RETENTION_FAILED), 0);
        let after = slot(&h, 0);
        assert_eq!(after.stop_position(), 3200);
        assert!(after.stop_timestamp() >= after.start_timestamp());
        assert!(index_count(&h, 0) <= entries_before);
        let file = std::fs::File::open(segment(&h, 0, 0)).expect("the segment file stays");
        assert_eq!(
            file.metadata().expect("stat").len(),
            cavalry_log::layout::SEGMENT_FILE_LENGTH as u64,
            "extended again"
        );
        let mut past = [0u8; 64];
        file.read_exact_at(&mut past, 3200)
            .expect("reads past the cut");
        assert!(past.iter().all(|&b| b == 0), "zero past the position");

        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, Some(5));
        wait_replay_end(&mut h, 6);
        assert_eq!(replay_stopped(&events(&mut h), 6), Some((3200, 0)));
        assert_eq!(read_replay(&h, 6, 0).len(), 50);
        drop(appender);
    }

    /// A truncate to the retained position removes every file: the recording is
    /// recovery's empty recording, its stop at the start with the start timestamp, its
    /// index one entry, and a replay of it ends at once (ADR 0035).
    #[test]
    fn truncate_to_the_start_removes_the_files_and_leaves_an_empty_recording() {
        let (mut h, mut appender) = recorded("retention-truncate-start");
        offer(&mut h, &mut appender, 0, 100);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let _ = events(&mut h);
        h.conductor.retention(
            20,
            replayer::Retention::Truncate {
                recording_id: 0,
                position: 0,
            },
        );
        wait_answer(&mut h, 20).expect("success");
        assert_eq!(h.counter(RECORDING_TRUNCATED), 1);
        // The file a truncate removes is not a purged segment (card 177).
        assert_eq!(h.counter(RECORDING_SEGMENTS_PURGED), 0);
        let after = slot(&h, 0);
        assert_eq!(after.stop_position(), 0);
        assert_eq!(after.stop_timestamp(), after.start_timestamp());
        assert!(!segment(&h, 0, 0).exists(), "the file at the position went");
        assert_eq!(index_count(&h, 0), 1);
        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, Some(5));
        wait_replay_end(&mut h, 6);
        assert_eq!(replay_stopped(&events(&mut h), 6), Some((0, 0)));
        drop(appender);
    }

    /// The full purge (invariant 3): a recording of a segment and a bit, purged at the
    /// second segment's base: the header moves to version 1, the slot to version 1's
    /// form with the retained position there, the first file goes, the index keeps its
    /// entries, a replay from −1 starts at the retained position and one from 0 is
    /// refused with code 23 naming it.
    #[test]
    fn purge_segments_moves_the_retained_position_and_a_replay_starts_there() {
        let (mut h, mut appender) = recorded("retention-purge");
        let segment_length = i64::from(cavalry_log::layout::SEGMENT_FILE_LENGTH);
        let mut message = [0u8; 1000];
        let mut position = 0;
        let mut n = 0i64;
        while position < segment_length + 4096 {
            message[..8].copy_from_slice(&n.to_le_bytes());
            let limit = h.conductor.publications[0].limit.get();
            match appender.offer(&message, limit, 0) {
                Ok(after) => {
                    position = after;
                    n += 1;
                }
                Err(ClaimError::AdminAction) => {}
                Err(ClaimError::BackPressured) => h.cycle(),
                Err(error) => panic!("{error}"),
            }
        }
        let end = position;
        sync(&mut h);
        stop_recording(&mut h, 4);
        let entries_before = index_count(&h, 0);
        let _ = events(&mut h);

        h.conductor.retention(
            20,
            replayer::Retention::Purge {
                recording_id: 0,
                position: segment_length,
            },
        );
        wait_answer(&mut h, 20).expect("success");
        assert_eq!(h.counter(RECORDING_SEGMENTS_PURGED), 1);
        let after = slot(&h, 0);
        assert_eq!(after.form(), cavalry_log::layout::Form::Version1);
        assert_eq!(after.retained_position(), segment_length);
        assert_eq!(after.stop_position(), end);
        assert!(!segment(&h, 0, 0).exists());
        assert!(segment(&h, 0, segment_length).exists());
        assert_eq!(
            index_count(&h, 0),
            entries_before,
            "the index keeps its entries"
        );

        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, Some(5));
        let got = events(&mut h);
        let (_, _, start, _, _) = replay_ready(&got, 6).expect("replay ready");
        assert_eq!(start, segment_length, "-1 is the retained position");
        wait_replay_end(&mut h, 6);
        let _ = events(&mut h);
        h.conductor
            .start_replay(8, 0, 0, NULL_LENGTH, REPLAY_STREAM, IPC_CHANNEL);
        let (code, message) = wait_answer(&mut h, 8).expect_err("below the retained position");
        assert_eq!(code, 23, "{message}");
        assert!(message.contains(&segment_length.to_string()), "{message}");
        drop(appender);
    }

    /// The full delete (invariant 3): state 3, the files gone, the counter, and a replay
    /// or a second delete refused with code 21.
    #[test]
    fn purge_recording_deletes_the_files_and_keeps_the_slot() {
        let (mut h, mut appender) = recorded("retention-delete");
        offer(&mut h, &mut appender, 0, 100);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let _ = events(&mut h);
        h.conductor
            .retention(20, replayer::Retention::Delete { recording_id: 0 });
        wait_answer(&mut h, 20).expect("success");
        assert_eq!(h.counter(RECORDING_DELETED), 1);
        // The files a delete removes are not purged segments (card 177).
        assert_eq!(h.counter(RECORDING_SEGMENTS_PURGED), 0);
        assert_eq!(slot(&h, 0).state(), cavalry_log::layout::State::Deleted);
        assert!(!segment(&h, 0, 0).exists());
        assert!(
            !h.log_dir
                .join(cavalry_log::layout::index_file_name(0))
                .exists()
        );
        h.conductor
            .start_replay(8, 0, NULL_POSITION, NULL_LENGTH, REPLAY_STREAM, IPC_CHANNEL);
        let (code, _) = wait_answer(&mut h, 8).expect_err("a deleted recording");
        assert_eq!(code, 21);
        h.conductor
            .retention(21, replayer::Retention::Delete { recording_id: 0 });
        let (code, _) = wait_answer(&mut h, 21).expect_err("deleted once");
        assert_eq!(code, 21);
        drop(appender);
    }

    /// Every refusal with its spec code (invariant 6): 21 for an id past the catalog,
    /// 24 for a live recording, for one a replay reads, and for one under a command;
    /// 23 for a position unaligned, above the stop, inside a frame, not a segment base
    /// and at the retained position; 10 at the registry's bound; `start replay` on a
    /// recording under a command is 24 too, counted in `replay.start_failed` with the
    /// replayer's three refusals (invariant 4).
    #[test]
    fn retention_errors_get_the_spec_codes() {
        let (mut h, mut appender) = recorded("retention-errors");
        offer(&mut h, &mut appender, 0, 1000);
        sync(&mut h);
        h.conductor.retention(
            20,
            replayer::Retention::Truncate {
                recording_id: 0,
                position: 0,
            },
        );
        let (code, message) = wait_answer(&mut h, 20).expect_err("live");
        assert_eq!(code, 24, "{message}");
        assert!(message.contains("--stop 0"), "{message}");
        h.conductor
            .retention(21, replayer::Retention::Delete { recording_id: 7 });
        let (code, _) = wait_answer(&mut h, 21).expect_err("past the catalog");
        assert_eq!(code, 21);
        stop_recording(&mut h, 4);
        let _ = events(&mut h);

        // A replay longer than the window stays live while its subscriber does not read.
        start_replay(&mut h, 6, NULL_POSITION, NULL_LENGTH, Some(5));
        assert_eq!(h.conductor.replays.len(), 1);
        h.conductor.retention(
            22,
            replayer::Retention::Truncate {
                recording_id: 0,
                position: 64,
            },
        );
        let (code, message) = wait_answer(&mut h, 22).expect_err("read by a replay");
        assert_eq!(code, 24, "{message}");
        assert!(message.contains("stop_replay"), "{message}");
        h.conductor.stop_replay(9, 6);
        wait_replay_end(&mut h, 6);
        let _ = events(&mut h);

        for (id, position, what) in [
            (30, 48, "unaligned"),
            (31, 64_000 + 64, "above the stop"),
            (32, 32, "inside a frame"),
        ] {
            h.conductor.retention(
                id,
                replayer::Retention::Truncate {
                    recording_id: 0,
                    position,
                },
            );
            let (code, message) = wait_answer(&mut h, id).expect_err(what);
            assert_eq!(code, 23, "{what}: {message}");
        }
        let (_, message) = {
            h.conductor.retention(
                33,
                replayer::Retention::Truncate {
                    recording_id: 0,
                    position: 32,
                },
            );
            wait_answer(&mut h, 33).expect_err("inside a frame")
        };
        assert!(message.contains("no frame begins"), "{message}");
        for (id, position, what) in [
            (40, 64, "not a segment base"),
            (41, 0, "at the retained position"),
        ] {
            h.conductor.retention(
                id,
                replayer::Retention::Purge {
                    recording_id: 0,
                    position,
                },
            );
            let (code, message) = wait_answer(&mut h, id).expect_err(what);
            assert_eq!(code, 23, "{what}: {message}");
        }

        // Two commands on one recording in one cycle: the second is 24, and so is a
        // replay started while the first is in progress, which counts as the replayer's
        // refusals do.
        let start_failed_before = h.counter(REPLAY_START_FAILED);
        h.conductor.retention(
            50,
            replayer::Retention::Truncate {
                recording_id: 0,
                position: 6400,
            },
        );
        h.conductor
            .retention(51, replayer::Retention::Delete { recording_id: 0 });
        h.conductor.start_replay(
            52,
            0,
            NULL_POSITION,
            NULL_LENGTH,
            REPLAY_STREAM,
            IPC_CHANNEL,
        );
        let got = events(&mut h);
        let refused: Vec<(i64, i32)> = errors(&got)
            .into_iter()
            .map(|(id, code, _)| (id, code))
            .collect();
        assert!(refused.contains(&(51, 24)), "{refused:?}");
        assert!(refused.contains(&(52, 24)), "{refused:?}");
        assert_eq!(
            h.counter(REPLAY_START_FAILED),
            start_failed_before + 1,
            "the code 24 replay counts in replay.start_failed"
        );
        wait_answer(&mut h, 50).expect("the first command completes");

        // The registry's bound: 64 entries in progress refuse the 65th with code 10.
        for i in 0..spec::MAX_COMMANDS_IN_PROGRESS as i64 {
            h.conductor.commands.push(CommandEntry {
                correlation_id: 1000 + i,
                command: replayer::Retention::Delete {
                    recording_id: 1000 + i,
                },
                hop: Hop::Checking,
                pending: None,
                failure: None,
            });
        }
        h.conductor
            .retention(60, replayer::Retention::Delete { recording_id: 0 });
        let (code, message) = h.last_error(60).expect("refused at once");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("commands in progress"), "{message}");
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), 1);
        h.conductor.commands.clear();
        drop(appender);
    }

    /// A file the operating system refuses (invariant 1): the log directory made
    /// read-only before a delete's file work; the command is code 12 naming the file,
    /// the slot is repaired from the files, and `recording.retention_failed` moves.
    #[test]
    fn a_file_the_system_refuses_is_code_12_and_the_slot_is_repaired() {
        use std::os::unix::fs::PermissionsExt;
        let (mut h, mut appender) = recorded("retention-refused");
        offer(&mut h, &mut appender, 0, 100);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let _ = events(&mut h);
        let writable = std::fs::metadata(&h.log_dir).expect("stat").permissions();
        std::fs::set_permissions(&h.log_dir, std::fs::Permissions::from_mode(0o555))
            .expect("read-only");
        h.conductor
            .retention(20, replayer::Retention::Delete { recording_id: 0 });
        let answer = wait_answer(&mut h, 20);
        std::fs::set_permissions(&h.log_dir, writable).expect("writable again");
        let (code, message) = answer.expect_err("the file was refused");
        assert_eq!(code, 12, "{message}");
        assert!(message.contains("cannot remove"), "{message}");
        // The repair runs in the same read-only directory, so it reports its own
        // refusal; a directory writable again would read "repaired".
        assert!(message.contains("repair"), "{message}");
        assert_eq!(h.counter(RECORDING_RETENTION_FAILED), 1);
        assert_eq!(h.counter(RECORDING_DELETED), 0);
        assert!(h.conductor.commands.is_empty());
        drop(appender);
    }

    /// A segment file the operating system refuses at check time, before any catalog
    /// write (invariant 1; ADR 0031): the truncate is code 12 naming the file,
    /// `recording.retention_failed` moves, and the slot is as it was.
    #[test]
    fn a_file_refused_at_check_time_is_code_12_with_the_slot_unwritten() {
        use std::os::unix::fs::PermissionsExt;
        let (mut h, mut appender) = recorded("retention-check-refused");
        offer(&mut h, &mut appender, 0, 100);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let _ = events(&mut h);
        let before = slot(&h, 0);
        let file = segment(&h, 0, 0);
        let mode = std::fs::metadata(&file).expect("stat").permissions();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000))
            .expect("unreadable");
        h.conductor.retention(
            20,
            replayer::Retention::Truncate {
                recording_id: 0,
                position: 3200,
            },
        );
        let answer = wait_answer(&mut h, 20);
        std::fs::set_permissions(&file, mode).expect("readable again");
        let (code, message) = answer.expect_err("the file was refused");
        assert_eq!(code, 12, "{message}");
        assert_eq!(h.counter(RECORDING_RETENTION_FAILED), 1);
        assert_eq!(h.counter(RECORDING_TRUNCATED), 0);
        let after = slot(&h, 0);
        assert_eq!(after.stop_position(), before.stop_position(), "unwritten");
        assert_eq!(after.stop_timestamp(), before.stop_timestamp());
        assert!(!after.is_live(), "no cut was opened");
        assert!(h.conductor.commands.is_empty());
        drop(appender);
    }

    /// A truncate whose file work fails after the cut opened, on a segment file made
    /// read-only, with the file writable again before the repair runs (invariant 4; ADR
    /// 0031): the repair's file side runs on the replayer, the recorder writes the one
    /// slot it ended in, the stop is rebuilt from the files, and `recording.recovered`
    /// moves by one.
    #[test]
    fn a_repair_after_failed_file_work_counts_and_rebuilds_the_stop() {
        use std::os::unix::fs::PermissionsExt;
        let (mut h, mut appender) = recorded("retention-repair-counts");
        offer(&mut h, &mut appender, 0, 100);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let _ = events(&mut h);
        let before = slot(&h, 0);
        let recovered_before = h.counter(cavalry_shm::counters::generated::RECORDING_RECOVERED);
        let file = segment(&h, 0, 0);
        let mode = std::fs::metadata(&file).expect("stat").permissions();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).expect("read-only");
        h.conductor.retention(
            20,
            replayer::Retention::Truncate {
                recording_id: 0,
                position: 3200,
            },
        );
        // The cut opens, then the cut of the file fails; the repair is the next hop,
        // and the file is writable again before it runs.
        let mut cycles = 0;
        while h
            .conductor
            .commands
            .first()
            .is_none_or(|entry| entry.hop != Hop::RepairFiles)
        {
            h.cycle();
            cycles += 1;
            assert!(cycles < 10_000, "the file work did not fail");
        }
        std::fs::set_permissions(&file, mode).expect("writable again");
        let (code, message) = wait_answer(&mut h, 20).expect_err("the file was refused");
        assert_eq!(code, 12, "{message}");
        assert!(
            message.contains("the slot was repaired from the files"),
            "{message}"
        );
        assert_eq!(h.counter(RECORDING_RETENTION_FAILED), 1);
        assert_eq!(
            h.counter(cavalry_shm::counters::generated::RECORDING_RECOVERED),
            recovered_before + 1,
            "the rebuilt stop is counted"
        );
        let after = slot(&h, 0);
        assert!(!after.is_live(), "the cut was closed by the repair");
        assert_eq!(
            after.stop_position(),
            before.stop_position(),
            "rebuilt from the files"
        );
        assert!(h.conductor.commands.is_empty());
        drop(appender);
    }

    /// A slot the parser refuses (`spec/client-protocol.md`, "truncate recording": a
    /// state other than 1 is code 21; invariants 4 and 6): state 2 written over the
    /// stopped recording's slot, as recovery writes it for damage, and the delete an
    /// operator sends after `cavalry doctor`'s `recording-invalid` finding is code 21
    /// naming the slot and the docs anchor, not code 12 for a file; nothing is written
    /// and `recording.retention_failed` does not move.
    #[test]
    fn a_slot_in_state_2_is_code_21_and_not_a_file_refused() {
        let (mut h, mut appender) = recorded("retention-invalid-slot");
        offer(&mut h, &mut appender, 0, 100);
        sync(&mut h);
        stop_recording(&mut h, 4);
        let _ = events(&mut h);
        OpenOptions::new()
            .write(true)
            .open(h.log_dir.join(cavalry_log::layout::CATALOG_FILE_NAME))
            .expect("the catalog opens")
            .write_all_at(
                &(cavalry_log::layout::State::Invalid as i32).to_le_bytes(),
                cavalry_log::layout::CatalogHeader::slot_offset(0),
            )
            .expect("state 2 is written");
        h.conductor
            .retention(20, replayer::Retention::Delete { recording_id: 0 });
        let (code, message) = wait_answer(&mut h, 20).expect_err("an invalid slot");
        assert_eq!(code, 21, "{message}");
        assert!(message.contains("its slot cannot be read"), "{message}");
        assert!(message.contains(ERRORS_DOC), "{message}");
        assert_eq!(h.counter(RECORDING_RETENTION_FAILED), 0);
        assert_eq!(h.counter(RECORDING_DELETED), 0);
        assert!(h.conductor.commands.is_empty());
        assert!(segment(&h, 0, 0).exists(), "nothing was written or removed");
        drop(appender);
    }

    /// A stop whose own writes fail (invariant 1; ADR 0064). Ten frames are synced,
    /// 640 bytes, and ten more are written and not synced, 1,280. Then the stop's
    /// catalog write, the last step of `LogDir::stop`, is refused: the slot's stop
    /// checksum no longer matches its stop fields. The stop's index entry and syncs go
    /// through files the recorder holds open, which a test cannot make fail without a
    /// full disk; a failure at any step of the stop takes the same branch of
    /// `Recorder::stop_at`. The recording is stopped all the same: the command is
    /// answered with `operation success`, `recording.write_failed` and
    /// `recording.stopped` move by one, and `recording stopped` carries 640, the
    /// position the last sync before the stop made durable, and not 1,280, the position
    /// written. The position counter keeps 1,280 and the synced counter 640. At the
    /// next open, recovery rebuilds the stop from the files, at or above 640.
    #[test]
    fn a_stop_whose_own_write_fails_announces_the_position_last_synced() {
        let (mut h, mut appender) = recorded("recording-stop-fails");
        offer(&mut h, &mut appender, 0, 10);
        sync(&mut h);
        let position = h.conductor.recordings[0]
            .position
            .clone()
            .expect("attached");
        let synced = h.conductor.recordings[0].synced.clone().expect("attached");
        assert_eq!(synced.load_acquire(), 640, "ten frames synced");
        // Ten more, written within a few milliseconds of the test's clock: no sync.
        let written = offer(&mut h, &mut appender, 10, 10);
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(written, 1280);
        assert_eq!(position.load_acquire(), 1280, "written");
        assert_eq!(synced.load_acquire(), 640, "not synced");
        let _ = events(&mut h);

        let catalog = OpenOptions::new()
            .read(true)
            .write(true)
            .open(h.log_dir.join(cavalry_log::layout::CATALOG_FILE_NAME))
            .expect("the catalog opens");
        let at = cavalry_log::layout::CatalogHeader::slot_offset(0) + 4;
        let mut checksum = [0u8; 4];
        catalog
            .read_exact_at(&mut checksum, at)
            .expect("the stop checksum reads");
        let damaged = (u32::from_le_bytes(checksum) ^ u32::MAX).to_le_bytes();
        catalog
            .write_all_at(&damaged, at)
            .expect("the stop checksum is damaged");

        stop_recording(&mut h, 4);
        let events = events(&mut h);
        assert_eq!(successes(&events), vec![4], "the stop is answered");
        let stopped: Vec<(i64, i64)> = events
            .iter()
            .filter_map(|(type_id, message)| match Event::parse(*type_id, message) {
                Ok(Event::RecordingStopped {
                    recording_id,
                    stop_position,
                }) => Some((recording_id, stop_position)),
                _ => None,
            })
            .collect();
        assert_eq!(
            stopped,
            vec![(0, 640)],
            "the position last synced, not the position written"
        );
        assert_eq!(
            h.counter(cavalry_shm::counters::generated::RECORDING_WRITE_FAILED),
            1
        );
        assert_eq!(h.counter(RECORDING_STOPPED), 1);
        assert_eq!(position.load_acquire(), 1280, "not lowered");
        assert_eq!(synced.load_acquire(), 640, "the last sync");

        // The recorder holds the directory's lock until it goes. The slot reads live,
        // and recovery rebuilds its stop from the files: the stop's own syncs ran
        // before its catalog write failed, so the files hold all 1,280 bytes.
        drop(appender);
        drop(h.recorder);
        let (log, recovery) =
            cavalry_log::dir::LogDir::open(&h.log_dir, h.now).expect("the log directory opens");
        assert_eq!(recovery.recovered, 1, "the live slot's stop is rebuilt");
        let rebuilt = log
            .catalog()
            .get(0)
            .expect("the slot reads")
            .expect("the slot holds the recording")
            .stop_position();
        assert!(
            rebuilt >= 640,
            "recovery's stop {rebuilt} is below the stop announced"
        );
        assert_eq!(rebuilt, 1280);
    }

    /// A recording one of whose syncs failed is given up with no stop run (invariant
    /// 1; ADR 0065). Ten frames are synced, 640 bytes, and ten more are written and not
    /// synced, 1,280. Then the recording is marked as the two failure branches of
    /// `Recorder::record` mark it when a sync fails: a test cannot make an `fdatasync`
    /// on the recorder's own descriptors fail, so it takes the branch's effect through
    /// `Recorder::fail_sync`, and what it does not cover is the branch's own lines and
    /// the writer's, which set the mark. The recorder's next visit stops the recording without `LogDir::stop`:
    /// `recording stopped` carries 640, the position the last sync that succeeded
    /// covered, and not 1,280; no command is answered, since none asked;
    /// `recording.write_failed` and `recording.stopped` move by one; the counters keep
    /// 1,280 and 640; and the catalog holds no stop, where the stop that ran before
    /// this decision wrote 1,280 after a sync that may have written nothing. Frames
    /// committed after the failure are not written. At the next open, recovery
    /// rebuilds the stop from the files, at or above 640.
    #[test]
    fn a_recording_whose_sync_failed_is_given_up_at_the_position_last_synced() {
        let (mut h, mut appender) = recorded("recording-sync-failed");
        offer(&mut h, &mut appender, 0, 10);
        sync(&mut h);
        let position = h.conductor.recordings[0]
            .position
            .clone()
            .expect("attached");
        let synced = h.conductor.recordings[0].synced.clone().expect("attached");
        assert_eq!(synced.load_acquire(), 640, "ten frames synced");
        let written = offer(&mut h, &mut appender, 10, 10);
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(written, 1280);
        assert_eq!(position.load_acquire(), 1280, "written");
        assert_eq!(synced.load_acquire(), 640, "not synced");
        let _ = events(&mut h);

        // The failure, and ten frames more that the recorder must not write after it.
        h.recorder.fail_sync(0);
        offer(&mut h, &mut appender, 20, 10);
        for _ in 0..20 {
            h.cycle();
        }
        let events = events(&mut h);
        assert_eq!(successes(&events), Vec::<i64>::new(), "no command asked");
        let stopped: Vec<(i64, i64)> = events
            .iter()
            .filter_map(|(type_id, message)| match Event::parse(*type_id, message) {
                Ok(Event::RecordingStopped {
                    recording_id,
                    stop_position,
                }) => Some((recording_id, stop_position)),
                _ => None,
            })
            .collect();
        assert_eq!(
            stopped,
            vec![(0, 640)],
            "the position last synced, not the position written"
        );
        assert_eq!(
            h.counter(cavalry_shm::counters::generated::RECORDING_WRITE_FAILED),
            1
        );
        assert_eq!(h.counter(RECORDING_STOPPED), 1);
        assert_eq!(
            position.load_acquire(),
            1280,
            "nothing written after the failure"
        );
        assert_eq!(synced.load_acquire(), 640, "the last sync that succeeded");
        assert!(h.conductor.recordings.is_empty());
        assert_eq!(h.recorder.live_recordings(), 0);

        // No stop was run: the slot reads live, with no stop position.
        let slot = cavalry_log::catalog::CatalogReader::open(&h.log_dir)
            .expect("the catalog opens")
            .get(0)
            .expect("the slot reads")
            .expect("the slot holds the recording");
        assert!(slot.is_live(), "the catalog holds no stop: {slot:?}");

        // The recorder holds the directory's lock until it goes. Recovery rebuilds the
        // stop from the files, which here hold what was written before the failure.
        drop(appender);
        drop(h.recorder);
        let (log, recovery) =
            cavalry_log::dir::LogDir::open(&h.log_dir, h.now).expect("the log directory opens");
        assert_eq!(recovery.recovered, 1, "the live slot's stop is rebuilt");
        let rebuilt = log
            .catalog()
            .get(0)
            .expect("the slot reads")
            .expect("the slot holds the recording")
            .stop_position();
        assert!(
            rebuilt >= 640,
            "recovery's stop {rebuilt} is below the stop announced"
        );
        assert_eq!(rebuilt, 1280);
    }

    /// The same failure with no room for the stop's reply (invariant 1; ADR 0065). The
    /// recorder's pending replies are at their bound when the sync fails, so the
    /// recording cannot be stopped yet, and it waits for room. While it waits it is
    /// neither written nor synced again, whatever is committed and however long it
    /// lasts: before this decision the recorder synced it again in the next cycle,
    /// that sync returned success, and `recording.synced_position` moved over the
    /// bytes the first sync had failed on. Ten frames are synced, 640 bytes, and ten
    /// more written and not synced, 1,280; the replies are filled with the conductor
    /// taking none; the recording is marked as a failed sync marks it; ten more frames
    /// are committed and more than a sync interval passes over several of the
    /// recorder's duty cycles. The position stays 1,280 and the synced position 640,
    /// the recording is still the recorder's, and nothing is counted. Once the
    /// conductor takes replies the recording is given up: `recording stopped` carries
    /// 640, and the catalog holds no stop.
    #[test]
    fn a_recording_whose_sync_failed_is_not_synced_again_while_it_waits_for_room() {
        use crate::recorder::{PENDING_REPLIES, Request};

        let (mut h, mut appender) = recorded("recording-sync-failed-no-room");
        offer(&mut h, &mut appender, 0, 10);
        sync(&mut h);
        let position = h.conductor.recordings[0]
            .position
            .clone()
            .expect("attached");
        let synced = h.conductor.recordings[0].synced.clone().expect("attached");
        assert_eq!(synced.load_acquire(), 640, "ten frames synced");
        let written = offer(&mut h, &mut appender, 10, 10);
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(written, 1280);
        assert_eq!(position.load_acquire(), 1280, "written");
        assert_eq!(synced.load_acquire(), 640, "not synced");
        let _ = events(&mut h);

        // Stops of ids no recording holds, each answered, until the recorder's pending
        // replies are at their bound: the recorder's duty cycle alone runs, so the
        // conductor takes none, and the clock stands still, so no sync falls due.
        let fill = |h: &mut Harness| {
            while h
                .conductor
                .recorder
                .requests
                .offer(Request::Stop {
                    recording_id: 1_000_000,
                    correlation_id: None,
                })
                .is_ok()
            {}
        };
        for _ in 0..10_000 {
            if h.recorder.pending_replies() >= PENDING_REPLIES {
                break;
            }
            fill(&mut h);
            h.recorder.duty_cycle(h.now);
        }
        assert_eq!(h.recorder.pending_replies(), PENDING_REPLIES);
        assert_eq!(
            synced.load_acquire(),
            640,
            "no sync while the replies filled"
        );

        // The failure, ten frames more, and more than a sync interval over several
        // duty cycles with no room for the stop's reply.
        h.recorder.fail_sync(0);
        offer(&mut h, &mut appender, 20, 10);
        for _ in 0..8 {
            h.now += SECOND;
            h.recorder.duty_cycle(h.now);
        }
        assert_eq!(position.load_acquire(), 1280, "not written while it waits");
        assert_eq!(
            synced.load_acquire(),
            640,
            "not synced again while it waits"
        );
        assert_eq!(h.recorder.live_recordings(), 1, "not stopped yet");
        assert_eq!(h.recorder.pending_replies(), PENDING_REPLIES);
        assert_eq!(
            h.counter(cavalry_shm::counters::generated::RECORDING_WRITE_FAILED),
            0
        );
        assert_eq!(h.counter(RECORDING_STOPPED), 0);

        // The conductor takes replies again, and the recording is given up.
        for _ in 0..10_000 {
            if h.conductor.recordings.is_empty() {
                break;
            }
            h.cycle();
        }
        assert!(
            h.conductor.recordings.is_empty(),
            "the recording was given up"
        );
        let stopped: Vec<(i64, i64)> = events(&mut h)
            .iter()
            .filter_map(|(type_id, message)| match Event::parse(*type_id, message) {
                Ok(Event::RecordingStopped {
                    recording_id,
                    stop_position,
                }) => Some((recording_id, stop_position)),
                _ => None,
            })
            .collect();
        assert_eq!(stopped, vec![(0, 640)], "the position last synced");
        assert_eq!(
            h.counter(cavalry_shm::counters::generated::RECORDING_WRITE_FAILED),
            1
        );
        assert_eq!(h.counter(RECORDING_STOPPED), 1);
        assert_eq!(position.load_acquire(), 1280);
        assert_eq!(synced.load_acquire(), 640);
        assert_eq!(h.recorder.live_recordings(), 0);
        let slot = cavalry_log::catalog::CatalogReader::open(&h.log_dir)
            .expect("the catalog opens")
            .get(0)
            .expect("the slot reads")
            .expect("the slot holds the recording");
        assert!(slot.is_live(), "the catalog holds no stop: {slot:?}");
        drop(appender);
    }

    /// Every `recording stopped` among `events`: (recording id, stop position).
    fn recording_stops(events: &[(i32, Vec<u8>)]) -> Vec<(i64, i64)> {
        events
            .iter()
            .filter_map(|(type_id, message)| match Event::parse(*type_id, message) {
                Ok(Event::RecordingStopped {
                    recording_id,
                    stop_position,
                }) => Some((recording_id, stop_position)),
                _ => None,
            })
            .collect()
    }

    /// `sync recording` at the request queue's bound, the full case first (invariant
    /// 3; ADR 0069). With the recorder's request queue full, the command is refused
    /// with code 10 and counted in `driver.registry_full`, and it is not kept: the
    /// deferred list stays empty, so a client's syncs never take the room a
    /// recording's attach or stop is owed. With a request waiting in the deferred list
    /// the command is refused the same way, since the recorder must see requests in
    /// order. Once the recorder has caught up, the command is taken and answered.
    #[test]
    fn a_sync_recording_that_finds_the_request_queue_full_is_refused_and_not_kept() {
        use crate::recorder::Request;

        let (mut h, mut appender) = recorded("sync-recording-queue-full");
        offer(&mut h, &mut appender, 0, 10);
        for _ in 0..20 {
            h.cycle();
        }
        let synced = h.conductor.recordings[0].synced.clone().expect("attached");
        assert_eq!(synced.load_acquire(), 0, "no sync fell due");
        let _ = events(&mut h);
        let stop_of_no_recording = || Request::Stop {
            recording_id: 1_000_000,
            correlation_id: None,
        };
        while h
            .conductor
            .recorder
            .requests
            .offer(stop_of_no_recording())
            .is_ok()
        {}
        let registry_full = h.counter(DRIVER_REGISTRY_FULL);

        h.conductor.sync_recording(40, 0);
        let refused = errors(&events(&mut h));
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!((refused[0].0, refused[0].1), (40, 10));
        assert!(refused[0].2.contains(ERRORS_DOC), "{}", refused[0].2);
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), registry_full + 1);
        assert!(h.conductor.deferred.is_empty(), "the command is not kept");
        assert_eq!(synced.load_acquire(), 0, "nothing synced");

        // The queue drained, 16 requests per recorder duty cycle, and one request
        // still waiting for room in it.
        let drain = |h: &mut Harness| {
            for _ in 0..10_000 {
                h.cycle();
                if h.conductor.recorder.requests.is_empty() && h.conductor.deferred.is_empty() {
                    break;
                }
            }
            h.cycle();
            assert_eq!(h.conductor.recorder.requests.len(), 0);
        };
        drain(&mut h);
        let _ = events(&mut h);
        h.conductor.deferred.push_back(stop_of_no_recording());
        h.conductor.sync_recording(41, 0);
        let refused = errors(&events(&mut h));
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!((refused[0].0, refused[0].1), (41, 10));
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), registry_full + 2);
        assert_eq!(h.conductor.deferred.len(), 1, "the command is not kept");

        // The recorder caught up: the command is taken and answered.
        drain(&mut h);
        assert!(h.conductor.deferred.is_empty());
        let _ = events(&mut h);
        h.conductor.sync_recording(42, 0);
        h.cycle();
        let events = events(&mut h);
        assert_eq!(successes(&events), vec![42]);
        assert_eq!(errors(&events), Vec::new());
        assert_eq!(synced.load_acquire(), 640);
        drop(appender);
    }

    /// `sync recording` syncs what the recorder has written when it takes the request,
    /// without waiting for the recorder's sync once a second (invariant 1; ADR 0069).
    /// Ten frames are written, 640 bytes, 20 ms after the recording started, so no
    /// sync is due and the synced position is the start position, 0. The command
    /// moves it to 640 in the recorder's next duty cycle and is answered with
    /// `operation success`. A second command with nothing unsynced is answered the
    /// same way. Ten more frames, and a third command moves it to 1,280. All of it
    /// happens inside the recording's first second, in which the recorder's own sync
    /// never ran. Then the errors: an id at the catalog's next id, and one below 0,
    /// are code 21; and a recording that has stopped is `operation success` with
    /// nothing done.
    #[test]
    fn a_sync_recording_syncs_what_the_recorder_has_written_without_waiting_a_second() {
        let (mut h, mut appender) = recorded("sync-recording");
        let started = h.now;
        offer(&mut h, &mut appender, 0, 10);
        for _ in 0..20 {
            h.cycle();
        }
        let position = h.conductor.recordings[0]
            .position
            .clone()
            .expect("attached");
        let synced = h.conductor.recordings[0].synced.clone().expect("attached");
        assert_eq!(position.load_acquire(), 640, "written");
        assert_eq!(synced.load_acquire(), 0, "no sync fell due");
        let _ = events(&mut h);

        h.conductor.sync_recording(40, 0);
        assert_eq!(
            synced.load_acquire(),
            0,
            "the conductor never touches the disk"
        );
        h.cycle();
        let answered = events(&mut h);
        assert_eq!(successes(&answered), vec![40]);
        assert_eq!(errors(&answered), Vec::new());
        assert_eq!(synced.load_acquire(), 640, "synced on request");

        h.conductor.sync_recording(41, 0);
        h.cycle();
        assert_eq!(
            successes(&events(&mut h)),
            vec![41],
            "nothing unsynced is success"
        );
        assert_eq!(synced.load_acquire(), 640);

        offer(&mut h, &mut appender, 10, 10);
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(position.load_acquire(), 1280, "written");
        assert_eq!(synced.load_acquire(), 640, "not synced yet");
        h.conductor.sync_recording(42, 0);
        h.cycle();
        assert_eq!(successes(&events(&mut h)), vec![42]);
        assert_eq!(synced.load_acquire(), 1280, "synced on request");
        assert!(
            h.now - started < SECOND,
            "all of it before the recorder's own sync was due"
        );
        assert_eq!(
            h.counter(cavalry_shm::counters::generated::RECORDING_WRITE_FAILED),
            0
        );

        // An id the catalog does not hold, either side.
        h.conductor.sync_recording(43, 1);
        h.conductor.sync_recording(44, -1);
        let refused = errors(&events(&mut h));
        assert_eq!(
            refused
                .iter()
                .map(|(id, code, _)| (*id, *code))
                .collect::<Vec<_>>(),
            vec![(43, 21), (44, 21)]
        );
        assert!(
            refused
                .iter()
                .all(|(_, _, message)| message.contains(ERRORS_DOC)),
            "{refused:?}"
        );

        // A recording that has stopped: success at once, with nothing handed over.
        stop_recording(&mut h, 45);
        let _ = events(&mut h);
        h.conductor.sync_recording(46, 0);
        assert_eq!(successes(&events(&mut h)), vec![46]);
        assert_eq!(h.conductor.recorder.requests.len(), 0);
        drop(appender);
    }

    /// A failed sync's refusal ends with the page and section that explains it, as the
    /// spec has every error message end (`spec/client-protocol.md`, "Events", error
    /// message), after what the sync itself said: a client names the section once,
    /// and the C ABI keeps it when it cuts a long message.
    #[test]
    fn a_failed_syncs_refusal_ends_with_its_docs_section() {
        let mut h = Harness::new("sync-failed-message");
        let _ = events(&mut h);
        h.conductor.on_sync_done(
            47,
            3,
            SyncOutcome::Failed("Input/output error (os error 5)".to_string()),
        );
        let refused = errors(&events(&mut h));
        assert_eq!(refused.len(), 1, "{refused:?}");
        let (correlation_id, code, message) = &refused[0];
        assert_eq!(
            (*correlation_id, *code),
            (47, ErrorCode::StorageSpace as i32)
        );
        assert!(
            message.starts_with("a sync of recording 3 failed, so the recording is given up"),
            "{message}"
        );
        assert!(
            message.ends_with(&format!(
                "; the sync said: Input/output error (os error 5); see {ERRORS_DOC}"
            )),
            "{message}"
        );
    }

    /// A `sync recording` for a recording one of whose syncs has already failed is
    /// refused with code 12, and no sync is made (invariant 1; ADR 0065, ADR 0069).
    /// Ten frames are synced, 640 bytes, and ten more written and not synced, 1,280.
    /// The recording is marked as a failed sync marks it, through `Recorder::fail_sync`
    /// as the tests of ADR 0065 do, since a test cannot make an `fdatasync` fail; what
    /// this does not run is the branch in `Recorder::sync_now` that sets the mark when
    /// the command's own sync fails. The command reaches the recorder before its
    /// visit to the recording. A sync made then would return success on a healthy
    /// disk, as it can on a disk whose writeback failed, and move the synced position
    /// to 1,280 over the bytes the failed sync left. Instead the command is answered
    /// with code 12, the synced position stays 640, and the recording is given up in
    /// the same duty cycle: `recording stopped` carries 640.
    #[test]
    fn a_sync_recording_after_a_sync_failed_is_refused_and_makes_no_sync() {
        let (mut h, mut appender) = recorded("sync-recording-after-failure");
        offer(&mut h, &mut appender, 0, 10);
        sync(&mut h);
        let position = h.conductor.recordings[0]
            .position
            .clone()
            .expect("attached");
        let synced = h.conductor.recordings[0].synced.clone().expect("attached");
        assert_eq!(synced.load_acquire(), 640, "ten frames synced");
        offer(&mut h, &mut appender, 10, 10);
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(position.load_acquire(), 1280, "written");
        assert_eq!(synced.load_acquire(), 640, "not synced");
        let _ = events(&mut h);

        h.recorder.fail_sync(0);
        h.conductor.sync_recording(50, 0);
        h.cycle();
        let events = events(&mut h);
        assert_eq!(successes(&events), Vec::<i64>::new(), "not a success");
        let refused = errors(&events);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!((refused[0].0, refused[0].1), (50, 12));
        assert!(
            refused[0].2.contains("recording 0") && refused[0].2.contains(ERRORS_DOC),
            "{}",
            refused[0].2
        );
        assert_eq!(
            recording_stops(&events),
            vec![(0, 640)],
            "given up at the position last synced"
        );
        assert_eq!(synced.load_acquire(), 640, "no sync was made");
        assert_eq!(position.load_acquire(), 1280);
        assert_eq!(
            h.counter(cavalry_shm::counters::generated::RECORDING_WRITE_FAILED),
            1
        );
        assert_eq!(h.recorder.live_recordings(), 0);
        drop(appender);
    }

    /// A `sync recording` that reaches the recorder after the recording stopped is
    /// `operation success` with nothing done (ADR 0069). The conductor still holds the
    /// recording as live when it takes the command, because the recorder's stop is
    /// ahead of the command in the request queue and its reply has not come back: the
    /// recorder stops the recording at the position written, with the stop's own
    /// sync, and then finds no recording to sync.
    #[test]
    fn a_sync_recording_behind_a_stop_in_the_queue_is_success_with_nothing_done() {
        use crate::recorder::Request;

        let (mut h, mut appender) = recorded("sync-recording-behind-a-stop");
        offer(&mut h, &mut appender, 0, 10);
        for _ in 0..20 {
            h.cycle();
        }
        let _ = events(&mut h);
        assert!(
            h.conductor
                .recorder
                .requests
                .offer(Request::Stop {
                    recording_id: 0,
                    correlation_id: None,
                })
                .is_ok()
        );
        h.conductor.sync_recording(60, 0);
        assert_eq!(
            h.conductor.recorder.requests.len(),
            2,
            "handed to the recorder"
        );
        h.cycle();
        let events = events(&mut h);
        assert_eq!(recording_stops(&events), vec![(0, 640)]);
        assert_eq!(successes(&events), vec![60]);
        assert_eq!(errors(&events), Vec::new());
        assert_eq!(h.recorder.live_recordings(), 0);
        drop(appender);
    }

    /// A `sync recording` for a recording that the recorder gave up after a sync
    /// failed is `operation success` with nothing done, and that success claims
    /// nothing past the position last synced (invariant 1; ADR 0065, ADR 0069). Ten
    /// frames are synced, 640 bytes, and ten more written and not synced, 1,280. The
    /// recording is marked as a failed sync marks it, and the recorder's duty cycle
    /// alone runs, so the recorder gives the recording up while the conductor still
    /// holds it. A command taken then goes to the recorder, which no longer holds the
    /// recording; one taken once the conductor has read the stop is answered at once.
    /// Both are `operation success`, both come after `recording stopped` at 640, and
    /// the synced position stays 640 with 1,280 written: a client that acknowledges
    /// up to `synced_position` after `Ok`, as `Recording::sync` says, acknowledges
    /// nothing a sync did not cover, and one that reads the events in order has the
    /// stop before the answer.
    #[test]
    fn a_sync_recording_after_a_failed_sync_gave_the_recording_up_is_success_after_its_stop() {
        let (mut h, mut appender) = recorded("sync-recording-after-giving-up");
        offer(&mut h, &mut appender, 0, 10);
        sync(&mut h);
        let position = h.conductor.recordings[0]
            .position
            .clone()
            .expect("attached");
        let synced = h.conductor.recordings[0].synced.clone().expect("attached");
        assert_eq!(synced.load_acquire(), 640, "ten frames synced");
        offer(&mut h, &mut appender, 10, 10);
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(position.load_acquire(), 1280, "written");
        assert_eq!(synced.load_acquire(), 640, "not synced");
        let _ = events(&mut h);

        // The recorder alone runs: it gives the recording up, and the conductor has
        // not read its reply.
        h.recorder.fail_sync(0);
        h.now += MS;
        h.recorder.duty_cycle(h.now);
        assert_eq!(h.recorder.live_recordings(), 0, "given up");
        assert_eq!(
            h.conductor.recordings.len(),
            1,
            "the conductor holds it still"
        );

        // Taken while the conductor holds the recording: handed to the recorder.
        h.conductor.sync_recording(70, 0);
        assert_eq!(
            h.conductor.recorder.requests.len(),
            1,
            "handed to the recorder"
        );
        h.cycle();
        assert!(
            h.conductor.recordings.is_empty(),
            "the conductor read the stop"
        );

        // Taken once the conductor has read the stop: answered at once.
        h.conductor.sync_recording(71, 0);
        assert_eq!(
            h.conductor.recorder.requests.len(),
            0,
            "nothing handed over"
        );

        let events = events(&mut h);
        let order: Vec<String> = events
            .iter()
            .filter_map(|(type_id, message)| match Event::parse(*type_id, message) {
                Ok(Event::RecordingStopped {
                    recording_id,
                    stop_position,
                }) => Some(format!(
                    "recording {recording_id} stopped at {stop_position}"
                )),
                Ok(Event::OperationSuccess { correlation_id }) => {
                    Some(format!("success {correlation_id}"))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            order,
            ["recording 0 stopped at 640", "success 70", "success 71"],
            "the stop at the position last synced, then the two answers"
        );
        assert_eq!(errors(&events), Vec::new());
        assert_eq!(synced.load_acquire(), 640, "no sync was made");
        assert_eq!(position.load_acquire(), 1280);
        assert_eq!(
            h.counter(cavalry_shm::counters::generated::RECORDING_WRITE_FAILED),
            1
        );
        drop(appender);
    }
}

#[cfg(test)]
mod receiving {
    //! The conductor and the receiver driven by hand, duty cycle by duty cycle, on a
    //! clock the test advances, against a sender by hand on a loopback socket: a
    //! `cavalry:udp` subscription's endpoint, the image a setup frame creates, data in
    //! and out of order with the NAK that mends the gap, the status messages on their
    //! triggers, the end of stream, the timeouts and the session table's states
    //! (`spec/udp.md`, "The receiver"; ADR 0042). The full cases of invariant 3 first.
    //! A datagram sent on the loopback lands when the kernel delivers it, so every
    //! expectation that follows a send cycles until it holds.

    use std::net::{SocketAddr, UdpSocket};
    use std::time::Duration;

    use cavalry_proto::frame::{DataFrameHeader, Flags, FrameType};
    use cavalry_proto::udp::{
        ControlFrame, Heartbeat, MIN_TERM_LENGTH, Nak, RttMeasurement, SetupFrame, StatusMessage,
        parse as parse_udp,
    };
    use cavalry_shm::counters::generated::{
        CHANNEL_CLOSED, CHANNEL_OPENED, COUNTERS_ALLOCATED, IMAGE_CREATED, IMAGE_ENDED,
        IMAGE_REMOVED, IMAGE_TIMED_OUT, IMAGE_UNSUBSCRIBED, LOG_BUFFER_REJECTED_POSITION,
        RECEIVER_DROPPED_COOL_DOWN, RECEIVER_DROPPED_FRAMES, RECEIVER_DROPPED_IN_PROGRESS,
        RECEIVER_DROPPED_PENDING, RECEIVER_DROPPED_UNWANTED, RECEIVER_FRAMES_RECEIVED,
        RECEIVER_GAPS_DETECTED, RECEIVER_HEARTBEATS_RECEIVED, RECEIVER_NAKS_SENT,
        RECEIVER_OVER_RUNS, RECEIVER_REJECTED_DATAGRAMS, RECEIVER_RTT_REPLIES_SENT,
        RECEIVER_SETUPS_ELICITED, RECEIVER_SETUPS_RECEIVED, RECEIVER_SETUPS_UNWANTED,
        RECEIVER_STATUS_MESSAGES_SENT, RECEIVER_UNDER_RUNS, RECORDING_STARTED, RECORDING_STOPPED,
    };
    use cavalry_shm::log::LogBuffers;
    use cavalry_shm::log::layout::NOT_ENDED;

    use super::recording::Harness;
    use super::*;

    const MS: i64 = 1_000_000;
    const SESSION: i32 = 55;
    const STREAM: i32 = 7;
    const INITIAL_TERM_ID: i32 = -5;
    /// The smallest term, so that an image's log is small in a test (ADR 0027).
    const TERM: u32 = MIN_TERM_LENGTH;

    /// A port no socket holds at the moment it is read: bound and released.
    fn free_port() -> u16 {
        UdpSocket::bind("127.0.0.1:0")
            .expect("a loopback socket")
            .local_addr()
            .expect("bound")
            .port()
    }

    /// A sender by hand: a socket of its own that sends setup frames, data and
    /// heartbeats to the receive endpoint and reads what the receiver answers.
    struct TestSender {
        socket: UdpSocket,
        buffer: Vec<u8>,
        target: SocketAddr,
        /// The term length its setup frames name and its control frames are parsed
        /// against: [`TERM`] unless a test sets another.
        term: u32,
    }

    impl TestSender {
        fn bind(port: u16) -> Self {
            let socket = UdpSocket::bind("127.0.0.1:0").expect("a loopback socket");
            socket
                .set_read_timeout(Some(Duration::from_millis(500)))
                .expect("a read timeout");
            Self {
                socket,
                buffer: vec![0u8; 2048],
                target: SocketAddr::from(([127, 0, 0, 1], port)),
                term: TERM,
            }
        }

        fn address(&self) -> SocketAddr {
            self.socket.local_addr().expect("bound")
        }

        fn send(&self, bytes: &[u8]) {
            self.socket
                .send_to(bytes, self.target)
                .expect("the datagram is sent");
        }

        fn send_setup(&self, session_id: i32, term_offset: i32) {
            self.send(&setup_with_term(session_id, STREAM, term_offset, self.term).to_bytes());
        }

        /// One datagram of 64-byte data frames, each with a 32-byte body, at
        /// consecutive offsets from `term_offset` of the initial term.
        fn send_frames(&self, session_id: i32, term_offset: u32, count: usize) {
            self.send(&frames(session_id, STREAM, term_offset, count));
        }

        /// A heartbeat at `term_offset` of the initial term.
        fn send_heartbeat(&self, session_id: i32, term_offset: i32, eos: bool) {
            self.send_heartbeat_in_term(session_id, INITIAL_TERM_ID, term_offset, eos);
        }

        /// A heartbeat at `term_offset` of the term `term_id`.
        fn send_heartbeat_in_term(
            &self,
            session_id: i32,
            term_id: i32,
            term_offset: i32,
            eos: bool,
        ) {
            let mut flags = Flags::UNFRAGMENTED.bits();
            if eos {
                flags |= Flags::EOS.bits();
            }
            self.send(
                &Heartbeat {
                    flags,
                    term_offset,
                    session_id,
                    stream_id: STREAM,
                    term_id,
                    reserved_value: 0,
                }
                .to_bytes(),
            );
        }

        /// The next control frame the receiver sent, or `None` at the read timeout.
        fn next_control(&mut self) -> Option<ControlFrame> {
            let (length, _) = self.socket.recv_from(&mut self.buffer).ok()?;
            parse_udp(&self.buffer[..length], self.term).ok()
        }

        /// The next status message, skipping anything else, or `None` at the timeout.
        fn next_status_message(&mut self) -> Option<StatusMessage> {
            loop {
                match self.next_control()? {
                    ControlFrame::StatusMessage(message) => return Some(message),
                    _ => continue,
                }
            }
        }

        /// The next status message that asks for a setup, skipping the image's own
        /// status messages and anything else, or `None` at the timeout.
        fn next_elicit(&mut self) -> Option<StatusMessage> {
            loop {
                match self.next_control()? {
                    ControlFrame::StatusMessage(message) if message.asks_for_setup() => {
                        return Some(message);
                    }
                    _ => continue,
                }
            }
        }

        /// The next NAK, skipping anything else, or `None` at the timeout.
        fn next_nak(&mut self) -> Option<cavalry_proto::udp::Nak> {
            loop {
                match self.next_control()? {
                    ControlFrame::Nak(nak) => return Some(nak),
                    _ => continue,
                }
            }
        }

        /// Takes everything waiting, so that the next read is of what follows.
        fn drain(&mut self) {
            self.socket
                .set_read_timeout(Some(Duration::from_millis(20)))
                .expect("a read timeout");
            while self.socket.recv_from(&mut self.buffer).is_ok() {}
            self.socket
                .set_read_timeout(Some(Duration::from_millis(500)))
                .expect("a read timeout");
        }
    }

    fn setup(session_id: i32, stream_id: i32, term_offset: i32) -> SetupFrame {
        setup_with_term(session_id, stream_id, term_offset, TERM)
    }

    /// A setup frame naming `term_length` as the stream's term.
    fn setup_with_term(
        session_id: i32,
        stream_id: i32,
        term_offset: i32,
        term_length: u32,
    ) -> SetupFrame {
        SetupFrame {
            flags: 0,
            term_offset,
            session_id,
            stream_id,
            initial_term_id: INITIAL_TERM_ID,
            active_term_id: INITIAL_TERM_ID,
            term_length: term_length as i32,
            mtu_length: 1408,
            ttl: 0,
        }
    }

    /// `count` 64-byte data frames from `term_offset` of the initial term.
    fn frames(session_id: i32, stream_id: i32, term_offset: u32, count: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(64 * count);
        for i in 0..count {
            let mut frame = vec![0x42u8; 64];
            DataFrameHeader::new(FrameType::Data, Flags::UNFRAGMENTED, 64)
                .expect("a header")
                .with_term_offset((term_offset + 64 * i as u32) as i32)
                .with_session_id(session_id)
                .with_stream_id(stream_id)
                .with_term_id(INITIAL_TERM_ID)
                .write(&mut frame)
                .expect("written");
            bytes.extend_from_slice(&frame);
        }
        bytes
    }

    /// The last `available image` for `subscription_id` among the events not yet read:
    /// (the image's registration id, the position counter id, the log file name, the
    /// source identity).
    fn last_available_image(
        h: &mut Harness,
        subscription_id: i64,
    ) -> Option<(i64, i32, String, String)> {
        let mut found = None;
        while h
            .receiver
            .receive(|type_id, message| {
                if let Ok(Event::AvailableImage {
                    correlation_id,
                    subscriber_registration_id,
                    subscriber_position_counter_id,
                    log_file_name,
                    source_identity,
                    ..
                }) = Event::parse(type_id, message)
                    && subscriber_registration_id == subscription_id
                {
                    found = Some((
                        correlation_id,
                        subscriber_position_counter_id,
                        String::from_utf8_lossy(log_file_name).into_owned(),
                        String::from_utf8_lossy(source_identity).into_owned(),
                    ));
                }
            })
            .expect("the receiver reads")
        {}
        found
    }

    /// The `unavailable image` events for `subscription_id` among the events not yet
    /// read, as the images' registration ids.
    fn unavailable_images(h: &mut Harness, subscription_id: i64) -> Vec<i64> {
        let mut found = Vec::new();
        while h
            .receiver
            .receive(|type_id, message| {
                if let Ok(Event::UnavailableImage {
                    correlation_id,
                    subscriber_registration_id,
                    ..
                }) = Event::parse(type_id, message)
                    && subscriber_registration_id == subscription_id
                {
                    found.push(correlation_id);
                }
            })
            .expect("the receiver reads")
        {}
        found
    }

    /// A control channel's publication reaches two subscriptions on two endpoints of
    /// this driver, each a destination (ADR 0053): an image per endpoint, each with the
    /// message, under `fc=min`, whose table holds the driver's one receiver id.
    #[test]
    fn a_control_channel_reaches_an_image_on_each_destination() {
        let mut h = Harness::new("udp-mdc-images");
        let ports = [free_port(), free_port()];
        for (i, port) in ports.iter().enumerate() {
            let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
            h.conductor
                .add_subscription(1, 2 + i as i64, STREAM, channel.as_bytes(), h.now);
            assert_eq!(h.last_error(2 + i as i64), None);
        }
        let control = format!(
            "cavalry:udp?control=127.0.0.1:{}|control-mode=manual|fc=min",
            free_port()
        );
        let now = h.now;
        h.conductor
            .add_publication(1, 4, STREAM, control.as_bytes(), false, now);
        assert_eq!(h.last_error(4), None);
        for (i, port) in ports.iter().enumerate() {
            let destination = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
            h.conductor
                .change_destination(1, 5 + i as i64, 4, destination.as_bytes(), true);
            assert_eq!(h.last_error(5 + i as i64), None);
        }
        let cycle_all = |h: &mut Harness, condition: &mut dyn FnMut(&mut Harness) -> bool| {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                h.sender.duty_cycle(h.now);
                h.cycle();
                if condition(h) {
                    return true;
                }
                if std::time::Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        assert!(
            cycle_all(&mut h, &mut |h| h.conductor.udp_images.len() == 2),
            "an image per destination"
        );
        assert!(
            cycle_all(&mut h, &mut |h| {
                h.conductor.publications.iter().any(|publication| {
                    publication.registration_id == 4
                        && publication.udp.as_ref().is_some_and(|udp| udp.connected)
                        && publication.limit.get() > 0
                })
            }),
            "connected by the destinations' status messages"
        );
        let images: Vec<i64> = h
            .conductor
            .udp_images
            .iter()
            .map(|image| image.registration_id)
            .collect();
        let index = h
            .conductor
            .publications
            .iter()
            .position(|publication| publication.registration_id == 4)
            .expect("the publication");
        let mut appender = h.conductor.publications[index]
            .log
            .appender()
            .expect("an appender");
        let limit = h.conductor.publications[index].limit.get();
        appender.offer(b"to both!", limit, 0).expect("offered");
        assert!(
            cycle_all(&mut h, &mut |h| {
                images
                    .iter()
                    .all(|&id| positions(h, id).map(|(_, rebuild, _)| rebuild) == Some(64))
            }),
            "both images rebuilt the frame"
        );
        let receivers_id = h.conductor.publications[index]
            .udp
            .as_ref()
            .expect("udp")
            .receivers_id;
        // One driver receives on both endpoints with one receiver id, so its two images
        // answer as one receiver of the table (`spec/udp.md`, "The receiver").
        assert_eq!(
            h.conductor
                .counters
                .counter(receivers_id)
                .expect("the gauge")
                .get(),
            1,
            "one receiver id for the driver's two images"
        );
        drop(appender);
    }

    /// The high-water mark, the rebuild position and the last status message's
    /// position of the image, as the receiver holds them.
    fn positions(h: &Harness, image_id: i64) -> Option<(i64, i64, i64)> {
        h.udp_receiver.positions_of(image_id)
    }

    /// The `client timeout` events among the events not yet read, as client ids.
    fn client_timeouts(h: &mut Harness) -> Vec<i64> {
        let mut found = Vec::new();
        while h
            .receiver
            .receive(|type_id, message| {
                if let Ok(Event::ClientTimeout { client_id }) = Event::parse(type_id, message) {
                    found.push(client_id);
                }
            })
            .expect("the receiver reads")
        {}
        found
    }

    /// Adds a subscription on a fresh port, gives the receiver its endpoint, sends the
    /// setup frame and cycles until the image exists and its first status message has
    /// arrived. Returns the sender, the image's registration id and the subscriber's
    /// position counter.
    fn subscribe_and_connect(h: &mut Harness, subscription_id: i64) -> (TestSender, i64, Counter) {
        subscribe_and_connect_with_term(h, subscription_id, TERM)
    }

    /// [`subscribe_and_connect`] with the stream's term length chosen: the image's log
    /// has that term, and the first status message's window follows it.
    fn subscribe_and_connect_with_term(
        h: &mut Harness,
        subscription_id: i64,
        term: u32,
    ) -> (TestSender, i64, Counter) {
        subscribe_and_connect_at(h, subscription_id, term, 0)
    }

    /// [`subscribe_and_connect_with_term`] with the setup frame's term offset chosen
    /// too: the image starts at that position of the initial term, and the first
    /// status message names it.
    fn subscribe_and_connect_at(
        h: &mut Harness,
        subscription_id: i64,
        term: u32,
        term_offset: i32,
    ) -> (TestSender, i64, Counter) {
        let port = free_port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        h.conductor
            .add_subscription(1, subscription_id, STREAM, channel.as_bytes(), h.now);
        assert_eq!(h.last_error(subscription_id), None);
        h.cycle();
        let mut sender = TestSender::bind(port);
        sender.term = term;
        sender.send_setup(SESSION, term_offset);
        assert!(
            h.cycle_until(|h| h.conductor.udp_images.len() == 1),
            "the setup frame created the image"
        );
        h.cycle();
        let (image_id, position_id, log_file_name, source_identity) =
            last_available_image(h, subscription_id).expect("an available image");
        assert_eq!(log_file_name, format!("images/{image_id}.logbuffer"));
        assert_eq!(source_identity, sender.address().to_string());
        let first = sender
            .next_status_message()
            .expect("the first status message");
        assert_eq!(
            (
                first.session_id,
                first.stream_id,
                first.consumption_term_id,
                first.consumption_term_offset
            ),
            (SESSION, STREAM, INITIAL_TERM_ID, term_offset)
        );
        assert_eq!(
            first.receiver_window_length,
            cavalry_proto::udp::receiver_window_length(term) as i32
        );
        assert_eq!(first.receiver_id, h.udp_receiver.receiver_id());
        let position = h
            .conductor
            .counters
            .counter(u32::try_from(position_id).expect("a counter id"))
            .expect("the subscriber's counter");
        (sender, image_id, position)
    }

    /// `add subscription` on a `cavalry:udp` channel binds the endpoint with its five
    /// counters and names `channel.status` in the event; a setup frame for the stream
    /// creates the image under `images/` with a registration id of its own, announced
    /// with the sender's address, and the receiver answers it with the first status
    /// message at once.
    #[test]
    fn a_udp_subscription_binds_its_endpoint_and_a_setup_frame_creates_its_image() {
        let mut h = Harness::new("udp-subscription");
        let allocated = h.counter(COUNTERS_ALLOCATED);
        let (sender, image_id, _position) = subscribe_and_connect(&mut h, 2);
        assert_eq!(h.conductor.receive_endpoints.len(), 1);
        assert_eq!(h.conductor.subscriptions.len(), 1);
        assert_eq!(h.counter(CHANNEL_OPENED), 1);
        assert_eq!(h.counter(IMAGE_CREATED), 1);
        assert_eq!(h.counter(RECEIVER_SETUPS_RECEIVED), 1);
        assert_eq!(h.conductor.udp_images.len(), 1);
        assert_eq!(h.conductor.udp_pairs, 1);
        let endpoint = &h.conductor.receive_endpoints[0];
        assert_eq!(endpoint.status.get(), 1);
        assert!(
            endpoint.socket_rcvbuf.get() > 0,
            "the receive buffer was read back"
        );
        assert_eq!(endpoint.images.get(), 1);
        assert_eq!(endpoint.publications.get(), 0);
        assert!(
            h.dir
                .path()
                .join(format!("images/{image_id}.logbuffer"))
                .is_file()
        );
        // Five for the endpoint, two for the image, one for the subscriber.
        assert_eq!(h.counter(COUNTERS_ALLOCATED), allocated + 8);
        drop(sender);
    }

    /// Every event the broadcast buffer holds, drained: (type id, message).
    fn drained_events(h: &mut Harness) -> Vec<(i32, Vec<u8>)> {
        let mut out = Vec::new();
        while h
            .receiver
            .receive(|type_id, message| out.push((type_id, message.to_vec())))
            .expect("the receiver reads")
        {}
        out
    }

    /// `start recording` on a subscription with no image arms the recording: operation
    /// success at once, code 22 for a second start meanwhile, and when the setup frame
    /// arrives the recording starts at the image's join position before the image is
    /// announced, with recording ready under the arming command's correlation id (ADR
    /// 0059).
    #[test]
    fn an_armed_recording_starts_at_the_images_join_position_and_answers_twice() {
        let mut h = Harness::new("armed-recording");
        let port = free_port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        h.conductor
            .add_subscription(1, 2, STREAM, channel.as_bytes(), h.now);
        assert_eq!(h.last_error(2), None);
        h.cycle();
        let now = h.now;
        h.conductor
            .start_recording(3, STREAM, channel.as_bytes(), now);
        assert!(
            drained_events(&mut h)
                .iter()
                .any(|(type_id, message)| matches!(
                    Event::parse(*type_id, message),
                    Ok(Event::OperationSuccess { correlation_id: 3 })
                )),
            "operation success answers the arming"
        );
        assert_eq!(h.counter(RECORDING_ARMED), 1);
        assert_eq!(h.conductor.armed_recordings.len(), 1);
        h.conductor
            .start_recording(4, STREAM, channel.as_bytes(), now);
        let (code, message) = h.last_error(4).expect("an error");
        assert_eq!(code, 22, "{message}");
        assert!(message.contains("armed"), "{message}");
        let mut sender = TestSender::bind(port);
        sender.term = TERM;
        sender.send_setup(SESSION, 4096);
        assert!(
            h.cycle_until(|h| h.conductor.udp_images.len() == 1),
            "the setup frame created the image"
        );
        assert!(h.conductor.armed_recordings.is_empty());
        assert_eq!(h.conductor.recordings.len(), 1);
        assert_eq!(h.conductor.recordings[0].start_position, 4096);
        // Handed to the recorder is not started: the count waits for recording ready.
        assert_eq!(h.conductor.recordings[0].state, RecordingState::Starting);
        assert_eq!(h.counter(RECORDING_ARMED_STARTED), 0);
        assert!(
            h.cycle_until(|h| h.conductor.recordings[0].state == RecordingState::Live),
            "the recorder took the start"
        );
        assert_eq!(h.counter(RECORDING_ARMED_STARTED), 1);
        assert_eq!(h.counter(RECORDING_ARMED_FAILED), 0);
        let all = drained_events(&mut h);
        let ready =
            all.iter()
                .find_map(|(type_id, message)| match Event::parse(*type_id, message) {
                    Ok(Event::RecordingReady {
                        correlation_id,
                        recording_id,
                        ..
                    }) => Some((correlation_id, recording_id)),
                    _ => None,
                });
        assert_eq!(ready, Some((3, 0)), "recording ready under the arming id");
        let announced = all.iter().position(|(type_id, message)| {
            matches!(
                Event::parse(*type_id, message),
                Ok(Event::AvailableImage { .. })
            )
        });
        assert!(announced.is_some(), "the image was announced");
        drop(sender);
    }

    /// An armed recording is dropped with its stream's last subscription on the
    /// endpoint, answered with code 20 under the arming command's id, while another
    /// stream's subscription keeps the endpoint open (ADR 0059).
    #[test]
    fn an_armed_recording_is_dropped_with_its_streams_last_subscription() {
        let mut h = Harness::new("armed-recording-dropped");
        let port = free_port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        h.conductor
            .add_subscription(1, 2, STREAM, channel.as_bytes(), h.now);
        h.conductor
            .add_subscription(1, 3, STREAM + 1, channel.as_bytes(), h.now);
        h.cycle();
        let now = h.now;
        h.conductor
            .start_recording(4, STREAM, channel.as_bytes(), now);
        assert_eq!(h.last_error(4), None);
        assert_eq!(h.conductor.armed_recordings.len(), 1);
        // A second subscription on the stream keeps it armed; the last one drops it.
        h.conductor
            .add_subscription(1, 5, STREAM, channel.as_bytes(), h.now);
        h.conductor.remove_subscription(1, 6, 2, now);
        assert_eq!(h.conductor.armed_recordings.len(), 1);
        h.conductor.remove_subscription(1, 7, 5, now);
        assert!(h.conductor.armed_recordings.is_empty());
        let (code, message) = h.last_error(4).expect("the second answer");
        assert_eq!(code, 20, "{message}");
        assert!(message.contains("dropped"), "{message}");
        assert_eq!(h.counter(RECORDING_ARMED_DROPPED), 1);
        assert_eq!(
            h.conductor.receive_endpoints.len(),
            1,
            "the other stream keeps the endpoint"
        );
    }

    /// A second start while a recording is armed is code 22 even with a publication
    /// sending to the endpoint on the stream, which is not recorded in the armed
    /// recording's place (`spec/client-protocol.md`, "Commands", start recording; ADR
    /// 0059).
    #[test]
    fn a_second_start_while_armed_is_code_22_beside_a_publication_on_the_stream() {
        let mut h = Harness::new("armed-recording-publication");
        let port = subscribe_and_arm(&mut h, 3);
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        let now = h.now;
        h.conductor
            .add_publication(1, 4, STREAM, channel.as_bytes(), false, now);
        assert_eq!(h.last_error(4), None);
        h.conductor
            .start_recording(5, STREAM, channel.as_bytes(), now);
        let (code, message) = h.last_error(5).expect("an error");
        assert_eq!(code, 22, "{message}");
        assert!(message.contains("armed"), "{message}");
        assert!(
            h.conductor.recordings.is_empty(),
            "the publication is not recorded"
        );
        assert_eq!(h.conductor.armed_recordings.len(), 1);
    }

    /// The armed recordings at capacity (invariant 3): the subscriptions registry full,
    /// one stream each on one endpoint, and every stream armed, the table holds
    /// exactly the registry's capacity, not past its allocation, and
    /// `recording.armed` reads the same; the next `add subscription` is code 10, so no
    /// stream gets a subscription to arm on and a start on it is code 20; and one
    /// subscription removed drops exactly its armed recording, with code 20 under its
    /// arming id (ADR 0059).
    #[test]
    fn the_armed_recordings_fill_at_the_subscriptions_registry_capacity() {
        let mut h = Harness::new("armed-recordings-capacity");
        let port = free_port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        let n = spec::MAX_SUBSCRIPTIONS as i32;
        let allocated = h.conductor.armed_recordings.capacity();
        for i in 0..n {
            h.conductor.add_subscription(
                1,
                10_000 + i64::from(i),
                STREAM + i,
                channel.as_bytes(),
                h.now,
            );
            // The receiver takes the interest, so its queue has room for the next add.
            let now = h.now;
            h.udp_receiver.duty_cycle(now);
        }
        assert_eq!(h.conductor.subscriptions.len(), spec::MAX_SUBSCRIPTIONS);
        assert_eq!(h.conductor.receive_endpoints.len(), 1);
        let now = h.now;
        for i in 0..n {
            h.conductor
                .start_recording(20_000 + i64::from(i), STREAM + i, channel.as_bytes(), now);
        }
        let events = drained_events(&mut h);
        let armed = events
            .iter()
            .filter(|(type_id, message)| {
                matches!(
                    Event::parse(*type_id, message),
                    Ok(Event::OperationSuccess { correlation_id })
                        if (20_000..20_000 + i64::from(n)).contains(&correlation_id)
                )
            })
            .count();
        assert_eq!(armed, spec::MAX_SUBSCRIPTIONS, "every start was armed");
        assert_eq!(h.conductor.armed_recordings.len(), spec::MAX_SUBSCRIPTIONS);
        assert_eq!(
            h.conductor.armed_recordings.capacity(),
            allocated,
            "the table did not grow"
        );
        assert_eq!(h.counter(RECORDING_ARMED), i64::from(n));

        // The next add is refused, so its stream has nothing to arm on.
        h.conductor
            .add_subscription(1, 30_000, STREAM + n, channel.as_bytes(), h.now);
        let (code, message) = h.last_error(30_000).expect("one more is refused");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("subscriptions"), "{message}");
        h.conductor
            .start_recording(30_001, STREAM + n, channel.as_bytes(), now);
        let (code, message) = h.last_error(30_001).expect("nothing to arm on");
        assert_eq!(code, 20, "{message}");
        assert_eq!(h.conductor.armed_recordings.len(), spec::MAX_SUBSCRIPTIONS);
        assert_eq!(h.counter(RECORDING_ARMED), i64::from(n));

        // One subscription goes: exactly its stream's armed recording is dropped.
        h.conductor.remove_subscription(1, 40_000, 10_005, now);
        assert_eq!(
            h.conductor.armed_recordings.len(),
            spec::MAX_SUBSCRIPTIONS - 1
        );
        assert!(
            !h.conductor
                .armed_recordings
                .iter()
                .any(|armed| armed.stream_id == STREAM + 5)
        );
        let errors: Vec<(i64, i32)> = drained_events(&mut h)
            .iter()
            .filter_map(|(type_id, message)| match Event::parse(*type_id, message) {
                Ok(Event::Error {
                    offending_correlation_id,
                    error_code,
                    ..
                }) => Some((offending_correlation_id, error_code)),
                _ => None,
            })
            .collect();
        assert_eq!(errors, vec![(20_005, 20)], "the one armed id, code 20");
        assert_eq!(h.counter(RECORDING_ARMED_DROPPED), 1);
    }

    /// Subscribes on a fresh endpoint and arms a recording on [`STREAM`] under
    /// `correlation_id`, answered with operation success; the endpoint's port.
    fn subscribe_and_arm(h: &mut Harness, correlation_id: i64) -> u16 {
        let port = free_port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        h.conductor
            .add_subscription(1, 2, STREAM, channel.as_bytes(), h.now);
        assert_eq!(h.last_error(2), None);
        h.cycle();
        let now = h.now;
        h.conductor
            .start_recording(correlation_id, STREAM, channel.as_bytes(), now);
        assert!(
            drained_events(h).iter().any(|(type_id, message)| matches!(
                Event::parse(*type_id, message),
                Ok(Event::OperationSuccess { correlation_id: id }) if id == correlation_id
            )),
            "operation success answers the arming"
        );
        assert_eq!(h.conductor.armed_recordings.len(), 1);
        port
    }

    /// Sends the setup frame at 4096 to `port` and drives the receiver and the
    /// conductor alone, the recorder never, until the image exists: the armed start
    /// has been refused or handed to the recorder, and the recorder has not answered.
    fn join_without_the_recorder(h: &mut Harness, port: u16) -> TestSender {
        let mut sender = TestSender::bind(port);
        sender.term = TERM;
        sender.send_setup(SESSION, 4096);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while h.conductor.udp_images.is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "the setup frame created the image"
            );
            h.now += MS;
            let now = h.now;
            h.udp_receiver.duty_cycle(now);
            h.conductor.duty_cycle(now);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(h.conductor.armed_recordings.is_empty());
        sender
    }

    /// The arming command's second answer among `events`: (code, message) of an
    /// `error` under `correlation_id`, and whether a `recording ready` named it too.
    fn second_answer(
        events: &[(i32, Vec<u8>)],
        correlation_id: i64,
    ) -> (Option<(i32, String)>, bool) {
        let mut error = None;
        let mut ready = false;
        for (type_id, message) in events {
            match Event::parse(*type_id, message) {
                Ok(Event::Error {
                    offending_correlation_id,
                    error_code,
                    error_message,
                }) if offending_correlation_id == correlation_id => {
                    error = Some((
                        error_code,
                        String::from_utf8_lossy(error_message).into_owned(),
                    ));
                }
                Ok(Event::RecordingReady {
                    correlation_id: id, ..
                }) if id == correlation_id => ready = true,
                _ => {}
            }
        }
        (error, ready)
    }

    /// Whether `events` hold an `available image` for `image_id`.
    fn announced(events: &[(i32, Vec<u8>)], image_id: i64) -> bool {
        events.iter().any(|(type_id, message)| {
            matches!(
                Event::parse(*type_id, message),
                Ok(Event::AvailableImage { correlation_id, .. }) if correlation_id == image_id
            )
        })
    }

    /// An armed recording whose start the recorder refuses, here a full catalog at the
    /// seam, is answered a second time with code 10 under the arming command's id,
    /// once, and counted in `recording.armed_failed` and `recording.catalog_full`,
    /// never in `recording.armed_started`; the image is announced all the same, and
    /// the recording's entry and its reader on the image go (`spec/client-protocol.md`,
    /// "Commands", start recording; ADR 0059).
    #[test]
    fn an_armed_recording_the_recorder_refuses_is_answered_and_counted_as_failed() {
        let mut h = Harness::new("armed-recording-refused");
        let port = subscribe_and_arm(&mut h, 3);
        let sender = join_without_the_recorder(&mut h, port);
        let image_id = h.conductor.udp_images[0].registration_id;
        assert_eq!(h.conductor.recordings.len(), 1);
        assert_eq!(h.conductor.recordings[0].state, RecordingState::Starting);
        assert!(h.conductor.recordings[0].armed);
        // The receiver takes the image with the subscription's reader and the
        // recording's.
        h.now += MS;
        let now = h.now;
        h.udp_receiver.duty_cycle(now);
        assert_eq!(h.udp_receiver.reader_count(), 2);

        h.conductor
            .on_start_failed(image_id, 3, StartFailure::CatalogFull);
        let events = drained_events(&mut h);
        let (error, ready) = second_answer(&events, 3);
        let (code, message) = error.expect("an error under the arming id");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("catalog"), "{message}");
        assert!(!ready, "the second answer is the error alone");
        assert_eq!(h.counter(RECORDING_ARMED), 1);
        assert_eq!(h.counter(RECORDING_ARMED_FAILED), 1);
        assert_eq!(h.counter(RECORDING_ARMED_STARTED), 0);
        assert_eq!(h.counter(RECORDING_CATALOG_FULL), 1);
        assert!(
            h.conductor.recordings.is_empty(),
            "the entry failed and went"
        );
        assert!(
            announced(&events, image_id),
            "the image was announced all the same"
        );
        // The recording's reader leaves the image; the subscription's stays.
        h.now += MS;
        let now = h.now;
        h.udp_receiver.duty_cycle(now);
        assert_eq!(h.udp_receiver.reader_count(), 1);
        drop(sender);
    }

    /// An armed recording refused when its image arrives, before the recorder sees
    /// it, here behind a request still waiting for room in the recorder's queue, is
    /// answered a second time with code 10 under the arming command's id and counted
    /// in `recording.armed_failed` and `driver.registry_full`; nothing is registered
    /// and no reader added, and the image is announced all the same (ADR 0059).
    #[test]
    fn an_armed_recording_refused_when_its_image_arrives_is_counted_as_failed() {
        let mut h = Harness::new("armed-recording-queue-full");
        let port = subscribe_and_arm(&mut h, 3);
        // The recorder's request queue full of stops of ids no recording holds, and
        // one more waiting behind them.
        while h
            .conductor
            .recorder
            .requests
            .offer(Request::Stop {
                recording_id: 1_000_000,
                correlation_id: None,
            })
            .is_ok()
        {}
        h.conductor.deferred.push_back(Request::Stop {
            recording_id: 1_000_001,
            correlation_id: None,
        });
        let registry_full = h.counter(DRIVER_REGISTRY_FULL);
        let sender = join_without_the_recorder(&mut h, port);
        let image_id = h.conductor.udp_images[0].registration_id;
        let events = drained_events(&mut h);
        let (error, ready) = second_answer(&events, 3);
        let (code, message) = error.expect("an error under the arming id");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("request queue"), "{message}");
        assert!(!ready, "the second answer is the error alone");
        assert_eq!(h.counter(RECORDING_ARMED_FAILED), 1);
        assert_eq!(h.counter(RECORDING_ARMED_STARTED), 0);
        assert_eq!(h.counter(DRIVER_REGISTRY_FULL), registry_full + 1);
        assert!(h.conductor.recordings.is_empty(), "nothing registered");
        assert!(
            announced(&events, image_id),
            "the image was announced all the same"
        );
        h.now += MS;
        let now = h.now;
        h.udp_receiver.duty_cycle(now);
        assert_eq!(
            h.udp_receiver.reader_count(),
            1,
            "the subscription's reader alone"
        );
        drop(sender);
    }

    /// Frames land in the image at their term offsets; one lost on the wire leaves a
    /// gap the rebuild stops at, which a NAK names after the NAK delay and the
    /// retransmit fills, after which a reader has every frame once and in order; a
    /// duplicate changes nothing (invariant 1).
    #[test]
    fn frames_fill_the_image_in_order_and_a_gap_is_naked_and_filled() {
        let mut h = Harness::new("udp-gap");
        let (mut sender, image_id, position) = subscribe_and_connect(&mut h, 2);
        // Frames at 0 and 64 arrive, the one at 128 is lost, the one at 192 arrives.
        sender.send_frames(SESSION, 0, 2);
        sender.send_frames(SESSION, 192, 1);
        assert!(h.cycle_until(|h| positions(h, image_id) == Some((256, 128, 0))));
        assert_eq!(h.counter(RECEIVER_FRAMES_RECEIVED), 3);
        assert_eq!(h.counter(RECEIVER_GAPS_DETECTED), 1);
        // The NAK, after the delay: a cycle is a millisecond on.
        h.cycle();
        let nak = sender.next_nak().expect("a NAK");
        assert_eq!(
            (
                nak.session_id,
                nak.stream_id,
                nak.term_id,
                nak.term_offset,
                nak.length
            ),
            (SESSION, STREAM, INITIAL_TERM_ID, 128, 64)
        );
        assert!(h.counter(RECEIVER_NAKS_SENT) >= 1);
        // The retransmit: the rebuild reaches the high-water mark and the gap is gone.
        sender.send_frames(SESSION, 128, 1);
        assert!(h.cycle_until(|h| positions(h, image_id) == Some((256, 256, 0))));
        assert_eq!(h.counter(RECEIVER_FRAMES_RECEIVED), 4);
        let image = h
            .conductor
            .udp_images
            .iter()
            .find(|image| image.registration_id == image_id)
            .expect("the image");
        assert_eq!(image.rebuild.get(), 256);
        assert_eq!(image.hwm.get(), 256);
        // A subscriber reads the four frames once and in order.
        let log = LogBuffers::open_image(h.dir.path(), image_id, false).expect("the image opens");
        let mut reader = log.reader(0).expect("a reader");
        let mut offsets = Vec::new();
        let _ = reader.poll(16, |_body, header| {
            offsets.push(header.term_offset());
        });
        assert_eq!(offsets, vec![0, 64, 128, 192]);
        // A duplicate changes nothing: the count stays.
        sender.send_frames(SESSION, 64, 1);
        h.settle();
        assert_eq!(h.counter(RECEIVER_FRAMES_RECEIVED), 4);
        // The subscriber's position drives the status message's consumption position,
        // on the interval trigger.
        position.store_release(256);
        sender.drain();
        h.now += 200 * MS;
        h.cycle();
        let message = sender.next_status_message().expect("a status message");
        assert_eq!(message.consumption_term_offset, 256);
        assert!(h.counter(RECEIVER_STATUS_MESSAGES_SENT) >= 2);
        // Below the last status message's position is an under-run; beyond it plus
        // half a term is an over-run; neither is delivered.
        sender.send_frames(SESSION, 0, 1);
        sender.send_frames(SESSION, 256 + TERM / 2, 1);
        assert!(h.cycle_until(|h| {
            h.counter(RECEIVER_UNDER_RUNS) == 1 && h.counter(RECEIVER_OVER_RUNS) == 1
        }));
        assert_eq!(h.counter(RECEIVER_FRAMES_RECEIVED), 4);
    }

    /// A gap longer than the scan bound is reported as the bound on the cycle that finds
    /// it; the next cycle scans on from where the last stopped, within the same bound,
    /// and finds the frame beyond, so that the NAK that follows names the gap whole, and
    /// the retransmit closes it (`spec/udp.md`, "The receiver", step 3; ADR 0042).
    #[test]
    fn a_gap_longer_than_the_scan_bound_is_walked_on_from_where_the_last_scan_stopped() {
        const BIG_TERM: u32 = 4 * 1024 * 1024;
        let limit = i64::from(spec::REBUILD_SCAN_LIMIT);
        let mut h = Harness::new("udp-long-gap");
        let (mut sender, image_id, _position) =
            subscribe_and_connect_with_term(&mut h, 2, BIG_TERM);
        sender.send_frames(SESSION, 0, 2);
        assert!(h.cycle_until(|h| positions(h, image_id) == Some((128, 128, 0))));
        assert_eq!(h.udp_receiver.gap_of(image_id), None);
        // A frame beyond the bound from the rebuild position: the gap is longer than
        // the bound, and the scan that finds it stops at the bound inside it.
        let beyond = 128 + limit + 64;
        sender.send_frames(SESSION, beyond as u32, 1);
        assert!(h.cycle_until(|h| positions(h, image_id) == Some((beyond + 64, 128, 0))));
        assert_eq!(
            h.udp_receiver.gap_of(image_id),
            Some((128, limit)),
            "the first scan's gap is the bound"
        );
        assert_eq!(h.counter(RECEIVER_GAPS_DETECTED), 1);
        sender.drain();
        // The next cycle scans on from the bound and finds the frame beyond.
        h.cycle();
        assert_eq!(
            h.udp_receiver.gap_of(image_id),
            Some((128, limit + 64)),
            "the gap ends at the frame beyond the bound"
        );
        assert_eq!(
            h.counter(RECEIVER_GAPS_DETECTED),
            1,
            "the same gap, known further"
        );
        // The NAK, after the delay, names the gap whole.
        h.cycle();
        let nak = sender.next_nak().expect("a NAK");
        assert_eq!(
            (nak.term_offset, nak.length),
            (128, (limit + 64) as i32),
            "the NAK names the gap whole"
        );
        // The retransmit, a datagram per cycle of at most 128 frames, 8192 bytes: below
        // macOS's default of 9216 bytes for a datagram (`net.inet.udp.maxdgram`,
        // docs/how-to/run-the-driver.md), so that the test reads the same on every
        // platform and needs no sysctl. The rebuild reaches the high-water mark and
        // the gap is gone.
        let mut offset = 128;
        while offset < beyond {
            let count = usize::try_from((beyond - offset) / 64)
                .expect("a count")
                .min(128);
            sender.send_frames(SESSION, offset as u32, count);
            offset += 64 * count as i64;
            assert!(h.cycle_until(|h| {
                positions(h, image_id).is_some_and(|(_, rebuild, _)| rebuild >= offset)
            }));
        }
        assert!(h.cycle_until(|h| positions(h, image_id) == Some((beyond + 64, beyond + 64, 0))));
        assert_eq!(h.udp_receiver.gap_of(image_id), None);
        let log = LogBuffers::open_image(h.dir.path(), image_id, false).expect("the image opens");
        let mut reader = log.reader(0).expect("a reader");
        let mut frames = 0;
        loop {
            let before = reader.position();
            let _ = reader.poll(1024, |_body, _header| frames += 1);
            if reader.position() == before {
                break;
            }
        }
        assert_eq!(frames, (beyond + 64) / 64, "every frame once, in order");
    }

    /// A heartbeat moves the high-water mark; one with EOS names the end of stream,
    /// stored once the rebuild reaches it, after which the status messages carry the
    /// flag and the image is retired once the subscriber has reached the end: the
    /// subscription gets `unavailable image`, the log goes, the record is on cool-down.
    #[test]
    fn a_heartbeat_with_eos_ends_the_image_and_the_subscriber_reaching_it_retires_it() {
        let mut h = Harness::new("udp-eos");
        let (mut sender, image_id, position) = subscribe_and_connect(&mut h, 2);
        sender.send_frames(SESSION, 0, 4);
        sender.send_heartbeat(SESSION, 256, false);
        assert!(h.cycle_until(|h| h.counter(RECEIVER_HEARTBEATS_RECEIVED) == 1));
        assert!(h.cycle_until(|h| positions(h, image_id) == Some((256, 256, 0))));
        sender.send_heartbeat(SESSION, 256, true);
        let log = LogBuffers::open_image(h.dir.path(), image_id, false).expect("the image opens");
        assert!(
            h.cycle_until(|_| log.end_of_stream_position() == 256),
            "the end of stream is stored"
        );
        sender.drain();
        h.now += 200 * MS;
        h.cycle();
        let message = sender.next_status_message().expect("a status message");
        assert!(
            message.is_end_of_stream(),
            "the flag is set once the stream ended"
        );
        assert_eq!(
            h.conductor.udp_images.len(),
            1,
            "retired only once the subscriber reaches the end"
        );
        // The subscriber reaches the end: retired as ended, removed.
        position.store_release(256);
        assert!(h.cycle_until(|h| h.counter(IMAGE_REMOVED) == 1));
        assert_eq!(h.counter(IMAGE_ENDED), 1);
        assert_eq!(unavailable_images(&mut h, 2), vec![image_id]);
        assert!(h.conductor.udp_images.is_empty());
        assert_eq!(h.conductor.udp_pairs, 0);
        assert!(
            !h.dir
                .path()
                .join(format!("images/{image_id}.logbuffer"))
                .exists(),
            "the log is unlinked"
        );
        drop(log);
        // The session is on cool-down: a setup for it creates no second image.
        sender.send_setup(SESSION, 256);
        assert!(h.cycle_until(|h| h.counter(RECEIVER_DROPPED_COOL_DOWN) == 1));
        h.settle();
        assert!(h.conductor.udp_images.is_empty());
        // The cool-down ends: a setup creates a new image with an id of its own.
        h.now += spec::nanos(spec::IMAGE_COOL_DOWN) + 200 * MS;
        h.cycle();
        sender.send_setup(SESSION, 256);
        assert!(h.cycle_until(|h| h.counter(IMAGE_CREATED) == 2));
        let (second, _, _, _) = last_available_image(&mut h, 2).expect("a second image");
        assert_ne!(second, image_id);
        // The endpoint stayed open through it all: one subscription holds it.
        assert_eq!(h.counter(CHANNEL_CLOSED), 0);
    }

    /// An image over UDP that is retired with a subscription remaining, less than the
    /// counter reuse timeout after its `available image`, tells the subscription at
    /// once and keeps its log and the subscription's position record until that time
    /// has passed, so that a client whose conductor has not read the announcement yet
    /// finds both (`spec/udp.md`, "The receiver", step 5; ADR 0078).
    #[test]
    fn an_image_retired_soon_after_its_announcement_keeps_its_log_and_records_for_the_reuse_timeout()
     {
        let mut h = Harness::new("udp-announced");
        let allocated = h.counter(COUNTERS_ALLOCATED);
        let (sender, image_id, position) = subscribe_and_connect(&mut h, 2);
        let announced = h.conductor.udp_images[0].subscriptions[0].announced;
        let record = position.id();
        let log = h.dir.path().join(format!("images/{image_id}.logbuffer"));
        // The stream ends where it began, and the subscriber stands there already.
        sender.send_heartbeat(SESSION, 0, true);
        assert!(
            h.cycle_until(|h| h.counter(IMAGE_ENDED) == 1),
            "the receiver retired the image"
        );
        let wait = spec::nanos(spec::COUNTER_REUSE_TIMEOUT);
        assert!(
            h.now < announced + wait,
            "the test's clock is inside the wait"
        );
        assert_eq!(
            unavailable_images(&mut h, 2),
            vec![image_id],
            "the subscription is told at once"
        );
        assert_eq!(h.counter(IMAGE_REMOVED), 0);
        assert_eq!(h.conductor.udp_images.len(), 1);
        assert_eq!(h.conductor.udp_waiting, 1);
        assert_eq!(h.conductor.udp_pairs, 1, "the pair is held with its record");
        assert!(h.conductor.counters.counter(record).is_ok());
        assert!(log.exists(), "the log is still there to open");
        // One cycle short of the time, nothing has gone; a cycle adds a millisecond.
        h.now = announced + wait - 2 * MS;
        h.cycle();
        assert_eq!(h.counter(IMAGE_REMOVED), 0);
        assert!(log.exists());
        // The time has passed: the record is freed, the log unlinked, the image removed,
        // and no second `unavailable image` goes.
        h.cycle();
        assert_eq!(h.counter(IMAGE_REMOVED), 1);
        assert!(h.conductor.udp_images.is_empty());
        assert_eq!((h.conductor.udp_pairs, h.conductor.udp_waiting), (0, 0));
        assert!(h.conductor.counters.counter(record).is_err());
        assert!(!log.exists());
        assert!(unavailable_images(&mut h, 2).is_empty());
        // The subscription and its endpoint stay; removing it frees every record.
        let now = h.now;
        h.conductor.remove_subscription(1, 3, 2, now);
        assert!(h.cycle_until(|h| h.conductor.receive_endpoints.is_empty()));
        assert_eq!(h.counter(COUNTERS_ALLOCATED), allocated);
        drop(sender);
    }

    /// A subscription removed while its retired image waits for it: its record is freed
    /// with the removal, as a removed subscription's always is, and the image, which
    /// now waits for nobody, is removed on the next duty cycle, before the time has
    /// passed. Its client knows it left (ADR 0078).
    #[test]
    fn a_subscription_removed_while_its_retired_image_waits_frees_its_record_and_the_image() {
        let mut h = Harness::new("udp-announced-left");
        let allocated = h.counter(COUNTERS_ALLOCATED);
        let (sender, image_id, position) = subscribe_and_connect(&mut h, 2);
        let announced = h.conductor.udp_images[0].subscriptions[0].announced;
        let record = position.id();
        sender.send_heartbeat(SESSION, 0, true);
        assert!(h.cycle_until(|h| h.counter(IMAGE_ENDED) == 1));
        assert_eq!(
            h.conductor.udp_waiting, 1,
            "the image waits for its subscriber"
        );
        let now = h.now;
        assert!(now < announced + spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
        h.conductor.remove_subscription(1, 3, 2, now);
        assert!(
            h.conductor.counters.counter(record).is_err(),
            "freed with the removal"
        );
        assert_eq!(h.conductor.udp_pairs, 0);
        assert_eq!(
            h.counter(IMAGE_REMOVED),
            0,
            "the image goes on the next cycle"
        );
        assert!(h.cycle_until(|h| h.conductor.receive_endpoints.is_empty()));
        assert!(
            h.now < announced + spec::nanos(spec::COUNTER_REUSE_TIMEOUT),
            "before the time has passed"
        );
        assert_eq!(h.counter(IMAGE_REMOVED), 1);
        assert_eq!(h.conductor.udp_waiting, 0);
        assert!(
            !h.dir
                .path()
                .join(format!("images/{image_id}.logbuffer"))
                .exists()
        );
        assert_eq!(h.counter(COUNTERS_ALLOCATED), allocated);
        drop(sender);
    }

    /// A publication added and removed with nothing published, beside a subscriber: the
    /// subscriber's image stands at the end from the start, and before ADR 0078 the log
    /// and the image's position counter went in the same duty cycle, ahead of the
    /// client's conductor. `unavailable image` still goes in that cycle; the log and
    /// the counter stay for the counter reuse timeout after the `available image`. A
    /// subscriber that leaves makes nothing wait, since its client knows,
    /// and neither does one that has read from the image, since its position shows
    /// that its client has the log and the counter.
    #[test]
    fn a_publication_closed_soon_after_an_image_was_announced_keeps_its_log_for_the_reuse_timeout()
    {
        let mut h = Harness::new("ipc-announced");
        let wait = spec::nanos(spec::COUNTER_REUSE_TIMEOUT);
        let now = h.now;
        h.conductor.add_subscription(1, 2, STREAM, IPC_CHANNEL, now);
        h.conductor
            .add_publication(1, 3, STREAM, IPC_CHANNEL, false, now);
        assert_eq!(h.conductor.images.len(), 1, "the subscriber has its image");
        let (record, announced) = (
            h.conductor.images[0].position.id(),
            h.conductor.images[0].announced,
        );
        assert_eq!(announced, now);
        let log = h.dir.path().join("publications/3.logbuffer");
        h.conductor.remove_publication(1, 4, 3, now);
        h.cycle();
        assert_eq!(
            h.conductor.publications.len(),
            1,
            "closed, drained, and waiting"
        );
        assert!(h.conductor.publications[0].closing.is_some());
        assert!(log.exists());
        assert!(h.conductor.counters.counter(record).is_ok());
        assert_eq!(
            unavailable_images(&mut h, 2),
            vec![3],
            "the subscriber hears the image go at once"
        );
        h.now = announced + wait - 2 * MS;
        h.cycle();
        assert_eq!(h.conductor.publications.len(), 1, "one cycle short");
        assert!(log.exists());
        h.cycle();
        assert!(h.conductor.publications.is_empty());
        assert!(h.conductor.images.is_empty());
        assert!(!log.exists());
        assert!(h.conductor.counters.counter(record).is_err());
        assert!(
            unavailable_images(&mut h, 2).is_empty(),
            "and hears it once"
        );

        // The same again, and the subscriber leaves first: the close waits for nothing.
        let now = h.now;
        h.conductor
            .add_publication(1, 5, STREAM, IPC_CHANNEL, false, now);
        assert_eq!(h.conductor.images.len(), 1);
        h.conductor.remove_subscription(1, 6, 2, now);
        h.conductor.remove_publication(1, 7, 5, now);
        h.cycle();
        assert!(h.conductor.publications.is_empty());
        assert!(!h.dir.path().join("publications/5.logbuffer").exists());

        // And a subscriber that has read a frame: its position moved from where it
        // joined, so the close finds every subscriber at the end and waits for nothing.
        let now = h.now;
        h.conductor.add_subscription(1, 8, STREAM, IPC_CHANNEL, now);
        h.conductor
            .add_publication(1, 9, STREAM, IPC_CHANNEL, false, now);
        h.cycle();
        let limit = h.conductor.publications[0].limit.get();
        let mut appender = h.conductor.publications[0]
            .log
            .appender()
            .expect("an appender");
        appender.offer(&[7u8; 32], limit, 0).expect("a frame");
        let end = appender.position().expect("the position");
        assert_eq!(end, 64, "one frame of 32 bytes and its header");
        h.conductor.images[0].position.store_release(end);
        let now = h.now;
        h.conductor.remove_publication(1, 10, 9, now);
        h.cycle();
        assert!(h.conductor.publications.is_empty());
        assert!(!h.dir.path().join("publications/9.logbuffer").exists());
        assert_eq!(unavailable_images(&mut h, 8), vec![9]);
    }

    /// A heartbeat's term offset is checked against its image's term, not the driver's:
    /// an image whose setup frame named a term longer than the driver's 16 MiB keeps
    /// the heartbeats past 16 MiB that a sender idle there sends, and one whose term
    /// offset is not below the image's term is dropped and counted in
    /// `receiver.dropped_frames` (`spec/udp.md`, "What a reader rejects",
    /// `term-offset`). One that over-runs the window is dropped before
    /// `receiver.heartbeats_received` counts it, as the two counters' docs say.
    #[test]
    fn a_heartbeat_is_checked_against_the_image_term_not_the_driver_term() {
        const BIG_TERM: u32 = 32 * 1024 * 1024;
        const START: i32 = 16 * 1024 * 1024;
        let mut h = Harness::new("udp-heartbeat-term");
        let (sender, image_id, _position) = subscribe_and_connect_at(&mut h, 2, BIG_TERM, START);
        let start = i64::from(START);
        assert_eq!(positions(&h, image_id), Some((start, start, start)));
        // Past the driver's term, within the image's: accepted, and the high-water
        // mark follows it.
        sender.send_heartbeat(SESSION, START + 64, false);
        assert!(h.cycle_until(|h| h.counter(RECEIVER_HEARTBEATS_RECEIVED) == 1));
        assert_eq!(
            positions(&h, image_id),
            Some((start + 64, start, start)),
            "the high-water mark moved"
        );
        assert_eq!(h.counter(RECEIVER_DROPPED_FRAMES), 0);
        // At the image's term, not below it: dropped and counted, nothing moves.
        sender.send_heartbeat(SESSION, BIG_TERM as i32, false);
        assert!(h.cycle_until(|h| h.counter(RECEIVER_DROPPED_FRAMES) == 1));
        assert_eq!(h.counter(RECEIVER_HEARTBEATS_RECEIVED), 1);
        assert_eq!(h.counter(RECEIVER_OVER_RUNS), 0);
        assert_eq!(positions(&h, image_id), Some((start + 64, start, start)));
        // Beyond half a term past the last status message, in the next term: an
        // over-run, dropped before it is counted as received.
        sender.send_heartbeat_in_term(SESSION, INITIAL_TERM_ID + 1, 64, false);
        assert!(h.cycle_until(|h| h.counter(RECEIVER_OVER_RUNS) == 1));
        assert_eq!(h.counter(RECEIVER_HEARTBEATS_RECEIVED), 1);
        assert_eq!(h.counter(RECEIVER_DROPPED_FRAMES), 1);
        assert_eq!(positions(&h, image_id), Some((start + 64, start, start)));
    }

    /// An image with no accepted datagram or heartbeat for the liveness timeout is
    /// retired as timed out, with `unavailable image`; its log's end of stream position
    /// stays not ended, since a sender that went quiet did not end its stream, and the
    /// subscription that holds the log until `unavailable image` reaches it must not
    /// read an end there (invariant 1).
    #[test]
    fn an_image_with_no_activity_for_the_liveness_timeout_is_retired() {
        let mut h = Harness::new("udp-liveness");
        let (sender, image_id, _position) = subscribe_and_connect(&mut h, 2);
        sender.send_frames(SESSION, 0, 4);
        assert!(h.cycle_until(|h| positions(h, image_id) == Some((256, 256, 0))));
        let log = LogBuffers::open_image(h.dir.path(), image_id, false).expect("the image opens");
        h.now += spec::nanos(cavalry_proto::udp::IMAGE_LIVENESS_TIMEOUT);
        assert!(h.cycle_until(|h| h.counter(IMAGE_TIMED_OUT) == 1));
        assert_eq!(
            log.end_of_stream_position(),
            NOT_ENDED,
            "a timed-out stream has no end of stream position"
        );
        assert!(h.cycle_until(|h| h.counter(IMAGE_REMOVED) == 1));
        assert_eq!(log.end_of_stream_position(), NOT_ENDED);
        assert_eq!(unavailable_images(&mut h, 2), vec![image_id]);
        assert!(h.conductor.udp_images.is_empty());
        drop(sender);
    }

    /// Removing the last subscription retires the image as unsubscribed with no
    /// `unavailable image`, frees its records, and closes the endpoint, whose five
    /// counters are freed once the receiver dropped the socket.
    #[test]
    fn removing_the_last_subscription_retires_the_image_and_closes_the_endpoint() {
        let mut h = Harness::new("udp-unsubscribe");
        let allocated = h.counter(COUNTERS_ALLOCATED);
        let (sender, _image_id, _position) = subscribe_and_connect(&mut h, 2);
        let now = h.now;
        h.conductor.remove_subscription(1, 3, 2, now);
        assert!(h.cycle_until(|h| h.conductor.receive_endpoints.is_empty()));
        assert_eq!(h.counter(IMAGE_UNSUBSCRIBED), 1);
        assert_eq!(h.counter(IMAGE_REMOVED), 1);
        assert!(
            unavailable_images(&mut h, 2).is_empty(),
            "none for a subscription its client removed"
        );
        assert!(h.conductor.udp_images.is_empty());
        assert_eq!(h.conductor.udp_pairs, 0);
        assert_eq!(h.counter(CHANNEL_CLOSED), 1);
        assert_eq!(
            h.counter(COUNTERS_ALLOCATED),
            allocated,
            "every record is freed"
        );
        drop(sender);
    }

    /// Two readers whose positions fail their bound in one cycle are both reported:
    /// each counts `log_buffer.rejected_position`, and each client is evicted with
    /// `client timeout`, so that no subscription is left whose position is ignored
    /// (ADR 0010).
    #[test]
    fn every_reader_with_a_rejected_position_evicts_its_client() {
        let mut h = Harness::new("udp-rejected");
        let (sender, image_id, first) = subscribe_and_connect(&mut h, 2);
        let port = h.conductor.receive_endpoints[0].endpoint.port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        h.conductor
            .add_subscription(2, 3, STREAM, channel.as_bytes(), h.now);
        let (joined, position_id, _, _) = last_available_image(&mut h, 3).expect("joins the image");
        assert_eq!(joined, image_id);
        let second = h
            .conductor
            .counters
            .counter(u32::try_from(position_id).expect("a counter id"))
            .expect("the second subscriber's counter");
        h.cycle();
        assert_eq!(h.conductor.subscriptions.len(), 2);
        // Both positions off the frame alignment in the same cycle.
        first.store_release(1);
        second.store_release(1);
        assert!(h.cycle_until(|h| h.counter(LOG_BUFFER_REJECTED_POSITION) == 2));
        assert!(
            h.cycle_until(|h| h.conductor.subscriptions.is_empty()),
            "both clients evicted"
        );
        let mut timed_out = client_timeouts(&mut h);
        timed_out.sort_unstable();
        assert_eq!(timed_out, vec![1, 2]);
        assert!(h.cycle_until(|h| h.counter(IMAGE_UNSUBSCRIBED) == 1));
        drop(sender);
    }

    /// A retire the pending replies' bound put off is tried again every cycle: with the
    /// replies one short of their bound, the last subscription's removal takes the last
    /// slot and leaves the image live; once the conductor has taken the replies, the
    /// image is retired as unsubscribed and the endpoint closes (invariant 3).
    #[test]
    fn a_retire_put_off_by_the_pending_replies_bound_is_tried_again() {
        let mut h = Harness::new("udp-retire-retry");
        let (sender, _image_id, _position) = subscribe_and_connect(&mut h, 2);
        // The receiver alone, its replies untaken: a close for an endpoint it never held
        // is a reply each, until the reply queue is full and the pending replies are
        // one short of their bound.
        let to_fill = spec::RECEIVER_QUEUE_CAPACITY + receiver::PENDING_REPLIES - 1;
        let mut offered = 0usize;
        while h.udp_receiver.pending_replies() < receiver::PENDING_REPLIES - 1 {
            while offered < to_fill
                && h.conductor
                    .receiver
                    .requests
                    .offer(receiver::Request::RemoveEndpoint {
                        registration_id: -1 - offered as i64,
                    })
                    .is_ok()
            {
                offered += 1;
            }
            h.now += MS;
            let now = h.now;
            h.udp_receiver.duty_cycle(now);
        }
        assert_eq!(
            h.udp_receiver.pending_replies(),
            receiver::PENDING_REPLIES - 1
        );
        // The last subscription goes: its reader's removal takes the last slot, and
        // the retire finds no room.
        let now = h.now;
        h.conductor.remove_subscription(1, 3, 2, now);
        h.now += MS;
        let now = h.now;
        h.udp_receiver.duty_cycle(now);
        assert_eq!(h.udp_receiver.pending_replies(), receiver::PENDING_REPLIES);
        assert_eq!(
            h.counter(IMAGE_UNSUBSCRIBED),
            0,
            "the retire waits for room"
        );
        assert_eq!(h.conductor.udp_images.len(), 1);
        // The conductor takes the replies: the retire goes on the next cycle.
        assert!(h.cycle_until(|h| h.counter(IMAGE_UNSUBSCRIBED) == 1));
        assert!(h.cycle_until(|h| h.counter(IMAGE_REMOVED) == 1));
        assert!(h.cycle_until(|h| h.conductor.receive_endpoints.is_empty()));
        assert_eq!(h.counter(CHANNEL_CLOSED), 1);
        drop(sender);
    }

    /// A reader's removal is held to the pending replies' bound too: with the replies
    /// one short of it and one subscription reading two images, the subscription's
    /// removal answers for one reader and takes the last slot, and the other reader is
    /// marked leaving, read and counted no more, its reply waiting for room, so that
    /// neither image counts a subscriber and neither retires while the bound holds.
    /// Once the conductor has taken the replies, the second `ReaderRemoved` goes, both
    /// pairs' records are freed on the two replies, with no `unavailable image` for a
    /// subscription its client removed, and both images retire as unsubscribed
    /// (invariant 3).
    #[test]
    fn a_reader_removal_at_the_pending_replies_bound_waits_for_room() {
        let mut h = Harness::new("udp-remove-bound");
        let (sender, first_id, _position) = subscribe_and_connect(&mut h, 2);
        // A second sender with another session on the same stream and port: a second
        // image, read by the same subscription.
        let port = h.conductor.receive_endpoints[0].endpoint.port();
        let second = TestSender::bind(port);
        second.send_setup(SESSION + 1, 0);
        assert!(h.cycle_until(|h| h.counter(IMAGE_CREATED) == 2));
        assert!(h.cycle_until(|h| h.udp_receiver.reader_count() == 2));
        let (second_id, _, _, _) = last_available_image(&mut h, 2).expect("a second image");
        assert_ne!(second_id, first_id);
        assert_eq!(h.conductor.udp_pairs, 2);
        assert_eq!(h.udp_receiver.subscriber_count_of(first_id), Some(1));
        assert_eq!(h.udp_receiver.subscriber_count_of(second_id), Some(1));
        // The receiver alone, its replies untaken: a close for an endpoint it never held
        // is a reply each, until the reply queue is full and the pending replies are
        // one short of their bound.
        let to_fill = spec::RECEIVER_QUEUE_CAPACITY + receiver::PENDING_REPLIES - 1;
        let mut offered = 0usize;
        while h.udp_receiver.pending_replies() < receiver::PENDING_REPLIES - 1 {
            while offered < to_fill
                && h.conductor
                    .receiver
                    .requests
                    .offer(receiver::Request::RemoveEndpoint {
                        registration_id: -1 - offered as i64,
                    })
                    .is_ok()
            {
                offered += 1;
            }
            h.now += MS;
            let now = h.now;
            h.udp_receiver.duty_cycle(now);
        }
        assert_eq!(
            h.udp_receiver.pending_replies(),
            receiver::PENDING_REPLIES - 1
        );
        // The subscription goes: one reader's reply takes the last slot; the other
        // reader stays, leaving, and neither image counts a subscriber or retires.
        let now = h.now;
        h.conductor.remove_subscription(1, 3, 2, now);
        h.now += MS;
        let now = h.now;
        h.udp_receiver.duty_cycle(now);
        assert_eq!(h.udp_receiver.pending_replies(), receiver::PENDING_REPLIES);
        assert_eq!(
            h.udp_receiver.reader_count(),
            1,
            "the other reader waits for room"
        );
        assert_eq!(h.udp_receiver.subscriber_count_of(first_id), Some(0));
        assert_eq!(h.udp_receiver.subscriber_count_of(second_id), Some(0));
        assert_eq!(
            h.counter(IMAGE_UNSUBSCRIBED),
            0,
            "the retires wait for room"
        );
        assert_eq!(h.conductor.udp_images.len(), 2);
        assert_eq!(h.conductor.udp_pairs, 2);
        // Another cycle with the replies still untaken changes nothing: the bound holds.
        h.now += MS;
        let now = h.now;
        h.udp_receiver.duty_cycle(now);
        assert_eq!(h.udp_receiver.pending_replies(), receiver::PENDING_REPLIES);
        assert_eq!(h.udp_receiver.reader_count(), 1);
        assert_eq!(h.counter(IMAGE_UNSUBSCRIBED), 0);
        // The conductor takes the replies: the other reader's reply goes, both pairs'
        // records are freed on the two `ReaderRemoved`, before either retire reaches
        // the conductor, and both images retire as unsubscribed.
        assert!(h.cycle_until(|h| h.conductor.udp_pairs == 0));
        assert!(h.cycle_until(|h| h.counter(IMAGE_UNSUBSCRIBED) == 2));
        assert!(h.cycle_until(|h| h.counter(IMAGE_REMOVED) == 2));
        assert_eq!(h.udp_receiver.reader_count(), 0);
        assert!(h.conductor.udp_images.is_empty());
        assert_eq!(
            unavailable_images(&mut h, 2),
            Vec::<i64>::new(),
            "removed by its client: the pairs went on the replies, not on the retires"
        );
        assert!(h.cycle_until(|h| h.conductor.receive_endpoints.is_empty()));
        assert_eq!(h.counter(CHANNEL_CLOSED), 1);
        drop(sender);
        drop(second);
    }

    /// A status message or a NAK that arrives at a receive endpoint is a frame this
    /// side does not handle: each is dropped and counted in `receiver.dropped_frames`
    /// by one, and nothing else moves, no other counter, no position and no answer
    /// (`spec/udp.md`, "The datagram").
    #[test]
    fn a_status_message_or_a_nak_at_a_receive_endpoint_is_dropped_and_counted() {
        let mut h = Harness::new("udp-sm-nak-dropped");
        let (mut sender, image_id, _position) = subscribe_and_connect(&mut h, 2);
        sender.send_frames(SESSION, 0, 4);
        assert!(h.cycle_until(|h| positions(h, image_id) == Some((256, 256, 0))));
        // A status message on the interval, so that the next is 200 ms of the test's
        // clock away and `receiver.status_messages_sent` can stand still below.
        h.now += 200 * MS;
        h.cycle();
        sender.drain();
        let others = |h: &Harness| {
            [
                h.counter(RECEIVER_FRAMES_RECEIVED),
                h.counter(RECEIVER_HEARTBEATS_RECEIVED),
                h.counter(RECEIVER_SETUPS_RECEIVED),
                h.counter(RECEIVER_SETUPS_UNWANTED),
                h.counter(RECEIVER_SETUPS_ELICITED),
                h.counter(RECEIVER_STATUS_MESSAGES_SENT),
                h.counter(RECEIVER_NAKS_SENT),
                h.counter(RECEIVER_GAPS_DETECTED),
                h.counter(RECEIVER_UNDER_RUNS),
                h.counter(RECEIVER_OVER_RUNS),
                h.counter(RECEIVER_REJECTED_DATAGRAMS),
                h.counter(RECEIVER_DROPPED_PENDING),
                h.counter(RECEIVER_DROPPED_IN_PROGRESS),
                h.counter(RECEIVER_DROPPED_COOL_DOWN),
                h.counter(RECEIVER_DROPPED_UNWANTED),
                h.counter(RECEIVER_RTT_REPLIES_SENT),
            ]
        };
        let before = others(&h);
        assert_eq!(h.counter(RECEIVER_DROPPED_FRAMES), 0);
        // A status message for the image, as the receiver itself sends one: dropped
        // and counted.
        let message = StatusMessage {
            flags: 0,
            session_id: SESSION,
            stream_id: STREAM,
            consumption_term_id: INITIAL_TERM_ID,
            consumption_term_offset: 0,
            receiver_window_length: 4096,
            receiver_id: h.udp_receiver.receiver_id(),
            group_tag: None,
        };
        sender.send(&message.to_bytes()[..message.frame_length()]);
        assert!(h.cycle_until(|h| h.counter(RECEIVER_DROPPED_FRAMES) == 1));
        assert_eq!(others(&h), before, "the status message moved nothing else");
        // A NAK for the image's first frame: dropped and counted.
        sender.send(
            &Nak {
                flags: 0,
                session_id: SESSION,
                stream_id: STREAM,
                term_id: INITIAL_TERM_ID,
                term_offset: 0,
                length: 64,
            }
            .to_bytes(),
        );
        assert!(h.cycle_until(|h| h.counter(RECEIVER_DROPPED_FRAMES) == 2));
        h.settle();
        assert_eq!(h.counter(RECEIVER_DROPPED_FRAMES), 2);
        assert_eq!(others(&h), before, "the NAK moved nothing else");
        assert_eq!(positions(&h, image_id), Some((256, 256, 0)));
    }

    /// A data datagram or heartbeat for a wanted stream and a session with no record
    /// asks the sender for its setup with a status message carrying the send-setup
    /// flag, once per pending setup timeout; one for a stream nobody wants asks for
    /// nothing.
    #[test]
    fn a_datagram_for_an_unknown_session_elicits_a_setup_once_per_timeout() {
        let mut h = Harness::new("udp-elicit");
        let (mut sender, _image_id, _position) = subscribe_and_connect(&mut h, 2);
        sender.drain();
        sender.send_frames(66, 0, 1);
        assert!(h.cycle_until(|h| h.counter(RECEIVER_SETUPS_ELICITED) == 1));
        let message = sender.next_elicit().expect("the elicit");
        assert_eq!((message.session_id, message.stream_id), (66, STREAM));
        assert_eq!(
            (
                message.consumption_term_id,
                message.consumption_term_offset,
                message.receiver_window_length
            ),
            (0, 0, 0)
        );
        assert_eq!(h.counter(RECEIVER_DROPPED_PENDING), 1);
        // While pending, more datagrams are dropped and no second status message goes.
        sender.send_frames(66, 64, 1);
        sender.send_heartbeat(66, 128, false);
        assert!(h.cycle_until(|h| h.counter(RECEIVER_DROPPED_PENDING) == 3));
        assert!(
            sender.next_elicit().is_none(),
            "no second elicit while pending"
        );
        assert_eq!(h.counter(RECEIVER_SETUPS_ELICITED), 1);
        // The pending record expires: the next datagram asks again.
        h.now += spec::nanos(spec::PENDING_SETUP_TIMEOUT) + 200 * MS;
        h.cycle();
        sender.send_frames(66, 128, 1);
        assert!(h.cycle_until(|h| h.counter(RECEIVER_SETUPS_ELICITED) == 2));
        assert!(sender.next_elicit().is_some());
        // A stream nobody wants, and a setup for one: dropped and counted, no answer.
        sender.drain();
        sender.send(&frames(77, 99, 0, 1));
        sender.send(&setup(77, 99, 0).to_bytes());
        assert!(h.cycle_until(|h| {
            h.counter(RECEIVER_DROPPED_UNWANTED) == 1 && h.counter(RECEIVER_SETUPS_UNWANTED) == 1
        }));
        assert!(sender.next_elicit().is_none());
        // The setup the elicit asked for creates the image.
        sender.send_setup(66, 128);
        assert!(h.cycle_until(|h| h.counter(IMAGE_CREATED) == 2));
    }

    /// A second setup while the first is with the conductor creates no second image: it
    /// is dropped and counted, and every message is delivered once.
    #[test]
    fn a_second_setup_while_the_image_is_in_progress_creates_no_second_image() {
        let mut h = Harness::new("udp-in-progress");
        let port = free_port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        h.conductor
            .add_subscription(1, 2, STREAM, channel.as_bytes(), h.now);
        h.cycle();
        let sender = TestSender::bind(port);
        sender.send_setup(SESSION, 0);
        sender.send_setup(SESSION, 0);
        // The receiver alone, until both have landed: the first setup goes to the
        // conductor, the second finds the session in progress.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while h.counter(RECEIVER_SETUPS_RECEIVED) < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "the setups did not land"
            );
            h.now += MS;
            let now = h.now;
            h.udp_receiver.duty_cycle(now);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(h.counter(RECEIVER_DROPPED_IN_PROGRESS), 1);
        assert_eq!(h.udp_receiver.session_records(), 1);
        assert!(h.cycle_until(|h| h.counter(IMAGE_CREATED) == 1));
        assert_eq!(h.conductor.udp_images.len(), 1);
        // A setup for a session with an image is ignored, as a duplicate frame is.
        sender.send_setup(SESSION, 0);
        assert!(h.cycle_until(|h| h.counter(RECEIVER_SETUPS_RECEIVED) == 3));
        h.settle();
        assert_eq!(h.counter(IMAGE_CREATED), 1);
        assert_eq!(h.conductor.udp_images.len(), 1);
    }

    /// An RTT measurement with the reply flag for the image's session is answered when
    /// it names this receiver or any; one for another receiver is dropped and counted.
    #[test]
    fn an_rtt_measurement_is_answered_for_this_receiver_and_dropped_for_another() {
        let mut h = Harness::new("udp-rtt");
        let (mut sender, _image_id, _position) = subscribe_and_connect(&mut h, 2);
        sender.drain();
        let probe = |receiver_id: i64| RttMeasurement {
            flags: cavalry_proto::udp::RTT_MEASUREMENT_REPLY,
            session_id: SESSION,
            stream_id: STREAM,
            echo_timestamp: 1234,
            reception_delta: 0,
            receiver_id,
        };
        sender.send(&probe(0).to_bytes());
        assert!(h.cycle_until(|h| h.counter(RECEIVER_RTT_REPLIES_SENT) == 1));
        match sender.next_control() {
            Some(ControlFrame::RttMeasurement(answer)) => {
                assert!(!answer.is_probe());
                assert_eq!(answer.echo_timestamp, 1234);
            }
            other => panic!("expected an RTT answer, got {other:?}"),
        }
        sender.send(&probe(h.udp_receiver.receiver_id()).to_bytes());
        assert!(h.cycle_until(|h| h.counter(RECEIVER_RTT_REPLIES_SENT) == 2));
        assert!(matches!(
            sender.next_control(),
            Some(ControlFrame::RttMeasurement(_))
        ));
        sender.send(&probe(h.udp_receiver.receiver_id() ^ 1).to_bytes());
        assert!(h.cycle_until(|h| h.counter(RECEIVER_DROPPED_UNWANTED) == 1));
        assert!(sender.next_control().is_none());
        assert_eq!(h.counter(RECEIVER_RTT_REPLIES_SENT), 2);
    }

    /// A second subscription on the endpoint and stream joins a live image at its
    /// rebuild position with a position of its own, and the slowest of the two drives
    /// the status message.
    #[test]
    fn a_second_subscription_joins_a_live_image_at_its_rebuild_position() {
        let mut h = Harness::new("udp-join");
        let (mut sender, image_id, position) = subscribe_and_connect(&mut h, 2);
        sender.send_frames(SESSION, 0, 4);
        assert!(h.cycle_until(|h| positions(h, image_id) == Some((256, 256, 0))));
        let port = h.conductor.receive_endpoints[0].endpoint.port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        h.conductor
            .add_subscription(1, 3, STREAM, channel.as_bytes(), h.now);
        let (joined, position_id, _, _) = last_available_image(&mut h, 3).expect("joins the image");
        assert_eq!(joined, image_id);
        let second = h
            .conductor
            .counters
            .counter(u32::try_from(position_id).expect("a counter id"))
            .expect("the second subscriber's counter");
        assert_eq!(second.get(), 256, "joined at the rebuild position");
        assert_eq!(h.conductor.udp_pairs, 2);
        assert_eq!(h.conductor.receive_endpoints[0].subscription_count, 2);
        h.cycle();
        // Both at 256: the next status message carries 256.
        position.store_release(256);
        sender.drain();
        h.now += 200 * MS;
        h.cycle();
        assert_eq!(
            sender
                .next_status_message()
                .map(|m| m.consumption_term_offset),
            Some(256)
        );
        // The first reads on, the second does not: the slowest drives the position.
        sender.send_frames(SESSION, 256, 4);
        assert!(h.cycle_until(|h| positions(h, image_id) == Some((512, 512, 256))));
        position.store_release(512);
        sender.drain();
        h.now += 200 * MS;
        h.cycle();
        assert_eq!(
            sender
                .next_status_message()
                .map(|m| m.consumption_term_offset),
            Some(256),
            "the slowest reader's position, the second subscriber's"
        );
        // Removing one subscription leaves the image with the other.
        let now = h.now;
        h.conductor.remove_subscription(1, 4, 3, now);
        assert!(h.cycle_until(|h| h.conductor.udp_pairs == 1));
        assert_eq!(h.conductor.udp_images.len(), 1);
        assert_eq!(h.counter(IMAGE_UNSUBSCRIBED), 0);
    }

    /// `start recording` on the `cavalry:udp` channel records the image: the recorder
    /// reads the image's log from its rebuild position, the recording is one of the
    /// image's readers, and the stop frees it.
    #[test]
    fn start_recording_on_the_udp_channel_records_the_image() {
        let mut h = Harness::new("udp-record");
        let (mut sender, image_id, position) = subscribe_and_connect(&mut h, 2);
        sender.send_frames(SESSION, 0, 4);
        assert!(h.cycle_until(|h| positions(h, image_id) == Some((256, 256, 0))));
        let port = h.conductor.receive_endpoints[0].endpoint.port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        let now = h.now;
        h.conductor
            .start_recording(9, STREAM, channel.as_bytes(), now);
        assert_eq!(h.last_error(9), None);
        assert!(h.cycle_until(|h| h.counter(RECORDING_STARTED) == 1));
        let recording = h
            .conductor
            .recordings
            .iter()
            .find(|recording| recording.publication_id == image_id)
            .expect("the recording of the image");
        assert_eq!(recording.start_position, 256, "from the rebuild position");
        let recording_id = recording.recording_id.expect("ready");
        // More frames: the recorder follows.
        sender.send_frames(SESSION, 256, 4);
        assert!(h.cycle_until(|h| {
            h.conductor
                .recordings
                .iter()
                .find(|recording| recording.publication_id == image_id)
                .and_then(|recording| recording.position.as_ref().map(Counter::get))
                == Some(512)
        }));
        // The recording is a reader: the subscriber ahead does not raise the status
        // message's position past the recorder, which is at 512 too.
        position.store_release(512);
        sender.drain();
        h.now += 200 * MS;
        h.cycle();
        assert_eq!(
            sender
                .next_status_message()
                .map(|m| m.consumption_term_offset),
            Some(512)
        );
        // A second start on the same channel and stream is code 22: already recording.
        let now = h.now;
        h.conductor
            .start_recording(10, STREAM, channel.as_bytes(), now);
        assert_eq!(h.last_error(10).map(|error| error.0), Some(22));
        h.conductor.stop_recording(11, recording_id);
        assert!(h.cycle_until(|h| h.counter(RECORDING_STOPPED) == 1));
        assert!(h.conductor.recordings.is_empty());
    }

    /// A recorded image that is retired keeps its log until the recording has stopped:
    /// the receiver's last scan leaves the rebuild position at the end of the contiguous
    /// frames, the conductor hands the recorder the stop once it has written up to it,
    /// `recording stopped` names that position, and only then is the log unlinked and
    /// the image removed (`spec/udp.md`, "The receiver", step 5). The log's end of
    /// stream position stays not ended throughout: the stream did not end.
    #[test]
    fn a_retired_recorded_image_keeps_its_log_until_the_recording_stops() {
        let mut h = Harness::new("udp-record-retire");
        let (sender, image_id, _position) = subscribe_and_connect(&mut h, 2);
        sender.send_frames(SESSION, 0, 4);
        assert!(h.cycle_until(|h| positions(h, image_id) == Some((256, 256, 0))));
        let port = h.conductor.receive_endpoints[0].endpoint.port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        let now = h.now;
        h.conductor
            .start_recording(9, STREAM, channel.as_bytes(), now);
        assert!(h.cycle_until(|h| h.counter(RECORDING_STARTED) == 1));
        // Four more frames, the last one lost: the recorder can write to 448 only.
        sender.send_frames(SESSION, 256, 3);
        sender.send_frames(SESSION, 512, 1);
        assert!(h.cycle_until(|h| positions(h, image_id) == Some((576, 448, 0))));
        let log = LogBuffers::open_image(h.dir.path(), image_id, false).expect("the image opens");
        // The last subscription goes: the image is retired, but its log stays while the
        // recording runs, and the recording is stopped at the rebuild position once the
        // recorder has written up to it.
        let now = h.now;
        h.conductor.remove_subscription(1, 3, 2, now);
        assert!(h.cycle_until(|h| h.counter(IMAGE_UNSUBSCRIBED) == 1));
        assert_eq!(
            h.counter(IMAGE_REMOVED),
            0,
            "the log waits for the recording"
        );
        assert!(
            h.dir
                .path()
                .join(format!("images/{image_id}.logbuffer"))
                .is_file()
        );
        assert_eq!(
            log.end_of_stream_position(),
            NOT_ENDED,
            "an unsubscribed stream has no end of stream position"
        );
        assert!(h.cycle_until(|h| h.counter(RECORDING_STOPPED) == 1));
        assert!(h.cycle_until(|h| h.counter(IMAGE_REMOVED) == 1));
        assert_eq!(log.end_of_stream_position(), NOT_ENDED);
        assert!(
            !h.dir
                .path()
                .join(format!("images/{image_id}.logbuffer"))
                .exists()
        );
        assert!(h.conductor.udp_images.is_empty());
        assert!(h.conductor.recordings.is_empty());
        assert!(h.cycle_until(|h| h.conductor.receive_endpoints.is_empty()));
        assert_eq!(h.counter(CHANNEL_CLOSED), 1);
        // The recording stopped at 448, the end of the contiguous frames.
        let mut stopped = None;
        while h
            .receiver
            .receive(|type_id, message| {
                if let Ok(Event::RecordingStopped { stop_position, .. }) =
                    Event::parse(type_id, message)
                {
                    stopped = Some(stop_position);
                }
            })
            .expect("the receiver reads")
        {}
        assert_eq!(stopped, Some(448));
    }

    /// `add subscription` on a channel whose previous endpoint is still closing, its
    /// socket with the receiver, waits for the close with nothing registered and no
    /// event sent, and runs once the receiver has closed it: the endpoint is bound
    /// again, the subscription registered and `subscription ready` sent, with
    /// `channel.opened` at 2 and nothing counted as an error (ADR 0049; card 194). A
    /// second add from the same client in the window joins the first, in order.
    #[test]
    fn an_add_on_a_channel_whose_endpoint_is_still_closing_waits_for_the_close() {
        let mut h = Harness::new("udp-closing");
        let port = free_port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        h.conductor
            .add_subscription(1, 2, STREAM, channel.as_bytes(), h.now);
        assert_eq!(h.last_error(2), None);
        h.cycle();
        let now = h.now;
        h.conductor.remove_subscription(1, 3, 2, now);
        assert_eq!(
            h.conductor.receive_endpoints[0].state,
            ChannelState::Closing
        );
        // Before the receiver's next cycle: the socket is still bound, and the adds
        // wait.
        h.conductor
            .add_subscription(1, 4, STREAM, channel.as_bytes(), h.now);
        h.conductor
            .add_subscription(1, 5, STREAM, channel.as_bytes(), h.now);
        assert_eq!(h.last_error(4), None);
        assert_eq!(h.last_error(5), None);
        assert!(h.conductor.subscriptions.is_empty(), "nothing registered");
        assert_eq!(h.conductor.pending_subscriptions.len(), 2);
        assert_eq!(h.counter(SUBSCRIPTION_ADD_DEFERRED), 2);
        assert_eq!(h.counter(SUBSCRIPTION_ADD_DEFERRED_WAITING), 2);
        assert_eq!(h.conductor.receive_endpoints.len(), 1);
        // The receiver closes the socket, the conductor frees the entry, and the adds
        // run in their order: the first binds the endpoint again.
        assert!(h.cycle_until(|h| h.conductor.subscriptions.len() == 2));
        assert!(h.conductor.pending_subscriptions.is_empty());
        assert_eq!(h.counter(SUBSCRIPTION_ADD_DEFERRED_WAITING), 0);
        assert_eq!(h.counter(SUBSCRIPTION_ADD_DEFERRED_TIMED_OUT), 0);
        assert_eq!(h.last_error(4), None);
        assert_eq!(h.last_error(5), None);
        assert_eq!(h.conductor.receive_endpoints.len(), 1);
        assert_eq!(h.conductor.receive_endpoints[0].state, ChannelState::Active);
        assert_eq!(
            h.conductor
                .subscriptions
                .iter()
                .map(|subscription| subscription.registration_id)
                .collect::<Vec<_>>(),
            vec![4, 5]
        );
        assert_eq!(h.counter(CHANNEL_OPENED), 2);
        assert_eq!(h.counter(CHANNEL_CLOSED), 1);
    }

    /// What the events not yet read say about subscription `id`, read in one pass
    /// since any reader drains them all: the codes of the errors under its
    /// correlation id, the images made available to it and the images made
    /// unavailable to it.
    fn events_about(h: &mut Harness, id: i64) -> (Vec<i32>, Vec<i64>, Vec<i64>) {
        let (mut errors, mut available, mut unavailable) = (Vec::new(), Vec::new(), Vec::new());
        for (type_id, message) in drained_events(h) {
            match Event::parse(type_id, &message) {
                Ok(Event::Error {
                    offending_correlation_id,
                    error_code,
                    ..
                }) if offending_correlation_id == id => errors.push(error_code),
                Ok(Event::AvailableImage {
                    correlation_id,
                    subscriber_registration_id,
                    ..
                }) if subscriber_registration_id == id => available.push(correlation_id),
                Ok(Event::UnavailableImage {
                    correlation_id,
                    subscriber_registration_id,
                    ..
                }) if subscriber_registration_id == id => unavailable.push(correlation_id),
                _ => {}
            }
        }
        (errors, available, unavailable)
    }

    /// An add that arrives while the image the stream's last subscription left is still
    /// being retired waits as one on a closing endpoint does (ADR 0068). The receiver
    /// has not yet said the image is retired, so the conductor still lists it as live
    /// and its endpoint as active: the add is given no image and registers nothing, and
    /// it runs once the endpoint has closed, binding it again. The stream's next setup
    /// then gives the new subscription an image of its own, with no cool-down to sit
    /// out. Before, the add joined the image being retired, whose log was unlinked when
    /// the receiver's reply came: a subscriber that had not opened the log yet read the
    /// missing file as corruption and closed its client.
    #[test]
    fn an_add_while_the_last_subscriptions_image_is_being_retired_waits_for_the_close() {
        let mut h = Harness::new("udp-resubscribe-retiring");
        let (sender, image_id, _position) = subscribe_and_connect(&mut h, 2);
        let port = h.conductor.receive_endpoints[0].endpoint.port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        let now = h.now;
        h.conductor.remove_subscription(1, 3, 2, now);
        // The receiver has not run: the image is listed, live, and holds the endpoint.
        assert_eq!(h.conductor.udp_images.len(), 1);
        assert_eq!(h.conductor.udp_images[0].state, ImageState::Live);
        assert!(h.conductor.udp_images[0].leaving);
        assert_eq!(h.conductor.receive_endpoints[0].state, ChannelState::Active);
        h.conductor
            .add_subscription(1, 4, STREAM, channel.as_bytes(), h.now);
        assert_eq!(
            events_about(&mut h, 4),
            (vec![], vec![], vec![]),
            "no error, and the image being retired is not handed over"
        );
        assert!(h.conductor.subscriptions.is_empty(), "nothing registered");
        assert_eq!(h.conductor.pending_subscriptions.len(), 1);
        assert_eq!(h.counter(SUBSCRIPTION_ADD_DEFERRED), 1);
        // The image is retired, the endpoint closes, and the add runs: bound again.
        assert!(h.cycle_until(|h| h.conductor.subscriptions.len() == 1));
        assert!(h.conductor.pending_subscriptions.is_empty());
        assert_eq!(h.counter(IMAGE_UNSUBSCRIBED), 1);
        assert_eq!(h.counter(IMAGE_REMOVED), 1);
        assert_eq!(h.counter(CHANNEL_CLOSED), 1);
        assert_eq!(h.counter(CHANNEL_OPENED), 2);
        assert!(h.conductor.udp_images.is_empty());
        assert_eq!(
            events_about(&mut h, 4),
            (vec![], vec![], vec![]),
            "it was never told of the image retired"
        );
        // The stream's next setup: an image of its own for the new subscription.
        sender.send_setup(SESSION, 0);
        assert!(
            h.cycle_until(|h| h.conductor.udp_images.len() == 1),
            "the setup frame created a new image"
        );
        h.cycle();
        let (joined, _, _, _) = last_available_image(&mut h, 4).expect("an available image");
        assert_ne!(joined, image_id, "a new image, not the one retired");
        drop(sender);
    }

    /// The same window on an endpoint that another stream's subscription keeps open.
    /// The add runs at once, since the endpoint is not closing, and is not given the
    /// image being retired. The image is retired as unsubscribed with no event to the
    /// new subscription, which was never told of it. The stream's next image comes
    /// after its session's cool-down, as `spec/udp.md` says of a subscription added
    /// back on an endpoint that stays open (ADR 0068).
    #[test]
    fn an_add_on_an_endpoint_another_stream_keeps_is_not_given_the_image_being_retired() {
        let mut h = Harness::new("udp-resubscribe-kept");
        let (sender, _image_id, _position) = subscribe_and_connect(&mut h, 2);
        let port = h.conductor.receive_endpoints[0].endpoint.port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        h.conductor
            .add_subscription(1, 5, STREAM + 1, channel.as_bytes(), h.now);
        assert_eq!(h.last_error(5), None);
        h.cycle();
        let now = h.now;
        h.conductor.remove_subscription(1, 6, 2, now);
        assert_eq!(h.conductor.udp_images.len(), 1);
        assert!(h.conductor.udp_images[0].leaving);
        h.conductor
            .add_subscription(1, 7, STREAM, channel.as_bytes(), h.now);
        assert_eq!(
            events_about(&mut h, 7),
            (vec![], vec![], vec![]),
            "no error, and the image being retired is not handed over"
        );
        assert!(
            h.conductor.pending_subscriptions.is_empty(),
            "the endpoint is not closing"
        );
        assert_eq!(h.conductor.subscriptions.len(), 2);
        assert!(h.cycle_until(|h| h.counter(IMAGE_REMOVED) == 1));
        assert_eq!(h.counter(IMAGE_UNSUBSCRIBED), 1);
        assert!(h.conductor.udp_images.is_empty());
        assert_eq!(
            events_about(&mut h, 7),
            (vec![], vec![], vec![]),
            "it was never told of the image retired"
        );
        assert_eq!(h.conductor.receive_endpoints.len(), 1);
        assert_eq!(h.conductor.receive_endpoints[0].state, ChannelState::Active);
        assert_eq!(h.counter(CHANNEL_CLOSED), 0);
        assert_eq!(h.counter(SUBSCRIPTION_ADD_DEFERRED), 0);
        drop(sender);
    }

    /// A waiting add holds a slot of the subscriptions registry, so the registry fills
    /// with waiting adds as with registered ones and one more is code 10 (invariant
    /// 3); and the waiting adds go with a client that closes before the endpoint has
    /// closed, so nothing of them is registered when it does (ADR 0049).
    #[test]
    fn waiting_adds_hold_registry_slots_and_go_with_their_client() {
        let mut h = Harness::new("udp-closing-full");
        let port = free_port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        h.conductor
            .add_subscription(1, 2, STREAM, channel.as_bytes(), h.now);
        assert_eq!(h.last_error(2), None);
        h.cycle();
        let now = h.now;
        h.conductor.remove_subscription(1, 3, 2, now);
        assert_eq!(
            h.conductor.receive_endpoints[0].state,
            ChannelState::Closing
        );
        for n in 0..spec::MAX_SUBSCRIPTIONS as i64 {
            h.conductor
                .add_subscription(1, 10 + n, STREAM, channel.as_bytes(), h.now);
        }
        assert_eq!(
            h.conductor.pending_subscriptions.len(),
            spec::MAX_SUBSCRIPTIONS
        );
        assert_eq!(h.last_error(10), None);
        h.conductor
            .add_subscription(1, 9_999, STREAM, channel.as_bytes(), h.now);
        let (code, message) = h.last_error(9_999).expect("one more is refused");
        assert_eq!(code, 10, "{message}");
        assert_eq!(
            h.conductor.pending_subscriptions.len(),
            spec::MAX_SUBSCRIPTIONS
        );
        // The client goes: its waiting adds go with it, and the endpoint's close
        // registers nothing.
        assert_eq!(
            h.counter(SUBSCRIPTION_ADD_DEFERRED_WAITING),
            spec::MAX_SUBSCRIPTIONS as i64
        );
        h.conductor.remove_client(1, now);
        assert!(h.conductor.pending_subscriptions.is_empty());
        assert_eq!(h.counter(SUBSCRIPTION_ADD_DEFERRED_WAITING), 0);
        assert!(h.cycle_until(|h| h.conductor.receive_endpoints.is_empty()));
        assert!(h.conductor.subscriptions.is_empty());
        assert_eq!(h.counter(CHANNEL_OPENED), 1);
    }

    /// An add that has waited the closing endpoint wait for its channel's previous
    /// receive endpoint, the receiver held for the whole wait, is answered with code 4,
    /// counted, with nothing registered; and the receiver's close afterwards registers
    /// nothing for it, so no `subscription ready` reaches a client that has given up
    /// on the add (`spec/client-protocol.md`, "Liveness and timeouts"; ADR 0049).
    #[test]
    fn an_add_that_waits_past_the_closing_endpoint_wait_is_code_4() {
        let mut h = Harness::new("udp-closing-wait");
        let port = free_port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        h.conductor
            .add_subscription(1, 2, STREAM, channel.as_bytes(), h.now);
        assert_eq!(h.last_error(2), None);
        h.cycle();
        let now = h.now;
        h.conductor.remove_subscription(1, 3, 2, now);
        assert_eq!(
            h.conductor.receive_endpoints[0].state,
            ChannelState::Closing
        );
        h.conductor
            .add_subscription(1, 4, STREAM, channel.as_bytes(), h.now);
        assert_eq!(h.conductor.pending_subscriptions.len(), 1);
        assert_eq!(h.counter(SUBSCRIPTION_ADD_DEFERRED), 1);
        assert_eq!(h.counter(SUBSCRIPTION_ADD_DEFERRED_WAITING), 1);
        // The receiver runs no cycle. A millisecond inside the bound the add still
        // waits; at the bound it is code 4.
        let arrived = h.conductor.pending_subscriptions[0].arrived;
        let wait = spec::nanos(spec::CLOSING_ENDPOINT_WAIT);
        h.conductor.duty_cycle(arrived + wait - MS);
        assert_eq!(h.last_error(4), None);
        assert_eq!(h.conductor.pending_subscriptions.len(), 1);
        h.conductor.duty_cycle(arrived + wait);
        let (code, message) = h.last_error(4).expect("refused at the bound");
        assert_eq!(code, 4, "{message}");
        assert!(message.contains(&format!("127.0.0.1:{port}")), "{message}");
        assert!(message.contains("has not closed in the 5 s"), "{message}");
        // Both causes, each with what to do (ADR 0068): a recording of an image being
        // retired that has not yet stopped, and the receiver behind.
        assert!(
            message.contains("its images still being retired"),
            "{message}"
        );
        assert!(message.contains("the recorder is behind"), "{message}");
        assert!(
            message.contains("stop the recording before removing"),
            "{message}"
        );
        assert!(message.contains("receiver thread is behind"), "{message}");
        assert!(message.contains("add the subscription again"), "{message}");
        assert!(h.conductor.pending_subscriptions.is_empty());
        assert!(h.conductor.subscriptions.is_empty(), "nothing registered");
        assert_eq!(h.counter(SUBSCRIPTION_ADD_DEFERRED_TIMED_OUT), 1);
        assert_eq!(h.counter(SUBSCRIPTION_ADD_DEFERRED_WAITING), 0);
        // The receiver runs again and closes the endpoint: the close registers nothing
        // and binds nothing.
        h.now = arrived + wait;
        assert!(h.cycle_until(|h| h.conductor.receive_endpoints.is_empty()));
        h.settle();
        assert!(h.conductor.subscriptions.is_empty());
        assert!(h.conductor.receive_endpoints.is_empty());
        let mut answered_again = false;
        while h
            .receiver
            .receive(|type_id, message| match Event::parse(type_id, message) {
                Ok(Event::SubscriptionReady {
                    correlation_id: 4, ..
                })
                | Ok(Event::Error {
                    offending_correlation_id: 4,
                    ..
                }) => answered_again = true,
                _ => {}
            })
            .expect("the receiver reads")
        {}
        assert!(!answered_again, "the refused add is answered once");
        assert_eq!(h.counter(CHANNEL_OPENED), 1);
        assert_eq!(h.counter(CHANNEL_CLOSED), 1);
        assert_eq!(h.counter(SUBSCRIPTION_ADD_DEFERRED_TIMED_OUT), 1);
    }

    /// A `cavalry:ipc` add counts the waiting adds against the subscriptions registry
    /// as a `cavalry:udp` add does, so it cannot take a slot a waiting add holds: with
    /// 1023 subscriptions registered and one add waiting, the IPC add is code 10 with
    /// nothing registered, and the close then registers the waiting add
    /// (`spec/client-protocol.md`, "Sizes"; ADR 0049).
    #[test]
    fn an_ipc_add_cannot_take_the_slot_a_waiting_add_holds() {
        let mut h = Harness::new("udp-closing-ipc");
        let port = free_port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        h.conductor
            .add_subscription(1, 2, STREAM, channel.as_bytes(), h.now);
        assert_eq!(h.last_error(2), None);
        h.cycle();
        let now = h.now;
        h.conductor.remove_subscription(1, 3, 2, now);
        assert_eq!(
            h.conductor.receive_endpoints[0].state,
            ChannelState::Closing
        );
        let registered = spec::MAX_SUBSCRIPTIONS as i64 - 1;
        for n in 0..registered {
            h.conductor
                .add_subscription(1, 10 + n, STREAM, IPC_CHANNEL, h.now);
        }
        assert_eq!(h.last_error(10), None);
        assert_eq!(h.conductor.subscriptions.len(), spec::MAX_SUBSCRIPTIONS - 1);
        h.conductor
            .add_subscription(1, 5_000, STREAM, channel.as_bytes(), h.now);
        assert_eq!(h.conductor.pending_subscriptions.len(), 1);
        // The registry is full, 1023 registered and one waiting: the IPC add is
        // refused, with nothing registered.
        h.conductor
            .add_subscription(1, 6_000, STREAM, IPC_CHANNEL, h.now);
        let (code, message) = h.last_error(6_000).expect("the IPC add is refused");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("subscriptions"), "{message}");
        assert_eq!(h.conductor.subscriptions.len(), spec::MAX_SUBSCRIPTIONS - 1);
        assert!(
            !h.conductor
                .subscriptions
                .iter()
                .any(|subscription| subscription.registration_id == 6_000)
        );
        // The receiver closes the endpoint: the waiting add takes the slot it held.
        assert!(h.cycle_until(|h| h.conductor.subscriptions.len() == spec::MAX_SUBSCRIPTIONS));
        assert_eq!(h.last_error(5_000), None);
        assert!(h.conductor.pending_subscriptions.is_empty());
        assert!(
            h.conductor
                .subscriptions
                .iter()
                .any(|subscription| subscription.registration_id == 5_000)
        );
        assert_eq!(h.counter(CHANNEL_OPENED), 2);
    }

    /// The receive endpoints registry fills at its capacity, and a 65th is code 10
    /// with nothing registered; a port in use is code 4.
    #[test]
    fn the_receive_endpoints_registry_fills_at_its_capacity() {
        let mut h = Harness::new("udp-endpoints");
        // A port in use is code 4, with nothing registered.
        let taken = UdpSocket::bind("127.0.0.1:0").expect("a socket");
        let channel = format!(
            "cavalry:udp?endpoint=127.0.0.1:{}",
            taken.local_addr().expect("bound").port()
        );
        h.conductor
            .add_subscription(1, 998, STREAM, channel.as_bytes(), h.now);
        let (code, message) = h.last_error(998).expect("refused");
        assert_eq!(code, 4, "{message}");
        assert!(h.conductor.subscriptions.is_empty());
        assert!(h.conductor.receive_endpoints.is_empty());
        for n in 0..spec::MAX_RECEIVE_ENDPOINTS as i64 {
            let port = free_port();
            let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
            h.conductor
                .add_subscription(1, 100 + n, STREAM, channel.as_bytes(), h.now);
            assert_eq!(h.last_error(100 + n), None, "endpoint {n}");
        }
        assert_eq!(
            h.conductor.receive_endpoints.len(),
            spec::MAX_RECEIVE_ENDPOINTS
        );
        let port = free_port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        h.conductor
            .add_subscription(1, 999, STREAM, channel.as_bytes(), h.now);
        let (code, message) = h.last_error(999).expect("refused");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("receive endpoints"), "{message}");
        assert_eq!(h.conductor.subscriptions.len(), spec::MAX_RECEIVE_ENDPOINTS);
    }

    /// `add subscription` on a `cavalry:udp` channel is code 10 while the receiver's
    /// request queue has no room for its two requests, with nothing registered.
    #[test]
    fn a_udp_subscription_is_refused_while_the_receivers_queue_is_full() {
        let dir = super::tests::TestDir::new("udp-queue-full");
        prepare_dir(dir.path()).expect("the directory is prepared");
        let (requests, _requests) = spsc::queue::<Request>(4);
        let (_replies, replies) = spsc::queue::<Reply>(4);
        let link = RecorderLink {
            requests,
            replies,
            next_recording_id: 0,
        };
        let (requests, _replayer_requests) = spsc::queue::<replayer::Request>(4);
        let (_replayer_replies, replies) = spsc::queue::<replayer::Reply>(4);
        let replayer_link = ReplayerLink { requests, replies };
        let (requests, _sender_requests) = spsc::queue::<sender::Request>(4);
        let (_sender_replies, replies) = spsc::queue::<sender::Reply>(4);
        let sender_link = SenderLink { requests, replies };
        // Room for one endpoint and one interest, and nobody takes them.
        let (requests, _receiver_requests) = spsc::queue::<receiver::Request>(2);
        let (_receiver_replies, replies) = spsc::queue::<receiver::Reply>(2);
        let receiver_link = ReceiverLink { requests, replies };
        let mut conductor = Conductor::create(
            dir.path(),
            IdleStrategy::Backoff,
            link,
            replayer_link,
            sender_link,
            receiver_link,
        )
        .expect("the conductor is created");
        let mut events = cavalry_shm::broadcast::BroadcastBuffer::open(dir.path(), false)
            .expect("the buffer opens")
            .receiver()
            .expect("a receiver");
        let port = free_port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        conductor.add_subscription(1, 2, STREAM, channel.as_bytes(), now_nanos());
        assert_eq!(conductor.subscriptions.len(), 1);
        let port = free_port();
        let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        conductor.add_subscription(1, 3, STREAM, channel.as_bytes(), now_nanos());
        let mut refused = None;
        while events
            .receive(|type_id, message| {
                if let Ok(Event::Error {
                    offending_correlation_id,
                    error_code,
                    error_message,
                }) = Event::parse(type_id, message)
                    && offending_correlation_id == 3
                {
                    refused = Some((
                        error_code,
                        String::from_utf8_lossy(error_message).into_owned(),
                    ));
                }
            })
            .expect("the receiver reads")
        {}
        let (code, message) = refused.expect("refused");
        assert_eq!(code, 10, "{message}");
        assert!(message.contains("receiver's request queue"), "{message}");
        assert_eq!(conductor.subscriptions.len(), 1, "nothing registered");
        assert_eq!(conductor.receive_endpoints.len(), 1);
        drop(conductor);
    }
}
