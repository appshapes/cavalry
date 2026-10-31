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
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cavalry_log::dir::Recovery;

use cavalry_proto::channel::{Channel, ChannelError, UdpChannel};
use cavalry_proto::client_protocol::{
    Command, ErrorCode, Event, IPC_CHANNEL, MAX_STRING_LENGTH, NO_CHANNEL_STATUS, NULL_LENGTH,
    ProtocolError, ReplayStopReason,
};
use cavalry_proto::frame::FRAME_ALIGNMENT;
use cavalry_shm::broadcast::{BroadcastBuffer, Transmitter};
use cavalry_shm::command_ring::{
    CommandRing, CommandRingError, Consumer, UnblockOutcome as RingUnblock,
};
use cavalry_shm::counters::generated::{
    CHANNEL_CLOSED, CHANNEL_IMAGES, CHANNEL_OPENED, CHANNEL_PUBLICATIONS, CHANNEL_SOCKET_RCVBUF,
    CHANNEL_SOCKET_SNDBUF, CHANNEL_STATUS, CLIENT_PROTOCOL_ERRORS,
    CLIENT_PROTOCOL_REJECTED_COMMAND, CLIENT_TIMED_OUT, COMMAND_RING_REJECTED_RECORD,
    COMMAND_RING_REJECTED_TAIL, COMMAND_RING_UNBLOCKED, CONDUCTOR_IDLE_STRATEGY,
    DRIVER_LOG_CREATE_FAILED, DRIVER_REGISTRY_FULL, LOG_BUFFER_REJECTED_POSITION,
    LOG_BUFFER_REJECTED_TAIL, PUBLICATION_LIMIT, PUBLICATION_POSITION, PUBLICATION_UNBLOCKED,
    RECORDING_BYTES, RECORDING_CATALOG_FULL, RECORDING_CATALOG_TRUNCATED, RECORDING_DELETED,
    RECORDING_FILES_REMOVED, RECORDING_INDEX_REWRITTEN, RECORDING_INVALIDATED, RECORDING_POSITION,
    RECORDING_RECOVERED, RECORDING_RETENTION_FAILED, RECORDING_SEGMENTS_PURGED,
    RECORDING_SLOT_DROPPED, RECORDING_START_FAILED, RECORDING_STARTED, RECORDING_STOPPED,
    RECORDING_SYNCED_POSITION, RECORDING_TRUNCATED, REPLAY_ENDED, REPLAY_FAILED,
    REPLAY_NOT_CONNECTED, REPLAY_POSITION, REPLAY_START_FAILED, REPLAY_STARTED, REPLAY_STOPPED,
    SENDER_LIMIT, SENDER_POSITION, SUBSCRIPTION_POSITION,
};
use cavalry_shm::counters::{Counter, CounterId, CountersError, CountersFile};
use cavalry_shm::log::layout as log_layout;
use cavalry_shm::log::{HeaderFields, LogBuffers, LogType, UnblockOutcome as LogUnblock};
use cavalry_shm::spsc;

use crate::error::DriverError;
use crate::idle::{IdleStrategy, Idler};
use crate::recorder::{Reply, Request, Start, StartFailure};
use crate::replayer;
use crate::sender;
use crate::socket::SendSocket;
use crate::spec;

/// The docs anchor an error event's message names.
const ERRORS_DOC: &str = "docs/reference/client-protocol.md#error-codes";

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

/// The sender's deferred requests' bound: one close per publication and one remove per
/// endpoint, since an add that finds no room in the sender's queue is refused with code
/// 10 and never deferred (ADR 0041).
const SENDER_DEFERRED_CAPACITY: usize = spec::MAX_PUBLICATIONS + spec::MAX_SEND_ENDPOINTS;

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

