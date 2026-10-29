//! [`Cavalry`], a process's attachment to the driver (`spec/client-protocol.md`, "The
//! driver directory"; ADR 0021).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use std::time::{Duration, Instant};

use cavalry_proto::client_protocol::{Command, ErrorCode};
use cavalry_shm::counters::generated::REPLAY_POSITION;
use cavalry_shm::broadcast::BroadcastBuffer;
use cavalry_shm::command_ring::layout::DRIVER_CLOSED;
use cavalry_shm::command_ring::{CommandRing, Producer};
use cavalry_shm::counters::{CounterValue, CountersReader, CountersWriter};
use cavalry_shm::log::LogBuffers;
use cavalry_shm::spsc;

use crate::conductor::{
    COMMAND_SCRATCH, Conductor, DRIVER_TIMEOUT, MAX_SUBSCRIPTIONS, NO_PENDING, REGISTRATION_QUEUE,
    REPLAY_QUEUE, RESPONSE_QUEUE, RESPONSE_TIMEOUT, ReplayRegistration, Shared, nanos, now_nanos,
};
use crate::error::{ClientError, Closed};
use crate::publication::Publication;
use crate::recording::Recording;
use crate::replay::{Replay, ReplayEnd};
use crate::response::Response;
use crate::subscription::{
    IMAGE_QUEUE, ImageEvent, RETIRE_QUEUE, Registration, Retired, Subscription,
};

/// The environment variable that names the driver directory (`config.toml`, `dir`;
/// ADR 0020). Clients read the same key the driver does.
pub const DIR_ENV: &str = "CAVALRY_DIR";

/// A client: one process's attachment to one driver. [`Cavalry::connect`] opens the
/// driver directory's files, takes a client id and starts the client conductor thread,
/// which keeps the client alive with the driver and reads every event. Publications and
/// subscriptions are added from here. [`Cavalry::close`] tells the driver and stops the
/// conductor; dropping a client does the same.
pub struct Cavalry {
    dir: PathBuf,
    client_id: i64,
    shared: Arc<Shared>,
    producer: Producer,
    ring: CommandRing,
    counters: CountersReader,
    responses: spsc::Consumer<Response>,
    registrations: spsc::Producer<Registration>,
    replay_registrations: spsc::Producer<ReplayRegistration>,
    conductor: Option<JoinHandle<()>>,
    scratch: Vec<u8>,
}

