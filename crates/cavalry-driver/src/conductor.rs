//! The conductor: the one thread that keeps the driver's directory. On every duty cycle
//! it stores its heartbeat, reads and answers the command ring, unblocks it, times out
//! clients, and keeps every publication's positions, cleaning, unblocking and closing
//! as `spec/log-buffer.md`, "The driver" says; what each command means is
//! `spec/client-protocol.md` (ADR 0019). Nothing here is on the message path: a
//! message goes from a publisher's claim to a subscriber's handler through the log
//! buffer alone, and the conductor only moves the limit that bounds the publisher. It
//! loads a publication's producer position at most once per millisecond, so that the
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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cavalry_log::dir::Recovery;

use cavalry_proto::client_protocol::{
    Command, ErrorCode, Event, IPC_CHANNEL, MAX_STRING_LENGTH, NULL_LENGTH, ProtocolError,
    ReplayStopReason,
};
use cavalry_proto::frame::FRAME_ALIGNMENT;
use cavalry_shm::broadcast::{BroadcastBuffer, Transmitter};
use cavalry_shm::command_ring::{
    CommandRing, CommandRingError, Consumer, UnblockOutcome as RingUnblock,
};
use cavalry_shm::counters::generated::{
    CLIENT_PROTOCOL_ERRORS, CLIENT_PROTOCOL_REJECTED_COMMAND, CLIENT_TIMED_OUT,
    COMMAND_RING_REJECTED_RECORD, COMMAND_RING_REJECTED_TAIL, COMMAND_RING_UNBLOCKED,
    CONDUCTOR_IDLE_STRATEGY, DRIVER_LOG_CREATE_FAILED, DRIVER_REGISTRY_FULL,
    LOG_BUFFER_REJECTED_POSITION, LOG_BUFFER_REJECTED_TAIL, PUBLICATION_LIMIT,
    PUBLICATION_POSITION, PUBLICATION_UNBLOCKED, RECORDING_CATALOG_FULL,
    RECORDING_CATALOG_TRUNCATED, RECORDING_FILES_REMOVED, RECORDING_INDEX_REWRITTEN,
    RECORDING_INVALIDATED, RECORDING_POSITION, RECORDING_RECOVERED, RECORDING_SLOT_DROPPED,
    RECORDING_START_FAILED, RECORDING_STARTED, RECORDING_STOPPED, RECORDING_SYNCED_POSITION,
    REPLAY_ENDED, REPLAY_FAILED, REPLAY_NOT_CONNECTED, REPLAY_POSITION, REPLAY_START_FAILED,
    REPLAY_STARTED, REPLAY_STOPPED, SUBSCRIPTION_POSITION,
};
use cavalry_shm::counters::{Counter, CounterId, CountersError, CountersFile};
use cavalry_shm::log::layout as log_layout;
use cavalry_shm::log::{HeaderFields, LogBuffers, LogType, UnblockOutcome as LogUnblock};
use cavalry_shm::spsc;

use crate::error::DriverError;
use crate::idle::{IdleStrategy, Idler};
use crate::recorder::{Reply, Request, Start, StartFailure};
use crate::replayer;
use crate::spec;

/// The docs anchor an error event's message names.
const ERRORS_DOC: &str = "docs/reference/client-protocol.md#error-codes";

/// How many commands one duty cycle reads before it turns to the publications.
const COMMANDS_PER_CYCLE: usize = 16;

/// The deferred requests' bound: one attach and one stop per registered recording
/// (`Conductor::deferred` says why it holds).
const DEFERRED_CAPACITY: usize = 2 * spec::MAX_RECORDINGS;