/// The directory protocol of `spec/client-protocol.md`, "The driver directory": a live
/// driver is `driver-active`; a dead one, a half-created one, or none, and the
/// directory is removed and created anew with create-new semantics.
pub(crate) fn prepare_dir(dir: &Path) -> Result<(), DriverError> {
    let io = |what: &'static str, source: std::io::Error| DriverError::Io {
        what,
        path: dir.to_path_buf(),
        source,
    };
    if dir.exists() {
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
        std::fs::remove_dir_all(dir)
            .map_err(|source| io("remove the dead driver's directory", source))?;
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|source| io("create the parent directory", source))?;
    }
    std::fs::create_dir(dir).map_err(|source| io("create the directory", source))
}

/// One client the driver knows: its id and when it last sent a command.
struct Client {
    id: i64,
    last_seen: i64,
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
    /// The producer position as last loaded, and when, by the conductor's clock: loaded
    /// at most once per [`spec::PRODUCER_POSITION_INTERVAL`] (ADR 0022, ADR 0032).
    producer: i64,
    producer_loaded_at: i64,
    /// The consumer position that has not moved, and since when.
    stalled: Option<(i64, i64)>,
    /// The end of stream position and when it was stored.
    closing: Option<(i64, i64)>,
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
    /// The sender's `sender.position` counter, loaded on the duty cycle.
    sender_position: Counter,
    /// The log's term length: the log is cleaned a term behind the sender position.
    term_length: i64,
    /// A status message arrived within the connection timeout, as the sender last said.
    connected: bool,
    /// The close was handed to the sender.
    close_sent: bool,
    /// The sender's linger ended and it dropped the publication.
    sender_closed: bool,
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
    /// Reserved for a later version that re-resolves names; never entered here.
    #[allow(dead_code)]
    Errored,
}

/// One send endpoint the conductor opened: the resolved channel, the socket's owner's
/// state, its five counters and how many publications send on it.
struct SendEndpoint {
    /// The correlation id of the `add publication` that opened it: keys `channel.status`.
    registration_id: i64,
    endpoint: SocketAddr,
    interface: Option<SocketAddr>,
    state: ChannelState,
    status: Counter,
    socket_rcvbuf: Counter,
    socket_sndbuf: Counter,
    images: Counter,
    publications: Counter,
    publication_count: usize,
}

/// One subscription: a client's interest in a stream.
struct Subscription {
    registration_id: i64,
    client_id: i64,
    stream_id: i32,
}

/// One image: a subscription's view of one publication, with the position counter the
/// client owns.
struct Image {
    publication_id: i64,
    subscription_id: i64,
    position: Counter,
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
    /// Since when the replay's publication has had no image, for the connect timeout.
    unconnected_since: Option<i64>,
    /// A stop is with the replayer; a second `stop replay` is answered at once.
    stop_requested: bool,
}