impl Cavalry {
    /// The directory a client looks in when none is given: `CAVALRY_DIR` when it is set
    /// and not empty, else the driver's default, `/dev/shm/cavalry-<user>` on Linux
    /// when `/dev/shm` exists and `<tmp>/cavalry-<user>` otherwise
    /// (`spec/client-protocol.md`, "The driver directory"; ADR 0020).
    pub fn default_dir() -> PathBuf {
        if let Some(dir) = std::env::var_os(DIR_ENV).filter(|dir| !dir.is_empty()) {
            return PathBuf::from(dir);
        }
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "default".to_string());
        let name = format!("cavalry-{user}");
        let shm = PathBuf::from("/dev/shm");
        if cfg!(target_os = "linux") && shm.is_dir() {
            shm.join(name)
        } else {
            std::env::temp_dir().join(name)
        }
    }

    /// Connects to the driver at [`Cavalry::default_dir`].
    pub fn connect() -> Result<Self, ClientError> {
        Self::connect_to(Self::default_dir())
    }

    /// Connects to the driver at `dir`: opens the command ring writable and checks the
    /// heartbeat, the broadcast buffer read-only at its latest record, and the counters
    /// file read-only; takes a client id from the ring's correlation id counter; sends
    /// the first keepalive, so the driver knows the client; and starts the conductor.
    /// A directory with no command ring is [`ClientError::NotFound`]; a driver that
    /// closed or whose heartbeat is stale is [`ClientError::DriverGone`].
    pub fn connect_to(dir: impl AsRef<Path>) -> Result<Self, ClientError> {
        let dir = dir.as_ref().to_path_buf();
        if !dir.join("command-ring").is_file() {
            return Err(ClientError::NotFound { dir });
        }
        let ring = CommandRing::open(&dir, true)?;
        let now = now_nanos();
        let heartbeat = ring.consumer_heartbeat();
        if heartbeat == DRIVER_CLOSED {
            return Err(ClientError::DriverGone {
                dir,
                reason: Closed::DriverClosed,
            });
        }
        if now.saturating_sub(heartbeat) > nanos(DRIVER_TIMEOUT) {
            return Err(ClientError::DriverGone {
                dir,
                reason: Closed::DriverTimeout,
            });
        }
        let mut producer = ring.producer(DRIVER_TIMEOUT)?;
        let conductor_producer = ring.producer(DRIVER_TIMEOUT)?;
        let client_id = producer.next_correlation_id();
        let broadcast = BroadcastBuffer::open(&dir, false)?;
        let receiver = broadcast.receiver()?;
        let counters = CountersReader::open(&dir)?;

        let mut scratch = vec![0u8; COMMAND_SCRATCH];
        let keepalive = Command::ClientKeepalive {
            client_id,
            correlation_id: producer.next_correlation_id(),
        };
        let length = keepalive.write(&mut scratch)?;
        producer.write(
            keepalive.command_type() as i32,
            &scratch[..length],
            now_nanos(),
        )?;

        let shared = Arc::new(Shared::new(now_nanos()));
        let (response_producer, responses) = spsc::queue::<Response>(RESPONSE_QUEUE);
        let (registrations, registration_consumer) =
            spsc::queue::<Registration>(REGISTRATION_QUEUE);
        let (replay_registrations, replay_registration_consumer) =
            spsc::queue::<ReplayRegistration>(REPLAY_QUEUE);
        let conductor_ring = CommandRing::open(&dir, true)?;
        let counters_writer = CountersWriter::open(&dir)?;
        let conductor = Conductor::new(
            dir.clone(),
            conductor_ring,
            conductor_producer,
            receiver,
            counters_writer,
            Arc::clone(&shared),
            response_producer,
            registration_consumer,
            replay_registration_consumer,
            client_id,
        );
        let handle = std::thread::Builder::new()
            .name("cavalry-client-conductor".to_string())
            .spawn(move || conductor.run())
            .map_err(|source| ClientError::Io {
                what: "start the client conductor thread",
                path: dir.clone(),
                source,
            })?;
        Ok(Self {
            dir,
            client_id,
            shared,
            producer,
            ring,
            counters,
            responses,
            registrations,
            replay_registrations,
            conductor: Some(handle),
            scratch,
        })
    }

    /// Adds a subscription on `channel` and `stream_id`: one image per publication on
    /// them, joining at the publisher's position, now and as publications come and go
    /// (`spec/client-protocol.md`, "Commands"). Waits for the driver's answer up to the
    /// response timeout. The channel must be `cavalry:ipc` in this version. A client
    /// that already holds the driver's bound of subscriptions is
    /// [`ClientError::SubscriptionsFull`], and the new one is removed again.
    pub fn add_subscription(
        &mut self,
        channel: &str,
        stream_id: i32,
    ) -> Result<Subscription, ClientError> {
        let correlation_id = self.producer.next_correlation_id();
        let (images, incoming) = spsc::queue::<ImageEvent>(IMAGE_QUEUE);
        let (retired, retired_consumer) = spsc::queue::<Retired>(RETIRE_QUEUE);
        // Registered before the command goes, so that the first image, which follows the
        // answer, finds its subscription; the conductor drains this queue every cycle.
        let mut registration = Registration {
            registration_id: correlation_id,
            images,
            retired: retired_consumer,
        };
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        loop {
            match self.registrations.offer(registration) {
                Ok(()) => break,
                Err(back) => {
                    registration = back;
                    if let Some(reason) = self.shared.closed() {
                        return Err(ClientError::Closed(reason));
                    }
                    if Instant::now() >= deadline {
                        return Err(ClientError::ResponseTimeout {
                            command: "add subscription",
                        });
                    }
                    std::thread::sleep(Duration::from_micros(50));
                }
            }
        }
        let command = Command::AddSubscription {
            client_id: self.client_id,
            correlation_id,
            stream_id,
            channel: channel.as_bytes(),
        };
        // On a refusal the queue halves drop with `incoming` and `retired`, and the
        // conductor sweeps the registration on its next cycle.
        match self.request(&command, correlation_id, "add subscription")? {
            Response::SubscriptionReady { .. } => {
                // The conductor admits or refuses every queued registration before it
                // hands over an answer, and a refused one is dropped there: its halves
                // here see their peer gone (`Registry::admit`).
                let refused = incoming.peer_dropped();
                let subscription = Subscription::new(
                    correlation_id,
                    stream_id,
                    channel.to_string(),
                    self.client_id,
                    Arc::clone(&self.shared),
                    self.ring.producer(DRIVER_TIMEOUT)?,
                    incoming,
                    retired,
                );
                if !refused {
                    return Ok(subscription);
                }
                // Dropping it sends `remove subscription`, so the driver forgets it too.
                drop(subscription);
                // A conductor that stopped drops every registration: that is the close.
                Err(match self.shared.closed() {
                    Some(reason) => ClientError::Closed(reason),
                    None => ClientError::SubscriptionsFull {
                        limit: MAX_SUBSCRIPTIONS,
                    },
                })
            }
            other => Err(ClientError::Driver {
                what: format!("{other:?} in answer to add subscription"),
            }),
        }
    }

    /// Adds a publication on `channel` and `stream_id`, shared with every other
    /// publication of this driver on the same channel and stream: they write one log
    /// under one session (`spec/client-protocol.md`, "Commands"). Waits for the driver's
    /// answer up to the response timeout. The channel must be `cavalry:ipc` in this
    /// version; another is [`ClientError::Refused`] with error code 1.
    pub fn add_publication(
        &mut self,
        channel: &str,
        stream_id: i32,
    ) -> Result<Publication, ClientError> {
        self.publication(channel, stream_id, false)
    }

    /// Adds an exclusive publication on `channel` and `stream_id`: a session of its own,
    /// written by this handle alone, which is what lets it claim without a
    /// compare-and-swap.
    pub fn add_exclusive_publication(
        &mut self,
        channel: &str,
        stream_id: i32,
    ) -> Result<Publication, ClientError> {
        self.publication(channel, stream_id, true)
    }

    fn publication(
        &mut self,
        channel: &str,
        stream_id: i32,
        exclusive: bool,
    ) -> Result<Publication, ClientError> {
        let correlation_id = self.producer.next_correlation_id();
        let command = if exclusive {
            Command::AddExclusivePublication {
                client_id: self.client_id,
                correlation_id,
                stream_id,
                channel: channel.as_bytes(),
            }
        } else {
            Command::AddPublication {
                client_id: self.client_id,
                correlation_id,
                stream_id,
                channel: channel.as_bytes(),
            }
        };
        let name = if exclusive {
            "add exclusive publication"
        } else {
            "add publication"
        };
        match self.request(&command, correlation_id, name)? {
            Response::PublicationReady {
                registration_id,
                session_id,
                stream_id: ready_stream,
                limit_counter_id,
                log_file_name,
                exclusive: ready_exclusive,
                ..
            } => {
                let expected = format!("publications/{registration_id}.logbuffer");
                if log_file_name != expected
                    || ready_stream != stream_id
                    || ready_exclusive != exclusive
                {
                    return Err(ClientError::Driver {
                        what: format!(
                            "publication ready for registration {registration_id}, stream \
                             {ready_stream}, exclusive {ready_exclusive}, log {log_file_name}"
                        ),
                    });
                }
                let log = LogBuffers::open(&self.dir, registration_id, true)?;
                let limit = self
                    .counters
                    .value(
                        u32::try_from(limit_counter_id).map_err(|_| ClientError::Driver {
                            what: format!("limit counter id {limit_counter_id}"),
                        })?,
                    )?;
                Publication::new(
                    log,
                    limit,
                    Arc::clone(&self.shared),
                    self.ring.producer(DRIVER_TIMEOUT)?,
                    self.client_id,
                    registration_id,
                    session_id,
                    stream_id,
                    channel.to_string(),
                    exclusive,
                )
            }
            other => Err(ClientError::Driver {
                what: format!("{other:?} in answer to {name}"),
            }),
        }
    }

    /// Asks the driver to record the live publication on `channel` and `stream_id` to
    /// the durable log, from its producer position: of the stream's live publications,
    /// the one with the lowest registration id not yet being recorded, so that a second
    /// call records the next one (`spec/client-protocol.md`, "Commands"; ADR 0025).
    /// Waits for `recording ready` up to the response timeout and returns the
    /// [`Recording`], whose two positions read from the counters file. The recording
    /// belongs to the driver: it outlives this client, dropping the handle stops nothing,
    /// and [`Cavalry::stop_recording`] from any client stops it. No live publication is
    /// [`ClientError::Refused`] with error code 20, every one already recorded code 22, a
    /// channel other than `cavalry:ipc` code 1, and a catalog, registry or counters file
    /// at capacity code 10.
    pub fn start_recording(
        &mut self,
        channel: &str,
        stream_id: i32,
    ) -> Result<Recording, ClientError> {
        let correlation_id = self.producer.next_correlation_id();
        let command = Command::StartRecording {
            client_id: self.client_id,
            correlation_id,
            stream_id,
            channel: channel.as_bytes(),
        };
        match self.request(&command, correlation_id, "start recording")? {
            Response::RecordingReady {
                recording_id,
                session_id,
                position_counter_id,
                synced_position_counter_id,
                ..
            } => {
                let counter = |id: i32| -> Result<CounterValue, ClientError> {
                    Ok(self.counters.value(u32::try_from(id).map_err(|_| {
                        ClientError::Driver {
                            what: format!("recording counter id {id}"),
                        }
                    })?)?)
                };
                Ok(Recording::new(
                    recording_id,
                    session_id,
                    stream_id,
                    channel.to_string(),
                    counter(position_counter_id)?,
                    counter(synced_position_counter_id)?,
                ))
            }
            other => Err(ClientError::Driver {
                what: format!("{other:?} in answer to start recording"),
            }),
        }
    }

    /// Stops recording `recording_id`, whichever client started it: the driver writes the
    /// catalog's stop at the position written and answers once it is synced
    /// (`spec/client-protocol.md`, "Commands"). A recording already stopped is success
    /// with nothing written; an id the catalog does not hold is [`ClientError::Refused`]
    /// with error code 21.
    pub fn stop_recording(&mut self, recording_id: i64) -> Result<(), ClientError> {
        let correlation_id = self.producer.next_correlation_id();
        let command = Command::StopRecording {
            client_id: self.client_id,
            correlation_id,
            recording_id,
        };
        match self.request(&command, correlation_id, "stop recording")? {
            Response::Success { .. } => Ok(()),
            other => Err(ClientError::Driver {
                what: format!("{other:?} in answer to stop recording"),
            }),
        }
    }

    /// Asks the driver to play recording `recording_id` from `position` for `length`
    /// bytes into a publication on `replay_stream_id` and `channel`, which a
    /// subscription on them reads as any stream, its image's positions the recording's
    /// (`spec/client-protocol.md`, "Commands"; ADR 0030). `position` is a multiple of 32
    /// from the recording's start position to its extent, or [`NULL_POSITION`] for the
    /// start position; `length` is a byte count, [`NULL_LENGTH`] for everything up to
    /// the extent when the replay starts, or [`FOLLOW_LENGTH`] to follow a live
    /// recording until it stops. Add the subscription first: a replay with no subscriber
    /// does not move and ends after the replay connect timeout. Waits for `replay ready`
    /// up to the response timeout and returns the [`Replay`], whose
    /// [`position`](Replay::position) reads the replayer's counter and whose
    /// [`ended`](Replay::ended) says how it ended once it has. The replay belongs to the
    /// driver: dropping the handle stops nothing, and [`Cavalry::stop_replay`] from any
    /// client stops it. A recording the catalog does not hold is
    /// [`ClientError::Refused`] with error code 21, a position out of range code 23, a
    /// length below −1 code 7, a channel other than `cavalry:ipc` code 1, a segment file
    /// that cannot be opened or a log that cannot be created code 12, and a registry or
    /// the counters file at capacity code 10.
    ///
    /// [`NULL_POSITION`]: cavalry_proto::client_protocol::NULL_POSITION
    /// [`NULL_LENGTH`]: cavalry_proto::client_protocol::NULL_LENGTH
    /// [`FOLLOW_LENGTH`]: cavalry_proto::client_protocol::FOLLOW_LENGTH
    pub fn start_replay(
        &mut self,
        recording_id: i64,
        position: i64,
        length: i64,
        replay_stream_id: i32,
        channel: &str,
    ) -> Result<Replay, ClientError> {
        let correlation_id = self.producer.next_correlation_id();
        // The registration goes first, before the command: the conductor admits the
        // queue before it handles an answer, so the `replay stopped` of a replay that
        // ends at once finds its registration (`Conductor::events`).
        let end = Arc::new(ReplayEnd::new());
        let registration = ReplayRegistration {
            replay_session_id: correlation_id,
            end: Arc::clone(&end),
        };
        if self.replay_registrations.offer(registration).is_err() {
            return Err(ClientError::Driver {
                what: "the replay registration queue is full: start replays one at a time"
                    .to_string(),
            });
        }
        let command = Command::StartReplay {
            client_id: self.client_id,
            correlation_id,
            recording_id,
            position,
            length,
            replay_stream_id,
            replay_channel: channel.as_bytes(),
        };
        match self.request(&command, correlation_id, "start replay")? {
            Response::ReplayReady {
                replay_session_id,
                recording_id: ready_recording,
                start_position,
                session_id,
                stream_id,
                ..
            } => {
                if replay_session_id != correlation_id
                    || ready_recording != recording_id
                    || stream_id != replay_stream_id
                {
                    return Err(ClientError::Driver {
                        what: format!(
                            "replay ready for replay {replay_session_id}, recording \
                             {ready_recording}, stream {stream_id}"
                        ),
                    });
                }
                let position = self.replay_position_counter(replay_session_id)?;
                Ok(Replay::new(
                    replay_session_id,
                    recording_id,
                    start_position,
                    session_id,
                    stream_id,
                    channel.to_string(),
                    position,
                    end,
                ))
            }
            other => Err(ClientError::Driver {
                what: format!("{other:?} in answer to start replay"),
            }),
        }
    }

    /// The `replay.position` counter of replay `replay_session_id`, found by its type
    /// and key in the counters file, since `replay ready` carries no counter id
    /// (`spec/client-protocol.md`, "Counters"); `None` when the replay has already ended
    /// and the driver freed the counter, which a short replay does before its client
    /// looks, and then `replay stopped` is on its way with the end position.
    fn replay_position_counter(&self, replay_session_id: i64) -> Result<Option<CounterValue>, ClientError> {
        let key = replay_session_id.to_le_bytes();
        for id in 0..self.counters.capacity() {
            if let Some(record) = self.counters.record(id)?
                && record.type_id() == REPLAY_POSITION.0
                && record.key().starts_with(&key)
            {
                return Ok(self.counters.value(id).ok());
            }
        }
        Ok(None)
    }

    /// Stops replay `replay_session_id`, whichever client started it, at the position
    /// reached; the driver answers once the replayer has written its last frame, then
    /// sends `replay stopped` with reason 1 (`spec/client-protocol.md`, "Commands"). A
    /// replay already ended is success with nothing done.
    pub fn stop_replay(&mut self, replay_session_id: i64) -> Result<(), ClientError> {
        let correlation_id = self.producer.next_correlation_id();
        let command = Command::StopReplay {
            client_id: self.client_id,
            correlation_id,
            replay_session_id,
        };
        match self.request(&command, correlation_id, "stop replay")? {
            Response::Success { .. } => Ok(()),
            other => Err(ClientError::Driver {
                what: format!("{other:?} in answer to stop replay"),
            }),
        }
    }

    /// Sends `command` and waits for its answer: the event with its correlation id, up
    /// to the response timeout. An `error` event is [`ClientError::Refused`]; a client
    /// closed meanwhile is [`ClientError::Closed`]; no answer is
    /// [`ClientError::ResponseTimeout`], and the command may have been unblocked or
    /// rejected before it was decoded (`spec/client-protocol.md`, "Liveness and
    /// timeouts"). Answers to earlier commands the caller gave up on are dropped.
    fn request(
        &mut self,
        command: &Command<'_>,
        correlation_id: i64,
        name: &'static str,
    ) -> Result<Response, ClientError> {
        if let Some(reason) = self.shared.closed() {
            return Err(ClientError::Closed(reason));
        }
        while self.responses.poll().is_some() {}
        self.shared.set_pending(correlation_id);
        let sent = self.send(command);
        if let Err(error) = sent {
            self.shared.set_pending(NO_PENDING);
            return Err(error);
        }
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        let mut spins: u32 = 0;
        let outcome = loop {
            match self.responses.poll() {
                Some(response) if response.correlation_id() == correlation_id => {
                    break Ok(response);
                }
                Some(_) => continue,
                None => {}
            }
            if let Some(reason) = self.shared.closed() {
                break Err(ClientError::Closed(reason));
            }
            if Instant::now() >= deadline {
                break Err(ClientError::ResponseTimeout { command: name });
            }
            if spins < 100 {
                spins += 1;
                std::hint::spin_loop();
            } else {
                std::thread::sleep(Duration::from_micros(50));
            }
        };
        self.shared.set_pending(NO_PENDING);
        match outcome? {
            Response::Error { code, message, .. } => Err(ClientError::Refused {
                code: ErrorCode::from_code(code),
                raw_code: code,
                message,
            }),
            response => Ok(response),
        }
    }

    /// Encodes and writes `command` on the ring, and notes the time for the keepalive.
    fn send(&mut self, command: &Command<'_>) -> Result<(), ClientError> {
        let length = command.write(&mut self.scratch)?;
        let now = now_nanos();
        self.producer
            .write(command.command_type() as i32, &self.scratch[..length], now)?;
        self.shared.sent(now);
        Ok(())
    }

    /// The driver directory this client is attached to.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The client id the driver knows this client by.
    pub fn client_id(&self) -> i64 {
        self.client_id
    }

    /// Why the client is closed, or `None` while it is open. A client closes when
    /// [`Cavalry::close`] is called, and when the conductor finds the driver gone, this
    /// client timed out, or the broadcast buffer lapped.
    pub fn closed(&self) -> Option<Closed> {
        self.shared.closed()
    }

    /// Whether the client is closed.
    pub fn is_closed(&self) -> bool {
        self.closed().is_some()
    }

    /// The driver's counters file, read-only: what `doctor` reads, from the client.
    pub fn counters(&self) -> &CountersReader {
        &self.counters
    }

    /// Sends `client close`, so the driver removes this client's publications and
    /// subscriptions now rather than at the liveness timeout, and stops the conductor.
    /// A driver that is gone cannot be told, and that is not an error here: the client
    /// is closed either way, and [`Cavalry::closed`] before the call said why.
    pub fn close(mut self) -> Result<(), ClientError> {
        self.shutdown();
        Ok(())
    }

    fn shutdown(&mut self) {
        if self.shared.close(Closed::ByUser) {
            let command = Command::ClientClose {
                client_id: self.client_id,
                correlation_id: self.producer.next_correlation_id(),
            };
            if let Ok(length) = command.write(&mut self.scratch) {
                let _ = self.producer.write(
                    command.command_type() as i32,
                    &self.scratch[..length],
                    now_nanos(),
                );
            }
        }
        if let Some(handle) = self.conductor.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Cavalry {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl std::fmt::Debug for Cavalry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cavalry")
            .field("dir", &self.dir)
            .field("client_id", &self.client_id)
            .field("closed", &self.closed())
            .finish()
    }
}