/// The replayer's deferred requests' bound: one attach and one stop per registered
/// replay, by the argument on `Conductor::deferred`.
const REPLAYER_DEFERRED_CAPACITY: usize = 2 * spec::MAX_REPLAYS;

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
    /// at most once per [`spec::PRODUCER_POSITION_INTERVAL`] (ADR 0022).
    producer: i64,
    producer_loaded_at: i64,
    /// The consumer position that has not moved, and since when.
    stalled: Option<(i64, i64)>,
    /// The end of stream position and when it was stored.
    closing: Option<(i64, i64)>,
    log_file_name: String,
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
    /// A stop is with the recorder; a second `stop recording` is answered at once.
    stop_requested: bool,
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
    recording_start_failed: Counter,
    recording_catalog_full: Counter,
    replay_started: Counter,
    replay_stopped: Counter,
    replay_ended: Counter,
    replay_not_connected: Counter,
    replay_failed: Counter,
    replay_start_failed: Counter,
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
            recording_start_failed: get(RECORDING_START_FAILED)?,
            recording_catalog_full: get(RECORDING_CATALOG_FULL)?,
            replay_started: get(REPLAY_STARTED)?,
            replay_stopped: get(REPLAY_STOPPED)?,
            replay_ended: get(REPLAY_ENDED)?,
            replay_not_connected: get(REPLAY_NOT_CONNECTED)?,
            replay_failed: get(REPLAY_FAILED)?,
            replay_start_failed: get(REPLAY_START_FAILED)?,
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
    /// The replayer's attaches and stops its request queue had no room for, kept and
    /// bounded as `deferred` is; while it holds any, `start replay` is code 10.
    replayer_deferred: VecDeque<replayer::Request>,
    /// Attaches and stops the request queue had no room for, offered again each cycle
    /// in order, before anything newer. While it holds any, `start recording` is
    /// answered with code 10, so no recording is registered until it empties; every
    /// request it takes meanwhile is one attach or one stop of a recording already
    /// registered, at most [`spec::MAX_RECORDINGS`], so it never holds more than two per
    /// recording, its capacity.
    deferred: VecDeque<Request>,
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
            publications: Vec::with_capacity(spec::MAX_PUBLICATIONS + spec::MAX_REPLAYS),
            subscriptions: Vec::with_capacity(spec::MAX_SUBSCRIPTIONS),
            images: Vec::with_capacity(spec::MAX_IMAGES),
            recordings: Vec::with_capacity(spec::MAX_RECORDINGS),
            replays: Vec::with_capacity(spec::MAX_REPLAYS),
            recorder,
            replayer,
            replayer_thread: None,
            replayer_deferred: VecDeque::with_capacity(REPLAYER_DEFERRED_CAPACITY),
            term_length: spec::IPC_TERM_LENGTH,
            deferred: VecDeque::with_capacity(DEFERRED_CAPACITY),
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
        if channel != IPC_CHANNEL {
            self.statics.rejected_command.increment();
            self.send_error(
                correlation_id,
                ErrorCode::InvalidChannel,
                &format!("the channel must be cavalry:ipc in this version; see {ERRORS_DOC}"),
            );
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
        };
        self.registration_count += 1;
        let ready = Event::PublicationReady {
            correlation_id,
            registration_id: correlation_id,
            session_id,
            stream_id,
            publication_limit_counter_id: counter_id(&publication.limit),
            log_file_name: publication.log_file_name.as_bytes(),
        };
        let exclusive_ready = Event::ExclusivePublicationReady {
            correlation_id,
            registration_id: correlation_id,
            session_id,
            stream_id,
            publication_limit_counter_id: counter_id(&publication.limit),
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
        if publication.registrations.is_empty() && publication.closing.is_none() {
            let end = publication
                .log
                .producer_position()
                .unwrap_or(publication.consumer_position);
            let _ = publication.log.set_end_of_stream_position(end);
            publication.closing = Some((end, now));
        }
    }

    fn add_subscription(
        &mut self,
        client_id: i64,
        correlation_id: i64,
        stream_id: i32,
        channel: &[u8],
    ) {
        if channel != IPC_CHANNEL {
            self.statics.rejected_command.increment();
            self.send_error(
                correlation_id,
                ErrorCode::InvalidChannel,
                &format!("the channel must be cavalry:ipc in this version; see {ERRORS_DOC}"),
            );
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
        self.send(&Event::SubscriptionReady { correlation_id });
        let matching: Vec<usize> = self
            .publications
            .iter()
            .enumerate()
            .filter(|(_, publication)| {
                publication.stream_id == stream_id && publication.closing.is_none()
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
    /// at most once per millisecond per publication, and its move is not work (ADR 0022).
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
        // The producer position at most once per millisecond (ADR 0022): the publisher
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
                    }
                    return true;
                }
            }
        }
        let producer = publication.producer;
        let mut moved = false;
        if publication.position.get() != producer {
            // Not work: a publisher that publishes while nothing else moves lets the
            // conductor idle, and the gauge is at most a millisecond behind (ADR 0022).
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
        let floor = match (min_subscriber, recording_floor) {
            (Some(subscriber), Some(recorder)) => Some(subscriber.min(recorder)),
            (subscriber, recorder) => subscriber.or(recorder),
        };
        let connected = floor.is_some();
        let consumer = floor.unwrap_or(publication.consumer_position);
        // Cleaning behind the slowest subscriber, one term at a time; a clean that does
        // not advance is not work.
        if publication.clean_position < consumer
            && let Ok(cleaned) = publication.log.clean(publication.clean_position, consumer)
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
            && publication.clean_position >= consumer;
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
        true
    }

    /// `start recording` (`spec/client-protocol.md`, "Commands"): of the live
    /// publications on the stream, the one with the lowest registration id that is not
    /// yet being recorded; none live is code 20, every one recorded is code 22, the
    /// registry or the recorder's queue full is code 10. The recording starts at the
    /// producer position loaded afresh, the limit respects it from now on, and the
    /// recorder is asked for the catalog slot; `recording ready` follows its reply.
    fn start_recording(&mut self, correlation_id: i64, stream_id: i32, channel: &[u8], now: i64) {
        if channel != IPC_CHANNEL {
            self.statics.rejected_command.increment();
            self.send_error(
                correlation_id,
                ErrorCode::InvalidChannel,
                &format!("the channel must be cavalry:ipc in this version; see {ERRORS_DOC}"),
            );
            return;
        }
        let mut live = false;
        let mut lowest: Option<usize> = None;
        let mut chosen: Option<usize> = None;
        for (index, publication) in self.publications.iter().enumerate() {
            if publication.stream_id != stream_id || publication.closing.is_some() {
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
        position.store_release(start_position);
        synced.store_release(start_position);
        let (position_id, synced_id) = (counter_id(&position), counter_id(&synced));
        {
            let entry = &mut self.recordings[index];
            entry.state = RecordingState::Live;
            entry.recording_id = Some(recording_id);
            entry.position = Some(position.clone());
            entry.synced = Some(synced.clone());
        }
        self.statics.recording_started.increment();
        self.request(Request::Attach {
            recording_id,
            position,
            synced,
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
            for counter in [entry.position, entry.synced].into_iter().flatten() {
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
        {
            let _ = publication.log.set_connected(connected);
        }
    }

    /// `start replay` (`spec/client-protocol.md`, "Commands"): the channel, the length
    /// and the registry are checked here; the recording's slot, the position and the
    /// segment file are the replayer's, whose reply creates the publication
    /// (`on_replay_ready`) or answers with code 21, 23 or 12 (`on_replay_start_failed`).
    /// The replay session id is the command's correlation id.
    fn start_replay(
        &mut self,
        correlation_id: i64,
        recording_id: i64,
        position: i64,
        length: i64,
        replay_stream_id: i32,
        channel: &[u8],
    ) {
        if channel != IPC_CHANNEL {
            self.statics.rejected_command.increment();
            self.send_error(
                correlation_id,
                ErrorCode::InvalidChannel,
                &format!(
                    "the replay channel must be cavalry:ipc in this version; see {ERRORS_DOC}"
                ),
            );
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
        if self.replays.len() >= spec::MAX_REPLAYS {
            self.registry_full(correlation_id, "replays", spec::MAX_REPLAYS);
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
            "the replayer's deferred requests passed one attach and one stop per replay"
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
    /// code 21, 23 or 12, each counted in `replay.start_failed`.
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
                     recording's start position and extent; resolve an instant through the time \
                     index, or pass -1 for the start position; see {ERRORS_DOC}"
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
        let mut conductor =
            Conductor::create(dir.path(), IdleStrategy::Backoff, link, replayer_link)
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
            let conductor =
                Conductor::create(dir.path(), IdleStrategy::Backoff, link, replayer_link)
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
            Harness {
                dir,
                log_dir,
                conductor,
                recorder,
                replayer,
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
        DRIVER_REGISTRY_FULL, LOG_BUFFER_REJECTED_FRAME, REPLAY_ENDED, REPLAY_FAILED,
        REPLAY_NOT_CONNECTED, REPLAY_START_FAILED, REPLAY_STARTED, REPLAY_STOPPED,
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

    /// A replay that follows a live recording (invariant 1): it plays what is synced
    /// and no more, moves when the recorder syncs, and ends at the recording's stop
    /// position with reason 0 once the catalog holds the stop; a replay with length −1
    /// of the same live recording ends at the extent it started with.
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

        // The recording stops: the replay learns it from the catalog within a second.
        stop_recording(&mut h, 4);
        for _ in 0..20 {
            h.cycle();
        }
        assert_eq!(
            h.conductor.replays.len(),
            1,
            "not yet: the slot is read once per second"
        );
        h.now += SECOND;
        wait_replay_end(&mut h, 6);
        assert_eq!(replay_stopped(&events(&mut h), 6), Some((1280, 0)));
        assert_eq!(h.counter(REPLAY_ENDED), 2);
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
    /// replays, each given its attach and its stop while the request queue is full,
    /// fills the list to exactly its bound and not past its allocation, and every
    /// request reaches the replayer once it catches up.
    #[test]
    fn the_replayers_deferred_requests_are_bounded_by_one_attach_and_one_stop_per_replay() {
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
                recording_id: 0,
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
                .on_replay_ready(100_000 + i, 0, 0, (0, term, 1408), (0, STREAM), now);
            h.conductor.stop_replay(70_000 + i, 100_000 + i);
        }
        assert_eq!(
            h.conductor.replayer_deferred.len(),
            REPLAYER_DEFERRED_CAPACITY
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
}