/// One recording the conductor registers: its publication, the command that started
/// it, its start position, and, once the recorder has given the id, the two counters.
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
    recording_segments_purged: Counter,
    recording_deleted: Counter,
    recording_retention_failed: Counter,
    recording_start_failed: Counter,
    recording_catalog_full: Counter,
    replay_started: Counter,
    replay_stopped: Counter,
    replay_ended: Counter,
    replay_not_connected: Counter,
    replay_failed: Counter,
    replay_start_failed: Counter,
    channel_opened: Counter,
    channel_closed: Counter,
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
            recording_segments_purged: get(RECORDING_SEGMENTS_PURGED)?,
            recording_deleted: get(RECORDING_DELETED)?,
            recording_retention_failed: get(RECORDING_RETENTION_FAILED)?,
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
    publications: Vec<Publication>,
    subscriptions: Vec<Subscription>,
    images: Vec<Image>,
    recordings: Vec<RecordingEntry>,
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
    /// Registrations across every publication: the publications registry's fill.
    registration_count: usize,
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
    ) -> Result<Self, DriverError> {
        let counters = CountersFile::create(dir, spec::COUNTERS_CAPACITY)?;
        let statics = Statics::new(&counters)?;
        counters
            .counter(CONDUCTOR_IDLE_STRATEGY.record())?
            .set(idle.counter_value());
        let ring = CommandRing::create(dir, spec::COMMAND_RING_CAPACITY)?;
        let consumer = ring.consumer(spec::COMMAND_RING_UNBLOCK_TIMEOUT)?;
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
            // A replay's publication has no registration, so the publications sit
            // beside the registrations' bound.
            publications: Vec::with_capacity(
                spec::MAX_PUBLICATIONS + spec::MAX_REPLAY_PUBLICATIONS,
            ),
            subscriptions: Vec::with_capacity(spec::MAX_SUBSCRIPTIONS),
            images: Vec::with_capacity(spec::MAX_IMAGES),
            recordings: Vec::with_capacity(spec::MAX_RECORDINGS),
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
            registration_count: 0,
            next_session_id: i32::from_le_bytes(
                (now >> 8).to_le_bytes()[..4].try_into().unwrap_or([0; 4]),
            ),
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
        work += self.take_sender_replies(now);
        work += self.flush_sender_deferred();
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
            } => self.add_subscription(client_id, correlation_id, stream_id, channel),
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
        }
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
        let Some(channel) = self.check_channel(correlation_id, channel, "the channel", true) else {
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
            producer: join_position,
            producer_loaded_at: now,
            stalled: None,
            closing: None,
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
            self.add_image(publication_index, subscription_id);
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
        let host = channel.endpoint().host();
        let endpoint = match resolve(host, channel.endpoint_port()) {
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
                return;
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
                    return;
                }
            },
        };
        let registration = Registration {
            client_id,
            correlation_id,
        };
        let open = self.send_endpoints.iter().position(|entry| {
            entry.endpoint == endpoint
                && entry.interface == interface
                && entry.state == ChannelState::Active
        });
        // A shared publication on the same stream and resolved channel joins the first.
        if !exclusive && let Some(endpoint_index) = open {
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
        if !self.sender_deferred.is_empty()
            || self.sender.requests.capacity() - self.sender.requests.len() < 2
        {
            self.sender_queue_full(correlation_id);
            return;
        }
        // The endpoint: the open one on the resolved channel, or a new one.
        let (endpoint_index, socket) = match open {
            Some(index) => (index, None),
            None => {
                if self.send_endpoints.len() >= spec::MAX_SEND_ENDPOINTS {
                    self.registry_full(correlation_id, "send endpoints", spec::MAX_SEND_ENDPOINTS);
                    return;
                }
                let socket = match SendSocket::open(endpoint, interface) {
                    Ok(socket) => socket,
                    Err(error) => {
                        self.send_error(
                            correlation_id,
                            ErrorCode::ChannelEndpointError,
                            &format!(
                                "the channel's endpoint {endpoint}{}: the socket could not be \
                                 opened, bound or connected: {error}; check that the interface \
                                 is this host's and its port is free; see {ERRORS_DOC}",
                                interface.map_or(String::new(), |i| format!(" from interface {i}"))
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
                            let reuse =
                                now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
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
                self.send_endpoints.push(SendEndpoint {
                    registration_id: correlation_id,
                    endpoint,
                    interface,
                    state: ChannelState::Initializing,
                    status,
                    socket_rcvbuf,
                    socket_sndbuf,
                    images,
                    publications,
                    publication_count: 0,
                });
                (self.send_endpoints.len() - 1, Some(socket))
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
        // The spec's UDP term, or the smaller one a test at a registry's capacity set,
        // since every log is made resident when it is created (ADR 0027).
        let term_length = self.term_length.min(spec::UDP_TERM_LENGTH);
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
        let log = match fields.and_then(|fields| LogBuffers::create(&self.dir, &fields)) {
            Ok(log) => log,
            Err(error) => {
                self.statics.log_create_failed.increment();
                self.abandon_send_endpoint(endpoint_index, socket.is_some(), reuse);
                self.send_error(correlation_id, ErrorCode::StorageSpace, &format!("{error}"));
                return;
            }
        };
        let key = position_key(correlation_id, session_id, stream_id);
        // The sender's two labels end in the endpoint's registration id, so that `doctor`
        // names a publication's endpoint from the counters file alone.
        let label = |name: &str, endpoint: bool| {
            if endpoint {
                format!("{name} {correlation_id} {session_id} {stream_id} {endpoint_id}")
            } else {
                format!("{name} {correlation_id} {session_id} {stream_id}")
            }
        };
        let mut allocated: Vec<Counter> = Vec::with_capacity(4);
        for (id, name, endpoint) in [
            (PUBLICATION_POSITION, "publication.position", false),
            (PUBLICATION_LIMIT, "publication.limit", false),
            (SENDER_POSITION, "sender.position", true),
            (SENDER_LIMIT, "sender.limit", true),
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
        let (Some(position), Some(limit), Some(sender_position), Some(sender_limit)) = (
            counters.next(),
            counters.next(),
            counters.next(),
            counters.next(),
        ) else {
            return;
        };
        let reader = match log.reader(0) {
            Ok(reader) => reader,
            Err(error) => {
                for counter in [&position, &limit, &sender_position, &sender_limit] {
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
        position.store_release(0);
        limit.store_release(0);
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
            consumer_position: 0,
            clean_position: 0,
            last_limit: 0,
            producer: 0,
            producer_loaded_at: now,
            stalled: None,
            closing: None,
            log_file_name,
            udp: Some(UdpPublication {
                endpoint_id,
                sender_position: sender_position.clone(),
                term_length: i64::from(term_length),
                connected: false,
                close_sent: false,
                sender_closed: false,
            }),
        };
        // To the sender: the endpoint first when it is new, then the publication; the
        // room was checked above, so neither offer fails.
        if let Some(socket) = socket {
            if self
                .sender
                .requests
                .offer(sender::Request::AddEndpoint {
                    registration_id: endpoint_id,
                    socket,
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
        };
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
        let status_id = counter_id(&entry.status);
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
        ] {
            let _ = self.counters.free(counter.id(), reuse);
        }
    }

    /// Answers `add publication` with error code 10 when the sender's request queue has
    /// no room for an endpoint and a publication, or requests are still waiting for it.
    fn sender_queue_full(&mut self, correlation_id: i64) {
        self.statics.registry_full.increment();
        self.send_error(
            correlation_id,
            ErrorCode::ResourceTemporarilyUnavailable,
            &format!(
                "the sender's request queue holds {} and is full; try again once the sender \
                 has caught up; see {ERRORS_DOC}",
                spec::SENDER_QUEUE_CAPACITY
            ),
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
    /// limit and stored in the log's connected flag for the publisher.
    fn set_udp_connected(&mut self, registration_id: i64, connected: bool) {
        if let Some(publication) = self
            .publications
            .iter_mut()
            .find(|publication| publication.registration_id == registration_id)
            && let Some(udp) = &mut publication.udp
        {
            udp.connected = connected;
            let _ = publication.log.set_connected(connected);
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
    /// and an `available image` event.
    fn add_image(&mut self, publication_index: usize, subscription_id: i64) {
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
        let _ = publication.log.set_connected(true);
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
    ) {
        if self
            .check_channel(correlation_id, channel, "the channel", false)
            .is_none()
        {
            return;
        }
        if self.subscriptions.len() >= spec::MAX_SUBSCRIPTIONS {
            self.registry_full(correlation_id, "subscriptions", spec::MAX_SUBSCRIPTIONS);
            return;
        }
        self.subscriptions.push(Subscription {
            registration_id: correlation_id,
            client_id,
            stream_id,
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
            self.add_image(index, correlation_id);
        }
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

    /// Removes the subscription at `index` and every image it has, freeing their counters.
    fn drop_subscription(&mut self, index: usize, now: i64) {
        let subscription = self.subscriptions.swap_remove(index);
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
        for publication in &mut self.publications {
            if publication.udp.is_some() {
                continue;
            }
            let id = publication.registration_id;
            let connected = self.images.iter().any(|image| image.publication_id == id)
                || self
                    .recordings
                    .iter()
                    .any(|recording| recording.publication_id == id);
            let _ = publication.log.set_connected(connected);
        }
    }

    /// Client close and client timeout: every registration and subscription of the
    /// client goes, as if removed, and the client is forgotten.
    fn remove_client(&mut self, client_id: i64, now: i64) {
        self.clients.retain(|client| client.id != client_id);
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
        if raise || lower {
            publication.limit.store_release(candidate);
            publication.last_limit = candidate;
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
    /// stream or the linger has passed: unavailable image events, counters freed, the
    /// log removed. Returns whether it was removed.
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
        let publication = self.publications.swap_remove(index);
        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
        let images = std::mem::take(&mut self.images);
        let mut kept = Vec::with_capacity(spec::MAX_IMAGES);
        for image in images {
            if image.publication_id == publication_id {
                let event = Event::UnavailableImage {
                    correlation_id: publication_id,
                    subscriber_registration_id: image.subscription_id,
                    stream_id: publication.stream_id,
                    channel: IPC_CHANNEL,
                };
                self.send(&event);
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

    /// `start recording` (`spec/client-protocol.md`, "Commands"): of the live
    /// publications on the stream, the one with the lowest registration id that is not
    /// yet being recorded; none live is code 20, every one recorded is code 22, the
    /// registry or the recorder's queue full is code 10. The recording starts at the
    /// producer position loaded afresh, the limit respects it from now on, and the
    /// recorder is asked for the catalog slot; `recording ready` follows its reply.
    /// The channel check every command that carries one runs first (`spec/udp.md`,
    /// "What a reader rejects"; ADR 0040): `cavalry:ipc` passes; a `cavalry:udp` channel
    /// that parses passes where `udp` says the command takes one, `add publication` and
    /// `add exclusive publication` in this version (ADR 0041), and is refused elsewhere
    /// with code 11 and a message naming the change that brings it, since this driver
    /// has no receiver yet (card 181), the refusal counted in `client_protocol.errors`
    /// alone, as a well-formed command's is; a channel that fails the grammar, names a
    /// parameter this version does not, has a bad endpoint or a multicast address is
    /// code 1 with the reason, counted in `client_protocol.rejected_command`. Returns
    /// the channel when the command goes on.
    fn check_channel<'a>(
        &mut self,
        correlation_id: i64,
        channel: &'a [u8],
        what: &str,
        udp: bool,
    ) -> Option<Channel<'a>> {
        match Channel::parse(channel) {
            Ok(Channel::Ipc) => Some(Channel::Ipc),
            Ok(Channel::Udp(parsed)) if udp => Some(Channel::Udp(parsed)),
            Ok(Channel::Udp(_)) => {
                self.send_error(
                    correlation_id,
                    ErrorCode::GenericError,
                    &format!(
                        "{what} names cavalry:udp, which this driver sends on but does not \
                         receive, record or replay yet: the receiver follows (card 181); use \
                         cavalry:ipc, or upgrade the driver to a release that receives over \
                         UDP; see {ERRORS_DOC}"
                    ),
                );
                None
            }
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

    fn start_recording(&mut self, correlation_id: i64, stream_id: i32, channel: &[u8], now: i64) {
        if self
            .check_channel(correlation_id, channel, "the channel", false)
            .is_none()
        {
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
        });
        let _ = self.publications[index].log.set_connected(true);
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
            }
            taken += 1;
        }
        taken
    }

    /// The recorder took the slot and created the files: the two counters are
    /// allocated, keyed by the recording id and set to the start position, the recorder
    /// gets their handles, and the client gets `recording ready`. A counters file with
    /// no free record fails the start with code 10 and asks the recorder to stop the
    /// recording (`fail_start`).
    fn on_ready(&mut self, publication_id: i64, correlation_id: i64, recording_id: i64, now: i64) {
        let Some(index) = self.recordings.iter().position(|recording| {
            recording.publication_id == publication_id
                && recording.state == RecordingState::Starting
        }) else {
            return;
        };
        let (session_id, stream_id, start_position) = {
            let entry = &self.recordings[index];
            (entry.session_id, entry.stream_id, entry.start_position)
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
    }

    /// A start the counters file refused after the slot was taken: the client gets
    /// code 10 and the entry fails, but the recorder is already reading the log and
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
    /// client gets code 10 (catalog full) or 12 (files), each with its counter.
    fn on_start_failed(&mut self, publication_id: i64, correlation_id: i64, failure: StartFailure) {
        if let Some(index) = self.recordings.iter().position(|recording| {
            recording.publication_id == publication_id
                && recording.state == RecordingState::Starting
        }) {
            self.recordings[index].state = RecordingState::Failed;
            self.recordings.swap_remove(index);
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
        self.refresh_connected(publication_id);
    }

    /// The recorder stopped a recording: its entry goes, its counters are freed after
    /// the reuse timeout, a command that asked gets `operation success`, and every
    /// client gets `recording stopped`.
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
        self.statics.recording_stopped.increment();
        if let Some(correlation_id) = correlation_id {
            self.send(&Event::OperationSuccess { correlation_id });
        }
        self.send(&Event::RecordingStopped {
            recording_id,
            stop_position,
        });
        self.refresh_connected(publication_id);
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

    /// Sets is connected for one publication from its images and its recording.
    fn refresh_connected(&mut self, publication_id: i64) {
        let connected = self
            .images
            .iter()
            .any(|image| image.publication_id == publication_id)
            || self
                .recordings
                .iter()
                .any(|recording| recording.publication_id == publication_id);
        if let Some(publication) = self
            .publications
            .iter_mut()
            .find(|publication| publication.registration_id == publication_id)
            && publication.udp.is_none()
        {
            let _ = publication.log.set_connected(connected);
        }
    }

    /// `start replay` (`spec/client-protocol.md`, "Commands"): the channel, the length,
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
        if self
            .check_channel(correlation_id, channel, "the replay channel", false)
            .is_none()
        {
            return;
        }
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
    /// stream (`spec/client-protocol.md`, "Commands"). A log that cannot be created is
    /// code 12, a counters file with no free record code 10; either removes the replay.
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
        let session_id = self.advance_session_id(stream_id);
        let term_length = u32::try_from(term_length).unwrap_or(0);
        let fields = HeaderFields::new(
            LogType::Exclusive,
            replay_session_id,
            initial_term_id,
            term_length,
            u32::try_from(mtu_length).unwrap_or(0),
            term_length / 2,
            session_id,
            stream_id,
        );
        let log =
            fields.and_then(|fields| LogBuffers::create_at(&self.dir, &fields, start_position));
        let log = match log {
            Ok(log) => log,
            Err(error) => {
                remove_log(&self.dir, replay_session_id);
                self.statics.log_create_failed.increment();
                self.fail_replay(index, ErrorCode::StorageSpace, &format!("{error}"));
                return;
            }
        };
        let key = position_key(replay_session_id, session_id, stream_id);
        let label = |name: &str| format!("{name} {replay_session_id} {session_id} {stream_id}");
        let reuse = now.saturating_add(spec::nanos(spec::COUNTER_REUSE_TIMEOUT));
        let position =
            match self
                .counters
                .allocate(PUBLICATION_POSITION, &key, &label("publication.position"))
            {
                Ok(counter) => counter,
                Err(error) => {
                    drop(log);
                    remove_log(&self.dir, replay_session_id);
                    self.statics.registry_full.increment();
                    self.fail_replay(
                        index,
                        ErrorCode::ResourceTemporarilyUnavailable,
                        &format!("{error}"),
                    );
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
                    let _ = self.counters.free(position.id(), reuse);
                    drop(log);
                    remove_log(&self.dir, replay_session_id);
                    self.statics.registry_full.increment();
                    self.fail_replay(
                        index,
                        ErrorCode::ResourceTemporarilyUnavailable,
                        &format!("{error}"),
                    );
                    return;
                }
            };
        let replay_label =
            format!("replay.position {replay_session_id} {recording_id} {session_id} {stream_id}");
        let replay_position = match self.counters.allocate(
            REPLAY_POSITION,
            &replay_session_id.to_le_bytes(),
            &replay_label,
        ) {
            Ok(counter) => counter,
            Err(error) => {
                let _ = self.counters.free(position.id(), reuse);
                let _ = self.counters.free(limit.id(), reuse);
                drop(log);
                remove_log(&self.dir, replay_session_id);
                self.statics.registry_full.increment();
                self.fail_replay(
                    index,
                    ErrorCode::ResourceTemporarilyUnavailable,
                    &format!("{error}"),
                );
                return;
            }
        };
        let writer = match log.writer(recorded_session_id, recorded_stream_id) {
            Ok(writer) => writer,
            Err(error) => {
                let _ = self.counters.free(position.id(), reuse);
                let _ = self.counters.free(limit.id(), reuse);
                let _ = self.counters.free(replay_position.id(), reuse);
                drop(log);
                remove_log(&self.dir, replay_session_id);
                self.statics.log_create_failed.increment();
                self.fail_replay(index, ErrorCode::StorageSpace, &format!("{error}"));
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
            producer: start_position,
            producer_loaded_at: now,
            stalled: None,
            closing: None,
            log_file_name,
            udp: None,
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
        let matching: Vec<i64> = self
            .subscriptions
            .iter()
            .filter(|subscription| subscription.stream_id == stream_id)
            .map(|subscription| subscription.registration_id)
            .collect();
        for subscription_id in matching {
            self.add_image(publication_index, subscription_id);
        }
        if self
            .images
            .iter()
            .any(|image| image.publication_id == replay_session_id)
        {
            self.replays[index].unconnected_since = None;
        }
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
        if let Some(publication) = self
            .publications
            .iter_mut()
            .find(|publication| publication.registration_id == replay_session_id)
            && publication.closing.is_none()
        {
            end = publication.log.producer_position().unwrap_or(position);
            let _ = publication.log.set_end_of_stream_position(end);
            publication.closing = Some((end, now));
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
            let connected = self.images.iter().any(|image| image.publication_id == id);
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
            &format!(
                "the driver's {registry} registry holds {capacity} and is full; remove what is not \
                 used; see {ERRORS_DOC}"
            ),
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

    /// The replayer's file work ended: a truncate's last catalog write, a purge's or a
    /// delete's answer with its counter, or the repair after a file the operating
    /// system refused.
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
                    self.commands.swap_remove(index);
                    for _ in 0..done.files_removed {
                        self.statics.recording_segments_purged.increment();
                    }
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

    /// Answers `start recording` with `error`, code 10, when the recorder's request
    /// queue is full or requests are still waiting for room in it.
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
        let mut conductor = Conductor::create(
            dir.path(),
            IdleStrategy::Backoff,
            link,
            replayer_link,
            sender_link,
        )
        .expect("the conductor is created");
        let t0 = now_nanos();
        let interval = spec::nanos(spec::PRODUCER_POSITION_INTERVAL);

        // A subscriber, then a publication, loaded at t0; the first cycle raises the
        // limit, and after it nothing moves.
        conductor.add_subscription(1, 1, 7, IPC_CHANNEL);
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
            let conductor = Conductor::create(
                dir.path(),
                IdleStrategy::Backoff,
                link,
                replayer_link,
                sender_link,
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
            Harness {
                dir,
                log_dir,
                conductor,
                recorder,
                replayer,
                sender,
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
            self.conductor.duty_cycle(self.now);
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
        ControlFrame, STATUS_MESSAGE_END_OF_STREAM, SetupFrame, StatusMessage, parse as parse_udp,
    };
    use cavalry_shm::counters::generated::{
        PUBLICATION_CONNECTED, SENDER_SETUPS_SENT, SENDER_STATUS_MESSAGES_RECEIVED,
    };

    /// A receiver by hand: a socket of its own that reads what the sender puts on the
    /// wire and answers as `spec/udp.md`, "The receiver" says a receiver answers.
    struct TestReceiver {
        socket: std::net::UdpSocket,
        buffer: Vec<u8>,
        peer: Option<std::net::SocketAddr>,
    }

    impl TestReceiver {
        fn bind() -> Self {
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

        fn channel(&self) -> Vec<u8> {
            format!(
                "cavalry:udp?endpoint={}",
                self.socket.local_addr().expect("bound")
            )
            .into_bytes()
        }

        /// The next datagram as a control frame, or `None` at the read timeout.
        fn next_control(&mut self) -> Option<ControlFrame> {
            let (length, from) = self.socket.recv_from(&mut self.buffer).ok()?;
            self.peer = Some(from);
            parse_udp(&self.buffer[..length], spec::UDP_TERM_LENGTH).ok()
        }

        fn send_status_message(&self, message: &StatusMessage) {
            let bytes = message.to_bytes();
            self.socket
                .send_to(
                    &bytes[..message.frame_length()],
                    self.peer.expect("a setup frame arrived first"),
                )
                .expect("the status message is sent");
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

    /// The refusals: a host that does not resolve is code 9, an interface that is not
    /// this host's is code 4 with nothing registered, and the commands that take no
    /// `cavalry:udp` channel yet refuse it with code 11 naming card 181.
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
        h.conductor
            .add_subscription(1, 4, 7, b"cavalry:udp?endpoint=127.0.0.1:40123");
        let (code, message) = h.last_error(4).expect("an error");
        assert_eq!(code, 11, "{message}");
        assert!(message.contains("card 181"), "{message}");
        h.conductor
            .start_recording(5, 7, b"cavalry:udp?endpoint=127.0.0.1:40123", now);
        assert_eq!(h.last_error(5).map(|error| error.0), Some(11));
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
    fn offer_to(
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
        h.conductor.add_subscription(1, 5, 7, IPC_CHANNEL);
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

    use cavalry_proto::client_protocol::{FOLLOW_LENGTH, NULL_LENGTH, NULL_POSITION};
    use cavalry_shm::counters::generated::{
        DRIVER_REGISTRY_FULL, LOG_BUFFER_REJECTED_FRAME, RECORDING_DELETED,
        RECORDING_RETENTION_FAILED, RECORDING_SEGMENTS_PURGED, RECORDING_TRUNCATED, REPLAY_ENDED,
        REPLAY_FAILED, REPLAY_NOT_CONNECTED, REPLAY_START_FAILED, REPLAY_STARTED, REPLAY_STOPPED,
    };
    use cavalry_shm::log::ClaimError;

    use super::recording::Harness;
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
                .add_subscription(CLIENT, registration, REPLAY_STREAM, IPC_CHANNEL);
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
        // The subscriber stood at the end from the start, so the publication closed at
        // once: the image went and the empty log was removed.
        assert!(
            h.conductor
                .publications
                .iter()
                .all(|publication| publication.registration_id != 6)
        );
        assert!(!h.dir.path().join("publications/6.logbuffer").exists());
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
            .add_subscription(CLIENT, 20, STREAM, IPC_CHANNEL);
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
            .add_subscription(CLIENT, 5, REPLAY_STREAM, IPC_CHANNEL);
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
}
