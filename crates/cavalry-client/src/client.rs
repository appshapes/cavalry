//! [`Cavalry`], a process's attachment to the driver (`spec/client-protocol.md`, "The
//! driver directory"; ADR 0021).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use std::time::{Duration, Instant};

use cavalry_proto::client_protocol::{Command, ErrorCode};
use cavalry_shm::broadcast::{BroadcastBuffer, BroadcastError};
use cavalry_shm::client_lock::ClientsDir;
use cavalry_shm::command_ring::layout::DRIVER_CLOSED;
use cavalry_shm::command_ring::{CommandRing, CommandRingError, Producer};
use cavalry_shm::counters::generated::REPLAY_POSITION;
use cavalry_shm::counters::{CounterValue, CountersError, CountersReader, CountersWriter};
use cavalry_shm::log::LogBuffers;
use cavalry_shm::spsc;

use crate::cluster::{ClusterError, ClusterSession};
use crate::committed_log::CommittedLog;
use crate::conductor::{
    COMMAND_SCRATCH, Conductor, DRIVER_TIMEOUT, MAX_REPLAYS, MAX_SUBSCRIPTIONS, NO_PENDING,
    RECORDING_QUEUE, RECORDING_REGISTRY, REGISTRATION_QUEUE, REPLAY_QUEUE, RESPONSE_QUEUE,
    RESPONSE_TIMEOUT, RecordingRegistration, ReplayRegistration, Shared, epoch_time, nano_time,
    nanos,
};
use crate::counter::Counter;
use crate::error::{ClientError, Closed};
use crate::publication::Publication;
use crate::recording::{ArmedRecording, Recording, RecordingAnswer, RecordingStop};
use crate::replay::{Replay, ReplayEnd};
use crate::response::Response;
use crate::service::ClusterService;
use crate::subscription::{
    IMAGE_QUEUE, ImageEvent, RETIRE_QUEUE, Registration, Retired, Subscription,
};

/// The environment variable that names the driver directory (`config.toml`, `dir`;
/// ADR 0020). Clients read the same key the driver does.
pub const DIR_ENV: &str = "CAVALRY_DIR";

/// How long a connecting client sleeps between two reads of a directory whose driver is
/// still creating its files: the interval at which a starting driver reads a
/// `not-ready` command ring again (`spec/client-protocol.md`, "The driver directory";
/// ADR 0072). Every read maps the files it finds whole anew, so the interval also
/// bounds the work of a client that waits the whole driver timeout: a thousand reads.
const ATTACH_POLL: Duration = Duration::from_millis(10);

/// What a test does at a step of this thread's attach.
#[cfg(test)]
type AttachStep = Box<dyn FnMut(u8)>;

#[cfg(test)]
thread_local! {
    /// What a test does in the middle of this thread's attach, called with the step
    /// the attach has reached: 1, the handle on `clients/` is open and nothing else;
    /// 2, the command ring is mapped and its heartbeat was found fresh; 3, the
    /// client's id is taken and its lock file is not made yet. A test starts another
    /// driver in the directory there, which is what a client that was stopped at that
    /// step finds when it is continued.
    pub(crate) static ATTACH_STEP: std::cell::RefCell<Option<AttachStep>> =
        const { std::cell::RefCell::new(None) };
}

/// The test's step, when a test set one; nothing outside a test.
#[cfg(test)]
fn attach_step(step: u8) {
    ATTACH_STEP.with(|test| {
        if let Some(test) = test.borrow_mut().as_mut() {
            test(step);
        }
    });
}

/// The test's step, when a test set one; nothing outside a test.
#[cfg(not(test))]
#[inline(always)]
fn attach_step(_step: u8) {}

/// What a client has once its first command is written: its id, its lock, the
/// producer that wrote the command, and the conductor's producer.
type FirstCommand = (i64, crate::owned::ClientLock, Producer, Producer);

/// How many client ids in a row a connecting client takes while it finds a lock file
/// of each one's name already there, before it gives up with the error of the last.
/// No client of the driver makes such a file: ids are distinct within a ring. It is
/// what another hand left in `clients/`.
const LOCK_NAMES_TRIED: usize = 8;

/// The driver directory's three files, each open and past its page's checks, beside a
/// driver whose heartbeat is fresh: what one read of the directory gives a connecting
/// client.
struct Attached {
    ring: CommandRing,
    broadcast: BroadcastBuffer,
    counters: CountersReader,
    /// The mapping this client stores into records through. Every later one is a
    /// share of it, never a second open by the directory's name.
    writer: CountersWriter,
    /// The handle on `clients/`, opened before the three files: the client's lock
    /// file is made through it (`cavalry_shm::client_lock`).
    clients: ClientsDir,
}

/// Why one read of the driver directory attached nothing.
enum Unattached {
    /// A file is as its creator leaves it on the way: the driver is creating its files
    /// now, or stopped while it was. Read again, up to the driver timeout.
    Creating(Unfinished),
    /// Anything else, which no wait changes: no driver, a driver that is gone, a file
    /// that fails a check of its page.
    Refused(ClientError),
}

/// Which file a read found unfinished.
enum Unfinished {
    /// The command ring: at the bound a dead driver's, as it is to a starting driver.
    CommandRing,
    /// The broadcast buffer or the counters file, beside a finished command ring.
    Other {
        /// The file's own refusal: what the client answers with at the bound when the
        /// driver has stored its heartbeat meanwhile.
        error: ClientError,
        /// The ring's heartbeat on this read. A driver that runs stores it again every
        /// heartbeat interval; a creator that stopped after its ring never does.
        heartbeat: i64,
    },
}

/// Whether a refusal of the broadcast buffer is of a file its creator has not
/// finished: `not-ready`, empty, or not there yet beside the command ring.
fn broadcast_unfinished(error: &BroadcastError) -> bool {
    match error {
        BroadcastError::NotReady | BroadcastError::TooShort { given: 0 } => true,
        BroadcastError::Io(source) => source.kind() == ErrorKind::NotFound,
        _ => false,
    }
}

/// As [`broadcast_unfinished`], of the counters file.
fn counters_unfinished(error: &CountersError) -> bool {
    match error {
        CountersError::NotReady | CountersError::TooShort { given: 0 } => true,
        CountersError::Io(source) => source.kind() == ErrorKind::NotFound,
        _ => false,
    }
}

impl Attached {
    /// One read of the directory, in the order of `spec/client-protocol.md`, "The
    /// driver directory": the command ring writable and its heartbeat, the broadcast
    /// buffer read-only, the counters file read-only. A file that is missing, empty
    /// or `not-ready` once the command ring is there is [`Unattached::Creating`]; a
    /// directory with no command ring is no driver.
    fn open(dir: &Path) -> Result<Self, Unattached> {
        if !dir.join("command-ring").is_file() {
            return Err(Unattached::Refused(ClientError::NotFound {
                dir: dir.to_path_buf(),
            }));
        }
        // The handle on `clients/` first, before any of the driver's files is opened:
        // the client's lock file is made through it, after the last of them, and a
        // file is made in no directory that has been removed. A driver that starts
        // removes `clients/` with everything else the last driver left, and only then
        // creates its own. So a lock file that is made says that every file opened
        // between here and there is of the one driver whose `clients/` this is
        // (`spec/client-protocol.md`, "The driver directory").
        let clients = match ClientsDir::open(dir) {
            Ok(clients) => Some(clients),
            // None there: a driver that is starting, or one of a version before the
            // lock, whose counters file is refused below with the message that says so.
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            // Anything else is no driver starting, and waiting changes nothing.
            Err(source) => {
                return Err(Unattached::Refused(ClientError::Io {
                    what: "open the directory of the clients' locks, in which this client \
                           makes its lock file; it must be a directory that this user can \
                           read and write: remove whatever else has its name, and start the \
                           driver again, which makes it",
                    path: dir.join(cavalry_proto::client_protocol::CLIENT_LOCK_DIRECTORY),
                    source,
                }));
            }
        };
        attach_step(1);
        let ring = match CommandRing::open(dir, true) {
            Ok(ring) => ring,
            Err(CommandRingError::NotReady | CommandRingError::TooShort { given: 0 }) => {
                return Err(Unattached::Creating(Unfinished::CommandRing));
            }
            // Gone between the look above and the open: a starting driver removes the
            // directory a dead one left. No command ring is no driver.
            Err(CommandRingError::Io(source)) if source.kind() == ErrorKind::NotFound => {
                return Err(Unattached::Refused(ClientError::NotFound {
                    dir: dir.to_path_buf(),
                }));
            }
            Err(error) => return Err(Unattached::Refused(error.into())),
        };
        let heartbeat = ring.consumer_heartbeat();
        if heartbeat == DRIVER_CLOSED {
            return Err(Unattached::Refused(ClientError::DriverGone {
                dir: dir.to_path_buf(),
                reason: Closed::DriverClosed,
            }));
        }
        if epoch_time().saturating_sub(heartbeat) > nanos(DRIVER_TIMEOUT) {
            return Err(Unattached::Refused(ClientError::DriverGone {
                dir: dir.to_path_buf(),
                reason: Closed::DriverTimeout,
            }));
        }
        attach_step(2);
        let broadcast = BroadcastBuffer::open(dir, false).map_err(|error| {
            Unattached::other(broadcast_unfinished(&error), error.into(), heartbeat)
        })?;
        let counters = CountersReader::open(dir).map_err(|error| {
            Unattached::other(counters_unfinished(&error), error.into(), heartbeat)
        })?;
        let writer = CountersWriter::open(dir).map_err(|error| {
            Unattached::other(counters_unfinished(&error), error.into(), heartbeat)
        })?;
        // A command ring with no `clients/` beside it is a driver that is starting, or
        // a directory that a driver which starts is emptying: read again. A driver of
        // a version before the lock has none either, and its counters file was
        // refused above, with the message that says so.
        let Some(clients) = clients else {
            return Err(Unattached::Creating(Unfinished::CommandRing));
        };
        Ok(Self {
            ring,
            broadcast,
            counters,
            writer,
            clients,
        })
    }
}

