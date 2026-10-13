//! The conductor: the one thread that keeps the driver's directory. On every duty cycle
//! it stores its heartbeat, reads and answers the command ring, unblocks it, times out
//! clients, and keeps every publication's positions, cleaning, unblocking and closing
//! as `spec/log-buffer.md`, "The driver" says; what each command means is
//! `spec/client-protocol.md` (ADR 0019). Nothing here is on the message path: a
//! message goes from a publisher's claim to a subscriber's handler through the log
//! buffer alone, and the conductor only moves the limit that bounds the publisher. It
//! loads a publication's producer position at most once per millisecond, so that the
//! publisher's cache line is not read on every cycle, and a move of that position is
//! not work for the idle strategy (ADR 0022).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cavalry_proto::client_protocol::{
    Command, ErrorCode, Event, IPC_CHANNEL, MAX_STRING_LENGTH, ProtocolError,
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
    PUBLICATION_POSITION, PUBLICATION_UNBLOCKED, SUBSCRIPTION_POSITION,
};
use cavalry_shm::counters::{Counter, CounterId, CountersError, CountersFile};
use cavalry_shm::log::layout as log_layout;
use cavalry_shm::log::{HeaderFields, LogBuffers, LogType, UnblockOutcome as LogUnblock};

use crate::error::DriverError;
use crate::idle::{IdleStrategy, Idler};
use crate::spec;

/// The docs anchor an error event's message names.
const ERRORS_DOC: &str = "docs/reference/client-protocol.md#error-codes";

/// How many commands one duty cycle reads before it turns to the publications.
const COMMANDS_PER_CYCLE: usize = 16;

/// A frame's alignment as the positions count.
const FRAME_ALIGN: i64 = FRAME_ALIGNMENT as i64;

/// How long a starter waits for a command ring that is `not-ready` before it treats the
/// file as a dead driver's, polling at this interval.
const NOT_READY_POLL: Duration = Duration::from_millis(10);

/// The driver's clock: nanoseconds since the Unix epoch, the clock the consumer
/// heartbeat stores and every timeout is measured on.
fn now_nanos() -> i64 {
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
        })
    }
}

/// The longest command of this version: client id, correlation id, stream id, and a
/// channel of [`MAX_STRING_LENGTH`] bytes with its length (`spec/client-protocol.md`,
/// "Commands"). A record longer than this is no command, and is refused, by the spec's
/// checks run on its first this many bytes, without being copied past this.
const MAX_COMMAND_LENGTH: usize = 8 + 8 + 4 + 4 + MAX_STRING_LENGTH;

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
    /// in that order, with the sizes of [`spec`], and records `idle` in the
    /// `conductor.idle_strategy` counter for `doctor`.
    pub(crate) fn create(dir: &Path, idle: IdleStrategy) -> Result<Self, DriverError> {
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
            publications: Vec::with_capacity(spec::MAX_PUBLICATIONS),
            subscriptions: Vec::with_capacity(spec::MAX_SUBSCRIPTIONS),
            images: Vec::with_capacity(spec::MAX_IMAGES),
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
        let fields = match HeaderFields::new(
            log_type,
            correlation_id,
            initial_term_id,
            spec::IPC_TERM_LENGTH,
            spec::IPC_MTU_LENGTH,
            spec::IPC_PUBLICATION_WINDOW,
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
            let connected = self
                .images
                .iter()
                .any(|image| image.publication_id == publication.registration_id);
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
        // The slowest subscriber bounds everything below; with none, the largest
        // position any subscriber reached, so that the publisher is told not connected.
        let consumer = min_subscriber.unwrap_or(publication.consumer_position);
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
        // consumer position alone with no subscriber, so the publisher is told not connected.
        let window = i64::from(spec::IPC_PUBLICATION_WINDOW);
        let candidate = if min_subscriber.is_some() {
            consumer.saturating_add(window)
        } else {
            consumer
        };
        let step = window / 8;
        let raise = candidate > publication.last_limit
            && (candidate - publication.last_limit >= step || min_subscriber.is_none())
            && publication.clean_position >= consumer;
        let lower = candidate < publication.last_limit && min_subscriber.is_none();
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
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(test: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("cavalry-driver-{}-{test}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            TestDir(path)
        }

        fn path(&self) -> &Path {
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
        let mut conductor =
            Conductor::create(dir.path(), IdleStrategy::Backoff).expect("the conductor is created");
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