impl Unattached {
    /// A refusal of the broadcast buffer or the counters file, beside a command ring
    /// that held `heartbeat`: to be read again when the file is `unfinished`, refused
    /// for good otherwise.
    fn other(unfinished: bool, error: ClientError, heartbeat: i64) -> Self {
        if unfinished {
            Unattached::Creating(Unfinished::Other { error, heartbeat })
        } else {
            Unattached::Refused(error)
        }
    }
}

/// A client: one process's attachment to one driver. [`Cavalry::connect`] opens the
/// driver directory's files, takes a client id and starts the client conductor thread,
/// which keeps the client alive with the driver and reads every event. Publications and
/// subscriptions are added from here. [`Cavalry::close`] tells the driver and stops the
/// conductor; dropping a client does the same. On macOS a client starts a second
/// thread, its watch, which ends a park of the conductor that outlived its timeout, as
/// the conductor ends the watch's; a park so ended is counted in a counter the client
/// adds for itself, `client.conductor_parks_overdue` (ADR 0177).
pub struct Cavalry {
    dir: PathBuf,
    client_id: i64,
    shared: Arc<Shared>,
    producer: Producer,
    ring: CommandRing,
    /// Shared with the recording handles, which check a record before they trust its
    /// value ([`Recording`]).
    counters: Arc<CountersReader>,
    /// The counters file mapped writable, for the counters this client adds and owns
    /// (ADR 0062): a second handle on the mapping the client opened when it attached,
    /// and never a second open by the directory's name (ADR 0179).
    counters_writer: CountersWriter,
    responses: spsc::Consumer<Response>,
    registrations: spsc::Producer<Registration>,
    replay_registrations: spsc::Producer<ReplayRegistration>,
    recording_registrations: spsc::Producer<RecordingRegistration>,
    conductor: Option<JoinHandle<()>>,
    /// The client's watch, where it has one (ADR 0177).
    watch: Option<JoinHandle<()>>,
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
    ///
    /// A driver that is still creating its files is waited for: while the command ring
    /// is there and one of the three files is missing, empty or `not-ready`, the call
    /// reads the directory again every 10 ms, for up to the driver timeout, 10 s. A
    /// driver takes a few milliseconds to create its files, so a client started right
    /// after its driver connects and needs no retry of its own. At the bound the
    /// driver is a dead one, [`ClientError::DriverGone`] with `driver-timeout`, when
    /// its command ring is still unfinished, or is finished and its heartbeat was
    /// never stored again while the client waited; another file still unfinished
    /// beside a heartbeat the driver has stored meanwhile is refused with its own
    /// check (`spec/client-protocol.md`, "The driver directory"; ADR 0072). The bound
    /// is measured on a clock that never goes back, so a wall clock that is set
    /// during the wait neither lengthens it nor changes its answer.
    pub fn connect_to(dir: impl AsRef<Path>) -> Result<Self, ClientError> {
        Self::connect_in(dir.as_ref())
    }

    /// The client's id, its lock and its first command, in that order, on what
    /// `attached` holds: the id from the ring's correlation id counter, the lock file
    /// made through the handle on `clients/` and locked, and then the keepalive by
    /// which the driver learns of the client. A client that has no lock sends no
    /// command, so the driver never reads a command of a client it cannot ask about
    /// (`spec/client-protocol.md`, "The records a client writes"; ADR 0179).
    ///
    /// `Ok(None)` when the directory of the locks has been removed: a driver has
    /// started in this directory since `attached` was opened, and the caller reads
    /// the directory again. A name that is already taken, which no client of this
    /// driver can have made, costs the id: the next one is taken, up to
    /// [`LOCK_NAMES_TRIED`] in all.
    fn first_command(
        dir: &Path,
        attached: &Attached,
        scratch: &mut [u8],
        taken: &mut usize,
    ) -> Result<Option<FirstCommand>, ClientError> {
        let mut producer = attached.ring.producer(DRIVER_TIMEOUT)?;
        let conductor_producer = attached.ring.producer(DRIVER_TIMEOUT)?;
        let (client_id, lock) = loop {
            let client_id = producer.next_correlation_id();
            attach_step(3);
            match crate::owned::ClientLock::take(&attached.clients, client_id) {
                Ok(lock) => break (client_id, lock),
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error)
                    if error.kind() == ErrorKind::AlreadyExists
                        && *taken + 1 < LOCK_NAMES_TRIED =>
                {
                    *taken += 1;
                }
                Err(source) => {
                    return Err(ClientError::Io {
                        what: "make and lock this client's lock file; the driver's directory \
                               must be one this user can write, on a filesystem that keeps \
                               advisory locks, as the driver's own lock on it needs",
                        path: cavalry_shm::client_lock::path(
                            dir,
                            client_id,
                            cavalry_proto::client_protocol::CLIENT_LOCK_SUFFIX,
                        ),
                        source,
                    });
                }
            }
        };
        let keepalive = Command::ClientKeepalive {
            client_id,
            correlation_id: producer.next_correlation_id(),
        };
        // From the command on, the driver knows the client and removes the file when
        // the client has gone; a command that could not be encoded or written leaves
        // a file the driver never learns of, which is removed here, through the handle
        // it was made through.
        let written = keepalive
            .write(scratch)
            .map_err(ClientError::from)
            .and_then(|length| {
                producer
                    .write(
                        keepalive.command_type() as i32,
                        &scratch[..length],
                        epoch_time(),
                    )
                    .map_err(ClientError::from)
            });
        if let Err(error) = written {
            lock.abandon(&attached.clients, client_id);
            return Err(error);
        }
        Ok(Some((client_id, lock, producer, conductor_producer)))
    }

    fn connect_in(dir: &Path) -> Result<Self, ClientError> {
        let dir = dir.to_path_buf();
        let deadline = Instant::now() + DRIVER_TIMEOUT;
        // The heartbeat on the first read that found the command ring finished and
        // another file not.
        let mut first_heartbeat = None;
        let mut scratch = vec![0u8; COMMAND_SCRATCH];
        // How many ids in a row had a lock file of their name already.
        let mut taken = 0;
        let (broadcast, counters, counters_writer, ring, producer, conductor_producer) = loop {
            let unfinished = match Attached::open(&dir) {
                Ok(attached) => {
                    match Self::first_command(&dir, &attached, &mut scratch, &mut taken) {
                        Ok(Some((client_id, lock, producer, conductor_producer))) => {
                            let Attached {
                                ring,
                                broadcast,
                                counters,
                                writer,
                                clients: _,
                            } = attached;
                            break (
                                broadcast,
                                counters,
                                writer,
                                ring,
                                (client_id, lock, producer),
                                conductor_producer,
                            );
                        }
                        // The directory of the locks was removed under the client: a
                        // driver is starting. Everything is opened again, and the id
                        // is taken again, from the ring that is there then.
                        Ok(None) => Unfinished::CommandRing,
                        Err(error) => return Err(error),
                    }
                }
                Err(Unattached::Refused(error)) => return Err(error),
                Err(Unattached::Creating(unfinished)) => unfinished,
            };
            if let Unfinished::Other { heartbeat, .. } = &unfinished {
                first_heartbeat.get_or_insert(*heartbeat);
            }
            if Instant::now() < deadline {
                std::thread::sleep(ATTACH_POLL);
                continue;
            }
            return Err(match unfinished {
                // The driver stored its heartbeat while the client waited: it runs, and
                // a running driver finished its files before it ran. The directory is
                // damaged, and the file's own check says how.
                Unfinished::Other { error, heartbeat } if first_heartbeat != Some(heartbeat) => {
                    error
                }
                // A ring never finished, or a heartbeat never stored again: the creator
                // stopped on the way.
                Unfinished::CommandRing | Unfinished::Other { .. } => ClientError::DriverGone {
                    dir,
                    reason: Closed::DriverTimeout,
                },
            });
        };
        let (client_id, lock, mut producer) = producer;
        // The conductor's handle on the ring: the same mapping, not a second open by
        // the directory's name.
        let conductor_ring = ring.share();
        let receiver = broadcast.receiver()?;
        let counters = Arc::new(counters);
        // The conductor's mapping to store release words through; the client's own is
        // a share of it, the same file by construction.
        let own_writer = counters_writer.share();

        let shared = Arc::new(Shared::new(nano_time(), Some(lock)));
        let (response_producer, responses) = spsc::queue::<Response>(RESPONSE_QUEUE);
        let (registrations, registration_consumer) =
            spsc::queue::<Registration>(REGISTRATION_QUEUE);
        let (replay_registrations, replay_registration_consumer) =
            spsc::queue::<ReplayRegistration>(REPLAY_QUEUE);
        let (recording_registrations, recording_registration_consumer) =
            spsc::queue::<RecordingRegistration>(RECORDING_QUEUE);
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
            recording_registration_consumer,
            client_id,
        );
        // The client's watch, where it has one: a second thread that ends a park of
        // the conductor that outlived its timeout, as the conductor ends the watch's
        // (ADR 0177). On macOS, where a timed wait has been seen to outlive its timer
        // (ADR 0171), and nowhere else; a test chooses for itself.
        let watching = cfg!(target_os = "macos");
        #[cfg(test)]
        let watching = crate::conductor::WATCH_FOR_TEST
            .get()
            .map_or(watching, |watch| watch.is_some());
        let pair = watching.then(|| Arc::new(crate::watch::Pair::new()));
        let conductor = conductor.watched(pair.clone());
        #[cfg(test)]
        let park_for_test = (
            crate::conductor::PARK_FOR_TEST.get(),
            crate::conductor::PARK_FOR_TEST_ONCE.get(),
        );
        let handle = std::thread::Builder::new()
            .name("cavalry-client-conductor".to_string())
            .spawn(move || {
                #[cfg(test)]
                {
                    crate::conductor::PARK_FOR_TEST.set(park_for_test.0);
                    crate::conductor::PARK_FOR_TEST_ONCE.set(park_for_test.1);
                }
                conductor.run()
            })
            .map_err(|source| ClientError::Io {
                what: "start the client conductor thread",
                path: dir.clone(),
                source,
            })?;
        // The watch after the conductor, which it looks at. A watch that cannot be
        // started closes the client and ends the conductor: a client is not left
        // without the thread its system needs.
        let watch = match pair {
            None => None,
            Some(pair) => {
                #[cfg(test)]
                let first_park = crate::conductor::WATCH_FOR_TEST.get().flatten().flatten();
                #[cfg(not(test))]
                let first_park = None;
                let watched = Arc::clone(&shared);
                let for_watch = Arc::clone(&pair);
                match std::thread::Builder::new()
                    .name("cavalry-client-watch".to_string())
                    .spawn(move || {
                        let _ = for_watch.watch_thread.set(std::thread::current());
                        for_watch.watch(nano_time, || watched.closed().is_some(), first_park);
                    }) {
                    Ok(watch) => Some(watch),
                    Err(source) => {
                        // As a close: the driver is told, so that it does not hold the
                        // client until its timeout, and the conductor is ended.
                        if shared.close(Closed::ByUser) {
                            let command = Command::ClientClose {
                                client_id,
                                correlation_id: producer.next_correlation_id(),
                            };
                            if let Ok(length) = command.write(&mut scratch) {
                                let _ = producer.write(
                                    command.command_type() as i32,
                                    &scratch[..length],
                                    epoch_time(),
                                );
                            }
                        }
                        handle.thread().unpark();
                        let _ = handle.join();
                        return Err(ClientError::Io {
                            what: "start the client watch thread",
                            path: dir.clone(),
                            source,
                        });
                    }
                }
            }
        };
        Ok(Self {
            dir,
            client_id,
            shared,
            producer,
            ring,
            counters,
            counters_writer: own_writer,
            responses,
            registrations,
            replay_registrations,
            recording_registrations,
            conductor: Some(handle),
            watch,
            scratch,
        })
    }

    /// Adds a subscription on `channel` and `stream_id`: one image per publication on
    /// them, joining at the publisher's position, now and as publications come and go
    /// (`spec/client-protocol.md`, "Commands"). Waits for the driver's answer up to the
    /// response timeout. The channel is `cavalry:ipc`, or a `cavalry:udp` channel, on
    /// which the driver binds the endpoint and an image arrives with the sender's setup
    /// frame, its source identity the sender's address (`spec/udp.md`, "The receiver";
    /// ADR 0042). An add past the driver's bound on subscriptions, 1,024 of every client
    /// together, is [`ClientError::Refused`] with error code 10. A subscription dropped
    /// just before leaves its place to the add, whether or not the driver has answered
    /// its removal yet. [`ClientError::SubscriptionsFull`] is a driver that answered
    /// past its own bound: the client cannot hand the new subscription its images, and
    /// removes it again.
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
        let removal = Arc::new(std::sync::atomic::AtomicI64::new(
            crate::subscription::NOT_REMOVED,
        ));
        let mut registration = Registration {
            registration_id: correlation_id,
            images,
            retired: retired_consumer,
            removal: Arc::clone(&removal),
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
                    removal,
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
    /// answer up to the response timeout. The channel is `cavalry:ipc`, or a
    /// `cavalry:udp` channel the driver's sender puts on the wire (`spec/udp.md`, "The
    /// sender"; ADR 0041); any other string is [`ClientError::Refused`] with error code
    /// 1.
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
    /// [`Recording`], whose two positions read from the counters file while it is live
    /// and are its stop position once the driver has said it stopped, which this client
    /// follows for as long as the handle lives. The recording
    /// belongs to the driver: it outlives this client, dropping the handle stops nothing,
    /// and [`Cavalry::stop_recording`] from any client stops it. No live publication is
    /// [`ClientError::Refused`] with error code 20, every one already recorded code 22, a
    /// channel that is neither `cavalry:ipc` nor a `cavalry:udp` channel code 1, and a
    /// catalog, registry or counters file at capacity code 10. On a `cavalry:udp`
    /// channel the logs that match are the publications sending on it and the images
    /// received on it (ADR 0042).
    ///
    /// A `cavalry:udp` stream with a live subscription and no publication or image is
    /// not refused: the driver arms the recording, to start with the stream's next
    /// image (ADR 0059). This call returns a started recording or nothing, so it
    /// returns [`ClientError::RecordingArmed`] then, and the driver holds the armed
    /// recording all the same; [`Cavalry::arm_recording`] is the call for that order.
    pub fn start_recording(
        &mut self,
        channel: &str,
        stream_id: i32,
    ) -> Result<Recording, ClientError> {
        let stop = Arc::new(RecordingStop::new());
        let ready = match self.record(channel, stream_id, &stop, None)? {
            Response::Success { .. } => return Err(ClientError::RecordingArmed { stream_id }),
            ready => RecordingAnswer::new(ready)?,
        };
        // A record that is not the recording's by now is a recording that has already
        // stopped, whose counters the driver freed: the handle is made once the
        // conductor has stored the stop, which follows the answer.
        loop {
            if let Some(recording) = ready.recording(channel, stream_id, &self.counters, &stop)? {
                return Ok(recording);
            }
            if let Some(reason) = self.shared.closed() {
                return Err(ClientError::Closed(reason));
            }
            std::thread::sleep(Duration::from_micros(50));
        }
    }

    /// Asks the driver to record `channel` and `stream_id` as
    /// [`Cavalry::start_recording`] does, and holds the recording when the driver arms
    /// it instead of starting it: on a `cavalry:udp` stream with a live subscription
    /// and no publication or image yet, the driver answers at once and starts the
    /// recording when the stream's next image arrives, at the image's join position, so
    /// that every frame of the image is recorded (`spec/client-protocol.md`,
    /// "Commands", start recording; ADR 0059). A follower of a cluster arms the
    /// recording of its leader's log this way before the leader's first frame.
    ///
    /// Returns an [`ArmedRecording`], which [`ArmedRecording::poll`] turns into the
    /// [`Recording`] once the driver has started it; when the stream already had a
    /// publication or an image the recording started at once, and the first poll hands
    /// it over. The refusals are those of `start_recording`. The driver holds one
    /// armed recording for each stream and endpoint with a live subscription, so a
    /// second call for the stream is [`ClientError::Refused`] with code 22, and this
    /// client holds as many as the driver's subscriptions registry.
    pub fn arm_recording(
        &mut self,
        channel: &str,
        stream_id: i32,
    ) -> Result<ArmedRecording, ClientError> {
        let stop = Arc::new(RecordingStop::new());
        let (producer, answers) = spsc::queue::<Response>(1);
        let ready = match self.record(channel, stream_id, &stop, Some(producer))? {
            Response::Success { .. } => None,
            ready => Some(RecordingAnswer::new(ready)?),
        };
        Ok(ArmedRecording::new(
            stream_id,
            channel,
            Arc::clone(&self.counters),
            Arc::clone(&self.shared),
            stop,
            answers,
            ready,
        ))
    }

    /// Registers a recording with the conductor and sends its `start recording`: what
    /// `start_recording` and `arm_recording` share. Returns the first answer,
    /// `recording ready` or `operation success`; any other is [`ClientError::Driver`].
    fn record(
        &mut self,
        channel: &str,
        stream_id: i32,
        stop: &Arc<RecordingStop>,
        armed: Option<spsc::Producer<Response>>,
    ) -> Result<Response, ClientError> {
        let correlation_id = self.producer.next_correlation_id();
        // Registered before the command goes, so that the conductor holds the place for
        // this recording's stop when `recording ready` gives it the id, and for an
        // armed recording's second answer when `operation success` says it is armed
        // (`Conductor::events`): the driver frees a stopped recording's counters, and
        // the handle must not read them after.
        let mut registration = RecordingRegistration {
            correlation_id,
            stop: Arc::clone(stop),
            armed,
        };
        // One command is in flight per client, so the queue holds one at most; it is
        // full only while the conductor does not run, and then the command would wait
        // out the response timeout as well.
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        while let Err(back) = self.recording_registrations.offer(registration) {
            if let Some(reason) = self.shared.closed() {
                return Err(ClientError::Closed(reason));
            }
            if Instant::now() >= deadline {
                return Err(ClientError::ResponseTimeout {
                    command: "start recording",
                });
            }
            registration = back;
            std::thread::sleep(Duration::from_micros(50));
        }
        let command = Command::StartRecording {
            client_id: self.client_id,
            correlation_id,
            stream_id,
            channel: channel.as_bytes(),
        };
        let answer = self.request(&command, correlation_id, "start recording")?;
        if stop.refused() {
            // More registrations than the driver's own bounds allow have no stop
            // stored and no second answer read: a driver that does not conform. The
            // recording, started or armed, is the driver's and stays; this client
            // cannot follow it, so it hands out no handle that would read its counters
            // after its stop.
            return Err(ClientError::Driver {
                what: format!(
                    "{answer:?} in answer to start recording with {RECORDING_REGISTRY} \
                     recordings of this client still live or armed, more than the driver \
                     holds"
                ),
            });
        }
        match answer {
            answer @ (Response::RecordingReady { .. } | Response::Success { .. }) => Ok(answer),
            other => Err(ClientError::Driver {
                what: format!("{other:?} in answer to start recording"),
            }),
        }
    }

    /// Adds the destination `channel`, `cavalry:udp?endpoint=<host>:<port>`, to the
    /// control channel of the publication this client registered as `registration_id`,
    /// [`Publication::registration_id`](crate::Publication::registration_id): the
    /// driver resolves the endpoint once, to the first address of the control address's
    /// family, and the sender sends every datagram of the channel's publications to it
    /// from then on (`spec/client-protocol.md`, "Commands"; `spec/udp.md`, "The
    /// sender"; ADR 0053). The driver answers once it has queued the change for the
    /// sender, and the answer does not wait for the sender. The sender takes its
    /// requests in the order they were queued, at most 16 at the start of each duty
    /// cycle and none while its replies wait for room. So the destination gets what the
    /// endpoint sends from the start of the cycle that takes the change, which is the
    /// next cycle only when nothing waits ahead of it (ADR 0057). A destination already
    /// added, as written, is added again by nothing and is success. A destination is
    /// the channel's send endpoint's and not the publication's: the endpoint keeps it
    /// while any publication on the control channel is live or lingers after its
    /// close, so a program that closes a publication and adds the next on the same
    /// control channel before the linger ends finds its destinations still there, and
    /// may add them again without a check. Refused with
    /// [`ClientError::Refused`]: code 3 for a
    /// registration this client does not hold; code 1 for a publication whose channel
    /// names no control address, with the reason `control`; code 1 with the reason
    /// `destination` for a destination that is `cavalry:ipc` or a control channel, one
    /// that names `interface` or `fc`, or one with no address the control address can
    /// send to, an address of the other family or one beyond the loopback beside a
    /// loopback control address among them; code 9 for a host that resolves to no
    /// address at all; code 10 for a 65th destination on the endpoint, or when the
    /// sender's request queue has no room for the destination.
    pub fn add_destination(
        &mut self,
        registration_id: i64,
        channel: &str,
    ) -> Result<(), ClientError> {
        self.destination(registration_id, channel, true)
    }

    /// Removes the destination `channel`, as it was added, from the control channel of
    /// the publication `registration_id`: the driver matches the endpoint as the add
    /// wrote it and resolves nothing, so a name whose address changed since removes the
    /// address the add resolved (`spec/client-protocol.md`, "Commands"; ADR 0053). The
    /// driver answers once it has queued the change for the sender, as it does an add,
    /// so everything the endpoint sends still reaches the destination until the start of
    /// the cycle that takes the change, which is the next cycle only when nothing waits
    /// ahead of it (ADR 0057). One not there is removed by nothing and is success.
    /// Refused with
    /// [`ClientError::Refused`]: code 3 for a registration this client does not hold;
    /// code 1 for a publication whose channel names no control address, with the reason
    /// `control`; code 1 with the reason `destination` for a destination that is
    /// `cavalry:ipc` or a control channel, or one that names `interface` or `fc`; code
    /// 10 when the sender's request queue has no room for the removal. A remove
    /// resolves nothing and adds nothing, so it never meets code 9, the refusal of an
    /// address the control address cannot send to, or a full table.
    pub fn remove_destination(
        &mut self,
        registration_id: i64,
        channel: &str,
    ) -> Result<(), ClientError> {
        self.destination(registration_id, channel, false)
    }

    fn destination(
        &mut self,
        registration_id: i64,
        channel: &str,
        add: bool,
    ) -> Result<(), ClientError> {
        let correlation_id = self.producer.next_correlation_id();
        let (command, name) = if add {
            (
                Command::AddDestination {
                    client_id: self.client_id,
                    correlation_id,
                    registration_id,
                    channel: channel.as_bytes(),
                },
                "add destination",
            )
        } else {
            (
                Command::RemoveDestination {
                    client_id: self.client_id,
                    correlation_id,
                    registration_id,
                    channel: channel.as_bytes(),
                },
                "remove destination",
            )
        };
        match self.request(&command, correlation_id, name)? {
            Response::Success { .. } => Ok(()),
            other => Err(ClientError::Driver {
                what: format!("{other:?} in answer to {name}"),
            }),
        }
    }

    /// Adds a counter this client owns: the driver allocates a record of its counters
    /// file with `type_id`, `key` and `label`, and the [`Counter`] returned is the one
    /// writer of its value (`spec/client-protocol.md`, "Commands"; ADR 0062). Waits for
    /// `counter ready` up to the response timeout.
    ///
    /// `type_id` is the `id` of a counter to which `counters.toml` gives the scope
    /// `client` (`docs/reference/counters.md`), `key` is what that entry's `doc` says
    /// its key is, at most 112 bytes, and `label` is the entry's name, a space and the
    /// key's text, at most 124 bytes. `cavalry doctor` and any reader of the counters
    /// file see the counter under that label. The driver frees it when the [`Counter`]
    /// is closed or dropped and when this client closes or is timed out.
    ///
    /// A key or a label past its bound is [`ClientError::Protocol`], and nothing is
    /// sent. Refused with [`ClientError::Refused`]: code 11 for a type id that is not a
    /// client's; code 10 when the driver holds 256 client counters, or its counters
    /// file has no free record.
    ///
    /// ```
    /// use cavalry_client::Cavalry;
    /// use cavalry_shm::counters::generated::CLUSTER_COMMIT_POSITION;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let dir = std::env::temp_dir().join(format!("cavalry-doc-counter-{}", std::process::id()));
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// # let driver = cavalry_driver::Driver::start(&dir, dir.join("log"))?;
    /// let mut client = Cavalry::connect_to(&dir)?;
    /// // A cluster member's commit position, keyed by its member id, 2.
    /// let member_id: i32 = 2;
    /// let counter = client.add_counter(
    ///     CLUSTER_COMMIT_POSITION.0,
    ///     &member_id.to_le_bytes(),
    ///     "cluster.commit_position 2",
    /// )?;
    /// counter.store_release(4096)?;
    /// // Any reader of the counters file sees it, by the record `counter ready` named.
    /// assert_eq!(client.counters().read(counter.id())?, 4096);
    /// counter.close();
    /// # client.close()?;
    /// # driver.close()?;
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// # Ok(())
    /// # }
    /// ```
    pub fn add_counter(
        &mut self,
        type_id: i32,
        key: &[u8],
        label: &str,
    ) -> Result<Counter, ClientError> {
        // Everything that can fail on this side fails before the command is sent, so
        // that the driver never holds a counter this client has no handle on.
        let producer = self.ring.producer(DRIVER_TIMEOUT)?;
        let correlation_id = self.producer.next_correlation_id();
        let command = Command::AddCounter {
            client_id: self.client_id,
            correlation_id,
            type_id,
            key,
            label: label.as_bytes(),
        };
        match self.request(&command, correlation_id, "add counter")? {
            Response::CounterReady { counter_id, .. } => {
                let cell = u32::try_from(counter_id)
                    .ok()
                    .and_then(|id| self.counters_writer.counter(id).ok());
                let Some(cell) = cell else {
                    // The driver named a record this client cannot write: give the
                    // counter back, so the registry does not keep it until the close.
                    let remove = Command::RemoveCounter {
                        client_id: self.client_id,
                        correlation_id: self.producer.next_correlation_id(),
                        registration_id: correlation_id,
                    };
                    let _ = self.send(&remove);
                    return Err(ClientError::Driver {
                        what: format!("counter ready with counter id {counter_id}"),
                    });
                };
                Ok(Counter::new(
                    crate::owned::Owned::new(cell, Arc::clone(&self.shared)),
                    Arc::clone(&self.shared),
                    producer,
                    self.client_id,
                    correlation_id,
                    type_id,
                ))
            }
            other => Err(ClientError::Driver {
                what: format!("{other:?} in answer to add counter"),
            }),
        }
    }

    /// Stops recording `recording_id`, whichever client started it: the driver writes the
    /// catalog's stop at the position written and answers once it is synced
    /// (`spec/client-protocol.md`, "Commands"). A stop whose own writes or syncs fail is
    /// success too, since the recording is stopped all the same; the driver counts it in
    /// `recording.write_failed` and announces the position last synced, which
    /// [`Recording::stop_position`] returns (ADR 0064). A recording already stopped is
    /// success with nothing written; an id the catalog does not hold is
    /// [`ClientError::Refused`] with error code 21.
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

    /// Has the driver's recorder sync what it has written of recording `recording_id`
    /// now, without waiting for its sync once a second, and waits for the answer
    /// (`spec/client-protocol.md`, "Commands"; ADR 0069). For a live recording, `Ok`
    /// means that everything the recorder had written when it took the request is
    /// covered by a sync that returned success, and `recording.synced_position` says
    /// how far that is. It does not mean that everything this client published before
    /// the call is covered: the recorder may not have written it yet. A caller that
    /// needs a position compares [`Recording::synced_position`] with it, and calls
    /// again while that is behind and [`Recording::stop_position`] is `None`; one sync
    /// covers everything written since the last.
    ///
    /// A recording that has stopped is `Ok` with nothing done, since nothing is left to
    /// sync. What is durable of it is its stop position, which `recording stopped`
    /// carried and [`Recording::stop_position`] returns. That is below what the
    /// recorder wrote when a sync of the recording, or the stop's own writes or syncs,
    /// failed (ADR 0064, ADR 0065). So `Ok` alone never says that a position is on the
    /// disk: [`Recording::synced_position`] says it, live or stopped, and a caller
    /// acknowledges up to that position and no further.
    ///
    /// An id the catalog does not hold is [`ClientError::Refused`] with error code 21.
    /// The recorder's request queue full is code 10: call again. A sync that failed is
    /// code 12: the driver gives the recording up, `recording.write_failed` counts it,
    /// and [`Recording::stop_position`] becomes the position the last sync that
    /// succeeded covered (ADR 0065). A driver older than client protocol version 8
    /// answers code 6.
    ///
    /// Each call that finds something unsynced costs the recorder one sync of the
    /// segment file and one of the time index, on the thread that writes every
    /// recording, so call it when there is something to acknowledge, not in a loop.
    pub fn sync_recording(&mut self, recording_id: i64) -> Result<(), ClientError> {
        let correlation_id = self.producer.next_correlation_id();
        let command = Command::SyncRecording {
            client_id: self.client_id,
            correlation_id,
            recording_id,
        };
        match self.request(&command, correlation_id, "sync recording")? {
            Response::Success { .. } => Ok(()),
            other => Err(ClientError::Driver {
                what: format!("{other:?} in answer to sync recording"),
            }),
        }
    }

    /// Sends `sync recording` for `recording_id` and returns without waiting for the
    /// answer, which the conductor drops when it comes: [`Recording::request_sync`].
    pub(crate) fn request_sync_recording(&mut self, recording_id: i64) -> Result<(), ClientError> {
        if let Some(reason) = self.shared.closed() {
            return Err(ClientError::Closed(reason));
        }
        let correlation_id = self.producer.next_correlation_id();
        self.send(&Command::SyncRecording {
            client_id: self.client_id,
            correlation_id,
            recording_id,
        })
    }

    /// Truncates stopped recording `recording_id` to `position`: the driver removes the
    /// segment files past it, cuts the one holding it so the bytes past the position are
    /// zero, cuts the time index to the entries at or before it, and writes the stop at
    /// the position, counted in `recording.truncated`; a replay of the recording then
    /// ends there (`spec/client-protocol.md`, "Commands"; ADR 0031). `position` is a
    /// multiple of 32 at which a frame of the recording begins, from its retained
    /// position to its stop position. The driver answers once the files and the catalog
    /// are written, one file per duty cycle, within the response timeout for a
    /// recording of the order of ten thousand segment files. A recording the catalog
    /// does not hold is [`ClientError::Refused`] with error code 21; one that is live,
    /// read by a replay or under another of these commands code 24, whose message says
    /// which and what to do; a position outside its rule code 23, naming the retained
    /// and stop positions; a file the driver could not read, cut or remove code 12,
    /// after which the driver has repaired the recording's slot from its files; and the
    /// driver's registry of commands in progress at capacity code 10.
    pub fn truncate_recording(
        &mut self,
        recording_id: i64,
        position: i64,
    ) -> Result<(), ClientError> {
        let correlation_id = self.producer.next_correlation_id();
        let command = Command::TruncateRecording {
            client_id: self.client_id,
            correlation_id,
            recording_id,
            position,
        };
        match self.request(&command, correlation_id, "truncate recording")? {
            Response::Success { .. } => Ok(()),
            other => Err(ClientError::Driver {
                what: format!("{other:?} in answer to truncate recording"),
            }),
        }
    }

    /// Purges the segment files of stopped recording `recording_id` before
    /// `new_start_position`: the driver writes the position as the recording's retained
    /// position, then removes the segment files whose bytes lie wholly before it, one
    /// per duty cycle, each counted in `recording.segments_purged`; the time index keeps
    /// its entries, and a replay from the start, or from a position before the retained
    /// position, is refused with code 23 naming it (`spec/client-protocol.md`,
    /// "Commands"; ADR 0031). `new_start_position` is a segment base of the recording
    /// above its retained position and at or below its stop position; a base at or
    /// below the retained position is nothing to purge and is code 23, as an earlier
    /// purge already took it. The error codes are
    /// [`truncate_recording`](Self::truncate_recording)'s.
    pub fn purge_segments(
        &mut self,
        recording_id: i64,
        new_start_position: i64,
    ) -> Result<(), ClientError> {
        let correlation_id = self.producer.next_correlation_id();
        let command = Command::PurgeSegments {
            client_id: self.client_id,
            correlation_id,
            recording_id,
            new_start_position,
        };
        match self.request(&command, correlation_id, "purge segments")? {
            Response::Success { .. } => Ok(()),
            other => Err(ClientError::Driver {
                what: format!("{other:?} in answer to purge segments"),
            }),
        }
    }

    /// Removes every file of stopped recording `recording_id` and marks its catalog
    /// slot deleted, state 3, which no command reads again and the catalog never
    /// reuses, counted in `recording.deleted` (`spec/client-protocol.md`, "Commands";
    /// ADR 0031). The driver writes the state first, then removes the segment files
    /// one per duty cycle and the time index last, so a crash in between leaves a
    /// deleted slot whose files recovery removes. The error codes are
    /// [`truncate_recording`](Self::truncate_recording)'s, less 23.
    pub fn purge_recording(&mut self, recording_id: i64) -> Result<(), ClientError> {
        let correlation_id = self.producer.next_correlation_id();
        let command = Command::PurgeRecording {
            client_id: self.client_id,
            correlation_id,
            recording_id,
        };
        match self.request(&command, correlation_id, "purge recording")? {
            Response::Success { .. } => Ok(()),
            other => Err(ClientError::Driver {
                what: format!("{other:?} in answer to purge recording"),
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
    /// does not move and ends after the replay connect timeout. On a `cavalry:udp`
    /// channel the replay's publication is put on the wire by the driver's sender as a
    /// client's `cavalry:udp` publication is, with the recording's term length and MTU,
    /// and its subscriber is a `cavalry:udp` subscription on that channel's endpoint,
    /// on this host or another, whose image's positions are the recording's; for the
    /// replay connect timeout it has a subscriber while its publication is connected,
    /// from the receiver's first status message until the connection timeout passes
    /// without one, and no subscription on this driver gets an `available image` of it
    /// but a `cavalry:udp` one on the same endpoint, through the receiver (ADR 0043).
    /// Waits for `replay ready`
    /// up to the response timeout and returns the [`Replay`], whose
    /// [`position`](Replay::position) reads the replayer's counter and whose
    /// [`ended`](Replay::ended) says how it ended once it has. The replay belongs to the
    /// driver: dropping the handle stops nothing, and [`Cavalry::stop_replay`] from any
    /// client stops it. A recording the catalog does not hold is
    /// [`ClientError::Refused`] with error code 21, a position out of range code 23, a
    /// length below −1 code 7, a channel that is neither `cavalry:ipc` nor a
    /// `cavalry:udp` channel code 1, a `cavalry:udp` host that does not resolve code 9
    /// and an endpoint whose socket the operating system refuses code 4, a segment file
    /// that cannot be opened or a log that cannot be created code 12, and a registry,
    /// the counters file or the sender's queue at capacity code 10. The client follows
    /// the end of every
    /// replay it started while the handle lives and the replay has not ended, as many
    /// as the driver allows at once; a client that holds the driver's bound of live
    /// replays and starts another is [`ClientError::ReplaysFull`], and the new one is
    /// stopped again. A frame's position in the recording is
    /// [`crate::Image::position_of`] of its header, or the same arithmetic from
    /// [`crate::Image::positions`] in the handler.
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
            return Err(ClientError::ReplayQueueFull {
                limit: REPLAY_QUEUE,
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
                // The conductor admits or refuses every queued registration before it
                // hands over an answer, and marks a refused one (`ReplayRegistry::admit`):
                // a replay nobody would tell of its end is stopped again, and refused.
                if end.refused() {
                    let _ = self.stop_replay(replay_session_id);
                    return Err(ClientError::ReplaysFull { limit: MAX_REPLAYS });
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
    fn replay_position_counter(
        &self,
        replay_session_id: i64,
    ) -> Result<Option<CounterValue>, ClientError> {
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
    /// closed before or meanwhile, and a command that finds the driver gone and so
    /// closes the client (ADR 0155), is [`ClientError::Closed`]; no answer is
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
        match self.producer.write(
            command.command_type() as i32,
            &self.scratch[..length],
            epoch_time(),
        ) {
            Ok(()) => {
                self.shared.sent(nano_time());
                Ok(())
            }
            // The ring's own look at the driver's heartbeat, which the conductor
            // takes at its next look: the same finding, so the same
            // close, and one error for a driver that is gone whichever looks first
            // (ADR 0155).
            Err(CommandRingError::DriverClosed) => Err(self.closed_for(Closed::DriverClosed)),
            Err(CommandRingError::DriverTimeout { .. }) => {
                Err(self.closed_for(Closed::DriverTimeout))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Closes the client for `reason`, unless it is closed, and gives the close as
    /// the error of the command that found it.
    fn closed_for(&self, reason: Closed) -> ClientError {
        self.shared.close(reason);
        ClientError::Closed(self.shared.closed().unwrap_or(reason))
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
    /// client timed out, or the broadcast buffer lapped. A command that finds the
    /// driver gone, on the command ring, before the conductor's next look, closes
    /// the client for the same reason and returns [`ClientError::Closed`] (ADR 0155).
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

    /// The counters file as the handles that check a record before they trust its
    /// value hold it ([`Recording`], [`CommittedLog`]).
    pub(crate) fn shared_counters(&self) -> Arc<CountersReader> {
        Arc::clone(&self.counters)
    }

    /// Opens the committed log of the cluster member on this client's driver at
    /// `position`, for a service to read (`spec/cluster.md`, "The service"; ADR 0066):
    /// it adds a subscription on `cavalry:ipc` and stream [`SERVICE_LOG_STREAM_ID`] and
    /// starts a replay, from `position`, that follows the recording of the member's
    /// log that holds it.
    /// `position` is where an entry begins: 0, or [`CommittedLog::position`] of an
    /// earlier handle.
    ///
    /// Returns `None` while the driver hosts no member that has a recording of its
    /// log: the member has not started, its election has not given it a log yet, it
    /// has closed, or it named another recording, or closed and removed its file,
    /// while this call read its file of recordings: with no file the call loads the
    /// member's counter again, and opens the live recording only when the counter
    /// still names it. A service that starts before its member calls again, with a
    /// pause between two calls: each reads the counters file's records to find the
    /// member, which is not a call for a loop that spins. Nothing is sent to the
    /// driver until a member is found.
    ///
    /// Returns `None` too while the member's live recording has not reached
    /// `position` (`spec/cluster.md`, "The service"; ADR 0135). A follower that
    /// caught up through a replay can hold the end of its log in its recording of
    /// that replay, above what its recording of the live log has synced, as it does
    /// while the leader's log is paced by a slower member. A service that read the
    /// replay's recording to its stop is then at a position that the live recording
    /// reaches later. The call reads the driver's `recording.synced_position` of
    /// the live recording, sends the driver nothing, and answers `None` until that
    /// counter has reached `position`. A position past the end of the member's log
    /// is answered so as well: the call cannot tell it from one the log will reach.
    /// When the live recording has stopped below `position` and the member names it
    /// still, the driver is asked and refuses, and beside a member that writes its
    /// file of recordings the answer is `None` again, until the member has named
    /// its next recording. The caller asks again after a pause, as after any
    /// `None`: each call reads the member's file.
    ///
    /// The refusals are those of [`Cavalry::add_subscription`] and
    /// [`Cavalry::start_replay`]: a position that is not a multiple of 32, or is
    /// below every recording of its log the member keeps, or is one that a
    /// recording the member lists no longer holds after a purge, or is past the
    /// stop of the live recording of a member that writes no file, is
    /// [`ClientError::Refused`] with code 23, and a driver whose
    /// replays registry is full, 64 replays, is code 10. The member's file of its
    /// log's recordings that cannot be read, or that fails a check of its page, is
    /// [`ClientError::Io`], which names the file and the check.
    ///
    /// A member's log is one recording, which begins at 0, until the member leaves
    /// the log and joins it again, a later term begins, or the member catches up
    /// through a replay; then it is several, in a row. The member lists them in a
    /// file in its driver directory, and this call replays the one that holds
    /// `position`: the first the file lists that does, and the recording the member
    /// records the live log into when none does (`spec/cluster.md`, "The service" and
    /// "The log's recordings"; ADR 0103). A handle is one recording's. On a recording
    /// below the live one it is ended at that recording's stop, with no report set,
    /// and the next call, at [`CommittedLog::position`], replays the next. So a
    /// service that starts at 0 beside a member whose log is in three recordings
    /// opens the log three times, and reads every entry once, in order. A member
    /// keeps the last 1,024 recordings it stopped: a position below them is refused
    /// with code 23.
    ///
    /// The call at the position where a handle ended whole has a third answer, beside
    /// the next recording and `None`: a handle on the same recording,
    /// [`CommittedLog::recording_id`] unchanged. When the member still names that
    /// recording as its live one, the recording has stopped there and the member has
    /// not yet left its log, as for the seconds a follower takes to leave a leader it
    /// lost once its driver has retired the leader's image, or after a write to the
    /// recording's segment file failed: the handle is ended at once, with no report
    /// set and no entry. When the member has named a later recording since, the same
    /// recording lies below it, and the handle reads on in it. A service asks again
    /// after a pause in both cases, as it does after `None`, and not in a loop that
    /// spins.
    ///
    /// A service reads a handle until [`CommittedLog::is_ended`] says that no poll will
    /// deliver another entry, and not until a poll returns 0: when its member closes or
    /// leaves the log, entries below the bound the handle took may still be on their
    /// way from the replay, and a service that stopped short of them would start again
    /// at a position the member's next recording does not hold. It keeps polling until
    /// then: the driver withdraws the replay's image 5 s after the replay ends, read or
    /// not, and a handle whose image went before it was read is ended short, which
    /// [`CommittedLog::withdrawn_at`] says. A handle is ended short too when the
    /// driver's replay of the recording failed, which [`CommittedLog::failed_at`]
    /// says, and when this client's read of the replay's image stopped on a fault,
    /// which the poll that met it returned and [`CommittedLog::faulted`] says from
    /// then on. A service reads the three once the handle is ended, and has been
    /// given every entry it was owed only when all three are `None`. It reads the
    /// replay's failure first: a replay that failed can leave an image that faults
    /// too, and the failure is then the cause.
    ///
    /// ```
    /// use cavalry_client::{Cavalry, CommittedLog, Fault};
    ///
    /// /// A service's loop: every committed entry from `position` on, in order, each
    /// /// once, until the member's log ends. Returns where to open the log again, or
    /// /// an error that says which entries it was not given.
    /// fn apply(client: &mut Cavalry, position: i64) -> Result<i64, Box<dyn std::error::Error>> {
    ///     // Ask until the member on this driver has its log.
    ///     let mut log: CommittedLog = loop {
    ///         if let Some(log) = client.committed_log(position)? {
    ///             break log;
    ///         }
    ///         std::thread::sleep(std::time::Duration::from_millis(100));
    ///     };
    ///     loop {
    ///         let polled = log.poll(
    ///             |entry, _header, entry_position| {
    ///                 // Apply `entry`, in `spec/cluster.md`'s encoding, at its position.
    ///                 let _ = (entry, entry_position);
    ///             },
    ///             64,
    ///         );
    ///         let entries = match polled {
    ///             Ok(entries) => entries,
    ///             // The fault of the log's own image: the handle is ended, and keeps
    ///             // the fault, which is read below after the other two reports.
    ///             Err(Fault::Image { .. } | Fault::ImagesFull { .. }) => 0,
    ///             // The client is closed, and `Cavalry::closed` says why.
    ///             Err(closed) => return Err(closed.into()),
    ///         };
    ///         if log.is_ended() {
    ///             let next = log.position();
    ///             let failed = log.failed_at();
    ///             let withdrawn = log.withdrawn_at();
    ///             let faulted = log.faulted();
    ///             log.close(client)?;
    ///             if let Some(failed) = failed {
    ///                 // The driver could not play the member's recording on from
    ///                 // `failed`: the entries from there on were not delivered,
    ///                 // though the member may have committed them.
    ///                 return Err(format!(
    ///                     "the replay of the member's recording failed at position \
    ///                      {failed}: the log was not opened at the position of an entry, \
    ///                      or the recording's files are damaged; run `cavalry doctor`, \
    ///                      which names a damaged recording, and open the log again from \
    ///                      position {next} if this handle read entries, or from 0; see \
    ///                      docs/how-to/read-the-committed-log.md#when-it-does-not-work"
    ///                 )
    ///                 .into());
    ///             }
    ///             if let Some(unread) = withdrawn {
    ///                 // The driver withdrew the replay's image before it was read: the
    ///                 // entries from `unread` on were not delivered.
    ///                 return Err(format!(
    ///                     "entries from position {unread} were withdrawn unread, 5 s after \
    ///                      the replay ended; open the log again from position {next}; see \
    ///                      docs/how-to/read-the-committed-log.md#when-it-does-not-work"
    ///                 )
    ///                 .into());
    ///             }
    ///             if let Some(fault) = faulted {
    ///                 // This client's read of the replay's image stopped: the entries
    ///                 // from `next` on were not delivered through this handle.
    ///                 return Err(format!(
    ///                     "the read of the member's log stopped before position {next} \
    ///                      was read; open the log again from there: {fault}"
    ///                 )
    ///                 .into());
    ///             }
    ///             // None of the three: every entry the handle's recording holds
    ///             // below the last commit position the handle took has been
    ///             // delivered. The caller opens the log again from `next`, after a
    ///             // pause when that is the same recording or `None`.
    ///             return Ok(next);
    ///         }
    ///         if entries == 0 {
    ///             // Nothing below the bound has arrived yet.
    ///             std::thread::yield_now();
    ///         }
    ///     }
    /// }
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let dir = std::env::temp_dir().join(format!("cavalry-doc-committed-{}", std::process::id()));
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// # let driver = cavalry_driver::Driver::start(&dir, dir.join("log"))?;
    /// let mut client = Cavalry::connect_to(&dir)?;
    /// // A driver that hosts no cluster member has no committed log to open, and a
    /// // service beside it asks again later, as `apply` does.
    /// assert!(client.committed_log(0)?.is_none());
    /// # let _ = apply;
    /// # client.close()?;
    /// # driver.close()?;
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [`SERVICE_LOG_STREAM_ID`]: crate::SERVICE_LOG_STREAM_ID
    pub fn committed_log(&mut self, position: i64) -> Result<Option<CommittedLog>, ClientError> {
        CommittedLog::open(self, crate::SERVICE_LOG_STREAM_ID, position)
    }

    /// [`committed_log`](Self::committed_log) on a stream the caller names: the
    /// subscription and the replay are on `cavalry:ipc` and `stream_id`. A service
    /// reads on the service log stream, which is what `committed_log` passes. A
    /// member's consensus module reads its own log on the log replay stream,
    /// [`LOG_REPLAY_STREAM_ID`](crate::LOG_REPLAY_STREAM_ID), for its sessions, so that
    /// its replay and a service's never reach each other's subscription: a reader
    /// that opens its log again then costs no other reader an image to open
    /// (`spec/cluster.md`, "Members and streams"; ADR 0116).
    ///
    /// ```
    /// use cavalry_client::{Cavalry, LOG_REPLAY_STREAM_ID};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let dir = std::env::temp_dir().join(format!("cavalry-doc-committed-on-{}", std::process::id()));
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// # let driver = cavalry_driver::Driver::start(&dir, dir.join("log"))?;
    /// let mut client = Cavalry::connect_to(&dir)?;
    /// // A driver that hosts no cluster member has no committed log on any stream.
    /// assert!(client.committed_log_on(LOG_REPLAY_STREAM_ID, 0)?.is_none());
    /// # client.close()?;
    /// # driver.close()?;
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// # Ok(())
    /// # }
    /// ```
    pub fn committed_log_on(
        &mut self,
        stream_id: i32,
        position: i64,
    ) -> Result<Option<CommittedLog>, ClientError> {
        CommittedLog::open(self, stream_id, position)
    }

    /// Opens a session with the cluster that `members` names, the members string the
    /// cluster's members were started with (`spec/cluster.md`, "Members and streams"
    /// and "Sessions"; ADR 0117). The cluster answers the session on
    /// `response_channel`, `cavalry:udp?endpoint=<host:port>`, a host and port of this
    /// client's host that every member can reach.
    ///
    /// The call subscribes to the response channel, adds a publication to every
    /// member's `ingress` endpoint, and sends a session connect to one member after
    /// another, the first in the string first. A member that is not the leader
    /// redirects, and the next connect goes to the leader it names; a connect with no
    /// answer in the session connect timeout, 5 s, was lost, and the next goes to the
    /// next member. The call returns the [`ClusterSession`] once the leader has
    /// opened it, which is once a quorum holds the session's first entry.
    ///
    /// The call takes no session message from the response channel. A service may
    /// reply to the session from its open event on, and that reply can arrive before
    /// the leader's answer does: it stays on the response channel, and the session's
    /// calls of [`ClusterSession::poll`] hand it over (ADR 0118).
    ///
    /// It gives up after 20 s, [`ClusterError::ConnectTimeout`]: long enough for a
    /// cluster that has just lost its leader to elect another. The leader's refusal
    /// is [`ClusterError::Refused`] with its reason, a members string that fails a
    /// check [`ClusterError::Members`], and a cluster of another protocol version
    /// [`ClusterError::Version`].
    ///
    /// ```no_run
    /// use cavalry_client::{Cavalry, ClusterError};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut client = Cavalry::connect()?;
    /// let members = "0,10.0.0.1:9010,10.0.0.1:9020,10.0.0.1:9030,10.0.0.1:9040,10.0.0.1:9050\
    ///                |1,10.0.0.2:9010,10.0.0.2:9020,10.0.0.2:9030,10.0.0.2:9040,10.0.0.2:9050\
    ///                |2,10.0.0.3:9010,10.0.0.3:9020,10.0.0.3:9030,10.0.0.3:9040,10.0.0.3:9050";
    /// let mut session = client.connect_cluster(members, "cavalry:udp?endpoint=10.0.0.9:9100")?;
    /// session.offer(b"hello")?;
    /// // The session's duty cycle: the cluster's replies, and a keepalive when due.
    /// while session.closed().is_none() {
    ///     session.poll(|reply, header| println!("term {}: {} bytes", header.leadership_term_id, reply.len()), 10)?;
    /// #   break;
    /// }
    /// session.close()?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// The block is compiled and not run: it needs a cluster of three members on
    /// three hosts. `docs/how-to/run-a-cluster.md`, "Run a cluster of three members",
    /// runs one on one host, on the loopback.
    pub fn connect_cluster(
        &mut self,
        members: &str,
        response_channel: &str,
    ) -> Result<ClusterSession, ClusterError> {
        ClusterSession::connect(self, members, response_channel)
    }

    /// A cluster's service beside the member on this client's driver
    /// (`spec/cluster.md`, "The service"; ADR 0118). [`ClusterService::poll`] reads the
    /// member's committed log from its start and hands over each entry as an
    /// [`Entry`](crate::Entry) with its position and the cluster's time, and the
    /// handler answers a session through [`Replies::reply`](crate::Replies::reply).
    /// The service beside the member that led an entry's term sends its replies,
    /// while that member leads the term still, and the service beside every other
    /// member, which runs the same code, sends none.
    /// Nothing is opened here: the first poll opens the member's log, once the driver
    /// hosts a member that has one.
    ///
    /// ```
    /// use cavalry_client::{Action, Cavalry, Entry, Reply};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let dir = std::env::temp_dir().join(format!("cavalry-doc-service-{}", std::process::id()));
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// # let driver = cavalry_driver::Driver::start(&dir, dir.join("log"))?;
    /// let mut client = Cavalry::connect_to(&dir)?;
    /// let mut service = client.cluster_service();
    /// let mut total = 0u64;
    /// // An echo that counts: the same state on every member, answered by one.
    /// let taken = service.poll(
    ///     &mut client,
    ///     |committed, replies| {
    ///         if let Entry::SessionMessage { session_id, payload } = committed.entry {
    ///             // Reply first, then change the state: an entry that is aborted is
    ///             // handed over again.
    ///             if replies.reply(session_id, payload) == Reply::Retry {
    ///                 return Action::Abort;
    ///             }
    ///             total += payload.len() as u64;
    ///         }
    ///         Action::Continue
    ///     },
    ///     10,
    /// )?;
    /// // A driver that hosts no cluster member has no committed log to read.
    /// assert_eq!((taken, total, service.position()), (0, 0, 0));
    /// # service.close(&mut client)?;
    /// # client.close()?;
    /// # driver.close()?;
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// # Ok(())
    /// # }
    /// ```
    pub fn cluster_service(&self) -> ClusterService {
        ClusterService::new()
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
                    epoch_time(),
                );
            }
        }
        if let Some(handle) = self.conductor.take() {
            // The conductor reads the closed state every pass and is parked for at most
            // a millisecond between passes. The unpark, after the state is stored, ends
            // that park, so the join does not wait on the park's timeout: on one Mac a
            // wait of 1 ms in about 40 million did not return until its thread was
            // woken, and this join then waited for ever (ADR 0171).
            handle.thread().unpark();
            let _ = handle.join();
        }
        // The watch reads the closed state after each park, of a tenth of a second at
        // most; the unpark ends that park.
        if let Some(handle) = self.watch.take() {
            handle.thread().unpark();
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use cavalry_driver::Driver;
    use cavalry_shm::counters::generated::{CLIENT_WITHOUT_LOCK, CLUSTER_COMMIT_POSITION};

    use super::*;

    /// A client that is stopped in the middle of its attach while the driver is closed
    /// and another is started in the directory, and is then continued. Wherever it
    /// was stopped, it ends as a client of the driver that runs: its mappings, its id
    /// and its lock file are that driver's, the one lock file in `clients/` is its
    /// own and is held, and nothing of the earlier driver's client is in the
    /// directory of the next. The three steps are the three places where a mix of two
    /// drivers could begin: before the first file is opened, between two of them, and
    /// between the id and the lock file.
    #[test]
    fn a_client_stopped_in_its_attach_across_a_restart_attaches_to_the_driver_that_runs() {
        for step in 1..=3u8 {
            let dir = std::env::temp_dir().join(format!(
                "cavalry-attach-restart-{step}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            let earlier = Driver::start(&dir, dir.join("log")).expect("the earlier driver");
            // A client of the earlier driver first, so that the id the stopped client
            // takes from that driver is not the first id of the driver that starts.
            let ahead = Cavalry::connect_to(&dir).expect("a client of the earlier driver");
            let ahead_id = ahead.client_id();
            drop(ahead);
            let drivers = Rc::new(RefCell::new((Some(earlier), None::<Driver>)));
            let restart = Rc::clone(&drivers);
            let at = dir.clone();
            ATTACH_STEP.with(|test| {
                *test.borrow_mut() = Some(Box::new(move |reached| {
                    let mut drivers = restart.borrow_mut();
                    if reached != step {
                        return;
                    }
                    let Some(earlier) = drivers.0.take() else {
                        return;
                    };
                    earlier.close().expect("the earlier driver closes");
                    drivers.1 = Some(Driver::start(&at, at.join("log")).expect("the next driver"));
                }));
            });
            let connected = Cavalry::connect_to(&dir);
            ATTACH_STEP.with(|test| *test.borrow_mut() = None);
            let mut client = connected.unwrap_or_else(|error| panic!("step {step}: {error}"));
            assert!(
                drivers.borrow().1.is_some(),
                "step {step}: the driver was started again in the middle of the attach"
            );

            // The one lock file there is this client's, and it is held.
            let names: Vec<String> = std::fs::read_dir(dir.join("clients"))
                .expect("the directory of the locks")
                .map(|entry| {
                    entry
                        .expect("an entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            assert_eq!(
                names,
                vec![format!("{}.lock", client.client_id())],
                "step {step}: what the next driver's directory holds"
            );
            let tried =
                std::fs::File::open(dir.join("clients").join(&names[0])).expect("the lock file");
            assert!(
                matches!(tried.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
                "step {step}: the client holds its lock"
            );
            // It is a client of the driver that runs: that driver answers it.
            let counter = client
                .add_counter(CLUSTER_COMMIT_POSITION.0, &1i32.to_le_bytes(), "a counter")
                .unwrap_or_else(|error| panic!("step {step}: {error}"));
            assert_eq!(
                client
                    .counters()
                    .read(CLIENT_WITHOUT_LOCK.record())
                    .expect("a static counter"),
                0,
                "step {step}: the driver found the client's lock held at its first command"
            );
            // Ids begin again with each driver. The earlier driver gave its first id
            // to the client above and its second to that client's keepalive, so an id
            // from its ring is two or more above the first. The driver that runs gave
            // its first id to this client, or to this client's attempt that found the
            // directory gone and its second to this client.
            assert!(
                client.client_id() <= ahead_id + 1,
                "step {step}: id {} is from the ring of the earlier driver, whose first was \
                 {ahead_id}",
                client.client_id()
            );
            counter.close();
            client.close().expect("the client closes");
            let next = drivers.borrow_mut().1.take().expect("the next driver");
            next.close().expect("the next driver closes");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
