//! The driver on disk, driven by a client that attaches through cavalry-shm and speaks
//! cavalry-proto's client protocol: the full cases first, the directory protocol, every
//! command, a registry at capacity, the linger, the client timeout, and the recording
//! commands against the durable log (`spec/client-protocol.md`; CLAUDE.md, invariants
//! 1, 3 and 4).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cavalry_driver::{Driver, DriverThread, IdleStrategy, ThreadStart, spec};
use cavalry_log::catalog::{CatalogReader, CatalogWriter};
use cavalry_log::layout::{Recording, State};
use cavalry_proto::client_protocol::{
    Command, CommandType, Event, IPC_CHANNEL, NULL_LENGTH, NULL_POSITION,
};
use cavalry_shm::broadcast::{BroadcastBuffer, Receiver};
use cavalry_shm::command_ring::{CommandRing, Producer};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    CLIENT_PROTOCOL_ERRORS, CLIENT_PROTOCOL_REJECTED_COMMAND, CLIENT_TIMED_OUT,
    CONDUCTOR_IDLE_STRATEGY, DRIVER_REGISTRY_FULL, PUBLICATION_POSITION, RECORDING_INDEX_REWRITTEN,
    RECORDING_RECOVERED, RECORDING_STARTED, RECORDING_STOPPED, REPLAY_ENDED, REPLAY_START_FAILED,
    REPLAY_STARTED, REPLAY_STOPPED,
};
use cavalry_shm::log::LogBuffers;

fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        })
}

/// A directory of its own per test, removed when the guard drops.
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

/// A client: a producer on the command ring and a receiver on the broadcast buffer.
struct Client {
    id: i64,
    producer: Producer,
    receiver: Receiver,
    buffer: Vec<u8>,
}

impl Client {
    fn attach(dir: &Path) -> Self {
        let ring = CommandRing::open(dir, true).expect("the ring opens");
        let producer = ring.producer(spec::DRIVER_TIMEOUT).expect("a producer");
        let broadcast = BroadcastBuffer::open(dir, false).expect("the buffer opens");
        let receiver = broadcast.receiver().expect("a receiver");
        let id = producer.next_correlation_id();
        Client {
            id,
            producer,
            receiver,
            buffer: vec![0u8; 8192],
        }
    }

    fn correlation_id(&self) -> i64 {
        self.producer.next_correlation_id()
    }

    fn send(&mut self, command: &Command<'_>) {
        let length = command
            .write(&mut self.buffer)
            .expect("the command encodes");
        self.producer
            .write(
                command.command_type() as i32,
                &self.buffer[..length],
                now_nanos(),
            )
            .expect("the command is written");
    }

    /// Sends a raw record, for the commands the protocol refuses.
    fn send_raw(&mut self, type_id: i32, message: &[u8]) {
        self.producer
            .write(type_id, message, now_nanos())
            .expect("the record is written");
    }

    /// Waits up to `timeout` for an event `pick` accepts, and returns what it picked.
    fn wait<T>(&mut self, timeout: Duration, mut pick: impl FnMut(&Event<'_>) -> Option<T>) -> T {
        let deadline = Instant::now() + timeout;
        loop {
            let mut picked = None;
            let got = self
                .receiver
                .receive(|type_id, message| {
                    if let Ok(event) = Event::parse(type_id, message) {
                        picked = pick(&event);
                    }
                })
                .expect("the receiver reads");
            if let Some(value) = picked {
                return value;
            }
            if !got {
                assert!(Instant::now() < deadline, "no event within {timeout:?}");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn add_publication(&mut self, stream_id: i32, exclusive: bool) -> (i64, i64, i32, i32, String) {
        let correlation_id = self.correlation_id();
        let command = if exclusive {
            Command::AddExclusivePublication {
                client_id: self.id,
                correlation_id,
                stream_id,
                channel: IPC_CHANNEL,
            }
        } else {
            Command::AddPublication {
                client_id: self.id,
                correlation_id,
                stream_id,
                channel: IPC_CHANNEL,
            }
        };
        self.send(&command);
        self.wait(Duration::from_secs(5), |event| match *event {
            Event::PublicationReady {
                correlation_id: id,
                registration_id,
                session_id,
                publication_limit_counter_id,
                log_file_name,
                ..
            }
            | Event::ExclusivePublicationReady {
                correlation_id: id,
                registration_id,
                session_id,
                publication_limit_counter_id,
                log_file_name,
                ..
            } if id == correlation_id => Some((
                correlation_id,
                registration_id,
                session_id,
                publication_limit_counter_id,
                String::from_utf8_lossy(log_file_name).into_owned(),
            )),
            _ => None,
        })
    }

    fn add_subscription(&mut self, stream_id: i32) -> i64 {
        let correlation_id = self.correlation_id();
        self.send(&Command::AddSubscription {
            client_id: self.id,
            correlation_id,
            stream_id,
            channel: IPC_CHANNEL,
        });
        self.wait(Duration::from_secs(5), |event| match *event {
            Event::SubscriptionReady { correlation_id: id } if id == correlation_id => Some(id),
            _ => None,
        })
    }

    fn wait_error(&mut self, correlation_id: i64) -> (i32, String) {
        self.wait(Duration::from_secs(5), |event| match *event {
            Event::Error {
                offending_correlation_id,
                error_code,
                error_message,
            } if offending_correlation_id == correlation_id => Some((
                error_code,
                String::from_utf8_lossy(error_message).into_owned(),
            )),
            _ => None,
        })
    }
}

fn counter(dir: &Path, id: cavalry_shm::counters::CounterId) -> i64 {
    CountersReader::open(dir)
        .expect("the counters file opens")
        .read(id.record())
        .expect("a static counter")
}

#[test]
fn a_live_directory_is_refused_and_a_dead_one_is_remade() {
    let dir = TestDir::new("directory");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    for file in ["counters", "command-ring", "broadcast-buffer"] {
        assert!(dir.path().join(file).is_file(), "{file} exists");
    }
    let refused =
        Driver::start(dir.path(), dir.path().join("log")).expect_err("a second driver is refused");
    assert_eq!(refused.reason(), "driver-active");
    assert!(
        refused
            .to_string()
            .contains("docs/reference/client-protocol.md#the-driver-directory")
    );
    let first_start = CommandRing::open(dir.path(), false)
        .expect("opens")
        .header()
        .start_timestamp();
    driver.close().expect("the driver closes");
    assert_eq!(
        CommandRing::open(dir.path(), false)
            .expect("opens")
            .consumer_heartbeat(),
        -1,
        "a closed driver leaves -1 in the heartbeat"
    );
    // A dead driver's directory: a ring whose heartbeat is older than the driver timeout.
    let again = Driver::start(dir.path(), dir.path().join("log"))
        .expect("the next driver remakes the directory");
    let second_start = CommandRing::open(dir.path(), false)
        .expect("opens")
        .header()
        .start_timestamp();
    assert!(second_start > first_start, "the files were made anew");
    again.close().expect("closes");

    // A half-created directory, the ring missing, is a dead driver's too.
    std::fs::remove_file(dir.path().join("command-ring")).expect("remove the ring");
    let third = Driver::start_with(dir.path(), dir.path().join("log"), IdleStrategy::Yield)
        .expect("starts on a half directory");
    assert!(dir.path().join("command-ring").is_file());
    third.close().expect("closes");
}

#[test]
fn the_idle_strategy_is_recorded_in_its_counter() {
    let dir = TestDir::new("idle");
    let strategy = |dir: &Path| {
        CountersReader::open(dir)
            .expect("opens")
            .read(CONDUCTOR_IDLE_STRATEGY.record())
            .expect("reads")
    };
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    assert_eq!(strategy(dir.path()), IdleStrategy::Backoff.counter_value());
    assert_eq!(strategy(dir.path()), 0, "the default reads 0");
    driver.close().expect("closes");
    for idle in IdleStrategy::ALL {
        let driver = Driver::start_with(dir.path(), dir.path().join("log"), idle).expect("starts");
        let value = strategy(dir.path());
        assert_eq!(
            IdleStrategy::from_counter_value(value),
            Some(idle),
            "{idle:?}"
        );
        driver.close().expect("closes");
    }
}

/// A replayer that holds no replay is parked, for at most a second at a time, and
/// `close` wakes it: a driver with nothing replaying closes in far less than that
/// second. A `start replay` wakes it the same way, which the replay tests below show
/// through their deadlines.
#[test]
fn closing_a_driver_wakes_its_parked_replayer() {
    let dir = TestDir::new("parked-replayer");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    // The replayer reaches its park after the backoff's spins and yields, microseconds.
    std::thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    driver.close().expect("closes");
    let took = started.elapsed();
    assert!(
        took < Duration::from_millis(500),
        "close took {took:?}: it waited for the replayer's park timeout instead of waking it"
    );
}

/// `Driver::start_with_threads`'s hook runs once on each driver thread, on the thread it
/// names, before that thread's loop (card 136): the bench pins the conductor from it. The
/// conductor's hook is held until the test releases it, and while it is held the driver
/// answers no command, which is what "before its loop" means; the counts are read once
/// `close` has joined both threads, which orders both hooks before the reads.
#[test]
fn the_thread_start_hook_runs_once_on_each_named_driver_thread_before_its_loop() {
    let dir = TestDir::new("thread-start");
    const CONDUCTOR: usize = 0;
    const RECORDER: usize = 1;
    const REPLAYER: usize = 2;
    // Per thread: how many times the hook ran, and how many of those on the thread of
    // the expected name. Atomics, not a mutex: the lock ban covers the test target.
    let runs = Arc::new([
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    ]);
    let named = Arc::new([
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    ]);
    let release = Arc::new(AtomicBool::new(false));
    let hook: ThreadStart = {
        let runs = Arc::clone(&runs);
        let named = Arc::clone(&named);
        let release = Arc::clone(&release);
        Arc::new(move |thread| {
            let (index, expected) = match thread {
                DriverThread::Conductor => (CONDUCTOR, "cavalry-conductor"),
                DriverThread::Recorder => (RECORDER, "cavalry-recorder"),
                DriverThread::Replayer => (REPLAYER, "cavalry-replayer"),
            };
            runs[index].fetch_add(1, Ordering::AcqRel);
            if std::thread::current().name() == Some(expected) {
                named[index].fetch_add(1, Ordering::AcqRel);
            }
            if thread == DriverThread::Conductor {
                while !release.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
            }
        })
    };
    let driver = Driver::start_with_threads(
        dir.path(),
        dir.path().join("log"),
        IdleStrategy::Yield,
        hook,
    )
    .expect("starts");

    // The conductor is in its hook, held there: a command goes unanswered.
    let deadline = Instant::now() + Duration::from_secs(5);
    while runs[CONDUCTOR].load(Ordering::Acquire) == 0 {
        assert!(
            Instant::now() < deadline,
            "the conductor's hook did not run"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut client = Client::attach(dir.path());
    let correlation_id = client.correlation_id();
    client.send(&Command::AddSubscription {
        client_id: client.id,
        correlation_id,
        stream_id: 1,
        channel: IPC_CHANNEL,
    });
    let held_until = Instant::now() + Duration::from_millis(200);
    while Instant::now() < held_until {
        let answered = client
            .receiver
            .receive(|_, _| {})
            .expect("the receiver reads");
        assert!(!answered, "the conductor answered from inside its hook");
        std::thread::sleep(Duration::from_millis(5));
    }

    // Released, the loop begins and the command is answered.
    release.store(true, Ordering::Release);
    client.wait(Duration::from_secs(5), |event| match *event {
        Event::SubscriptionReady { correlation_id: id } if id == correlation_id => Some(()),
        _ => None,
    });
    driver.close().expect("closes");
    for (index, thread) in [
        (CONDUCTOR, "conductor"),
        (RECORDER, "recorder"),
        (REPLAYER, "replayer"),
    ] {
        assert_eq!(
            runs[index].load(Ordering::Acquire),
            1,
            "the {thread}'s hook ran once"
        );
        assert_eq!(
            named[index].load(Ordering::Acquire),
            1,
            "the {thread}'s hook ran on the thread named cavalry-{thread}"
        );
    }
}

#[test]
fn a_publication_gets_a_log_and_a_subscription_gets_an_image_at_the_producer_position() {
    let dir = TestDir::new("pubsub");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut client = Client::attach(dir.path());

    let (correlation_id, registration_id, session_id, limit_id, log_file_name) =
        client.add_publication(1001, false);
    assert_eq!(
        registration_id, correlation_id,
        "the first registration names the publication"
    );
    assert_eq!(
        log_file_name,
        format!("publications/{registration_id}.logbuffer")
    );
    assert!(dir.path().join(&log_file_name).is_file(), "the log exists");
    let reader = CountersReader::open(dir.path()).expect("opens");
    assert_eq!(
        reader.read_acquire(limit_id as u32).expect("limit"),
        0,
        "no subscriber: not connected"
    );

    // A second concurrent registration on the same stream joins the publication.
    let (second_correlation, second_registration, second_session, second_limit, second_log) =
        client.add_publication(1001, false);
    assert_ne!(second_correlation, registration_id);
    assert_eq!(
        second_registration, registration_id,
        "a join carries the first registration's id"
    );
    assert_eq!(second_session, session_id);
    assert_eq!(second_limit, limit_id);
    assert_eq!(second_log, log_file_name);

    // An exclusive publication is a new session every time.
    let (_, exclusive_registration, exclusive_session, _, _) = client.add_publication(1001, true);
    assert_ne!(exclusive_registration, registration_id);
    assert_ne!(exclusive_session, session_id);

    // A subscription gets an image per matching publication, at the producer's position.
    let subscription_id = client.add_subscription(1001);
    let mut images = Vec::new();
    for _ in 0..2 {
        let image = client.wait(Duration::from_secs(5), |event| match *event {
            Event::AvailableImage {
                correlation_id,
                session_id,
                subscriber_registration_id,
                subscriber_position_counter_id,
                log_file_name,
                source_identity,
                ..
            } if subscriber_registration_id == subscription_id => Some((
                correlation_id,
                session_id,
                subscriber_position_counter_id,
                String::from_utf8_lossy(log_file_name).into_owned(),
                source_identity.to_vec(),
            )),
            _ => None,
        });
        images.push(image);
    }
    images.sort();
    let concurrent = images
        .iter()
        .find(|image| image.0 == registration_id)
        .expect("the concurrent publication's image");
    assert_eq!(concurrent.1, session_id);
    assert_eq!(concurrent.3, log_file_name);
    assert_eq!(concurrent.4, IPC_CHANNEL);
    assert!(images.iter().any(|image| image.0 == exclusive_registration));
    assert_eq!(
        reader.read_acquire(concurrent.2 as u32).expect("position"),
        0,
        "joined at the producer position"
    );

    // With a subscriber the limit rises to the window.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let limit = reader.read_acquire(limit_id as u32).expect("limit");
        if limit == i64::from(spec::IPC_PUBLICATION_WINDOW) {
            break;
        }
        assert!(Instant::now() < deadline, "the limit did not rise: {limit}");
        std::thread::sleep(Duration::from_millis(1));
    }

    // Remove: the client's own registration, by its correlation id.
    let remove = client.correlation_id();
    client.send(&Command::RemovePublication {
        client_id: client.id,
        correlation_id: remove,
        registration_id: second_correlation,
    });
    client.wait(Duration::from_secs(5), |event| match *event {
        Event::OperationSuccess { correlation_id } if correlation_id == remove => Some(()),
        _ => None,
    });
    let unknown = client.correlation_id();
    client.send(&Command::RemovePublication {
        client_id: client.id,
        correlation_id: unknown,
        registration_id: second_correlation,
    });
    let (code, message) = client.wait_error(unknown);
    assert_eq!(code, 3, "removed twice: unknown publication; {message}");
    assert!(
        message.ends_with("docs/reference/client-protocol.md#error-codes"),
        "{message}"
    );

    let remove_sub = client.correlation_id();
    client.send(&Command::RemoveSubscription {
        client_id: client.id,
        correlation_id: remove_sub,
        registration_id: subscription_id,
    });
    client.wait(Duration::from_secs(5), |event| match *event {
        Event::OperationSuccess { correlation_id } if correlation_id == remove_sub => Some(()),
        _ => None,
    });
    driver.close().expect("closes");
}

#[test]
fn refused_commands_get_the_spec_error_codes_and_count() {
    let dir = TestDir::new("errors");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut client = Client::attach(dir.path());

    // An unknown type: code 6.
    let unknown_type = client.correlation_id();
    let mut message = Vec::new();
    message.extend_from_slice(&client.id.to_le_bytes());
    message.extend_from_slice(&unknown_type.to_le_bytes());
    client.send_raw(7, &message);
    assert_eq!(client.wait_error(unknown_type).0, 6);

    // A channel that is not cavalry:ipc: code 1.
    let bad_channel = client.correlation_id();
    client.send(&Command::AddPublication {
        client_id: client.id,
        correlation_id: bad_channel,
        stream_id: 7,
        channel: b"cavalry:udp?endpoint=localhost:40123",
    });
    assert_eq!(client.wait_error(bad_channel).0, 1);

    // A message longer than its fields: code 7.
    let too_long = client.correlation_id();
    let mut message = Vec::new();
    message.extend_from_slice(&client.id.to_le_bytes());
    message.extend_from_slice(&too_long.to_le_bytes());
    message.extend_from_slice(&[0, 0, 0, 0]);
    client.send_raw(CommandType::ClientKeepalive as i32, &message);
    assert_eq!(client.wait_error(too_long).0, 7);

    // Longer than any command of this version, a 5,000-byte record with a readable
    // correlation id, is checked in the spec's order on the conductor's copy of its
    // first bytes, without the conductor copying past its scratch. A legal head and
    // fields with bytes past them: `length`, code 7, naming the record's own length.
    let client_id = client.id;
    let over_long = move |correlation_id: i64| {
        let mut message = vec![0u8; 5000];
        message[..8].copy_from_slice(&client_id.to_le_bytes());
        message[8..16].copy_from_slice(&correlation_id.to_le_bytes());
        message
    };
    let length = client.correlation_id();
    client.send_raw(CommandType::AddPublication as i32, &over_long(length));
    let (code, text) = client.wait_error(length);
    assert_eq!(code, 7);
    assert!(text.contains("(length): 5000 bytes"), "{text}");

    // A reserved type id is `type`, code 6, however long the record.
    let reserved = client.correlation_id();
    client.send_raw(7, &over_long(reserved));
    let (code, text) = client.wait_error(reserved);
    assert_eq!(code, 6);
    assert!(text.contains("(type)"), "{text}");

    // A channel length above the bound is `string`, code 7, however long the record.
    let string = client.correlation_id();
    let mut message = over_long(string);
    message[20..24].copy_from_slice(&4097i32.to_le_bytes());
    client.send_raw(CommandType::AddPublication as i32, &message);
    let (code, text) = client.wait_error(string);
    assert_eq!(code, 7);
    assert!(text.contains("(string): a string of length 4097"), "{text}");

    // Too short to carry a correlation id: no answer, only the counter.
    client.send_raw(
        CommandType::ClientKeepalive as i32,
        &client.id.to_le_bytes(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while counter(dir.path(), CLIENT_PROTOCOL_REJECTED_COMMAND) < 7 {
        assert!(
            Instant::now() < deadline,
            "rejected_command did not reach 7"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(counter(dir.path(), CLIENT_PROTOCOL_REJECTED_COMMAND), 7);
    assert_eq!(counter(dir.path(), CLIENT_PROTOCOL_ERRORS), 6);
    driver.close().expect("closes");
}

#[test]
fn the_publication_registry_is_bounded_and_says_so() {
    let dir = TestDir::new("capacity");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut client = Client::attach(dir.path());
    let (_, registration_id, _, _, _) = client.add_publication(42, false);
    for _ in 1..spec::MAX_PUBLICATIONS {
        let (_, joined, _, _, _) = client.add_publication(42, false);
        assert_eq!(joined, registration_id);
    }
    let one_too_many = client.correlation_id();
    client.send(&Command::AddPublication {
        client_id: client.id,
        correlation_id: one_too_many,
        stream_id: 42,
        channel: IPC_CHANNEL,
    });
    let (code, message) = client.wait_error(one_too_many);
    assert_eq!(code, 10, "{message}");
    assert!(message.contains("publications registry"), "{message}");
    assert_eq!(counter(dir.path(), DRIVER_REGISTRY_FULL), 1);
    driver.close().expect("closes");
}

#[test]
fn a_removed_publication_lingers_then_its_images_go_and_its_log_is_removed() {
    let dir = TestDir::new("linger");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut client = Client::attach(dir.path());
    let (correlation_id, registration_id, _, limit_id, log_file_name) =
        client.add_publication(5, false);
    let subscription_id = client.add_subscription(5);
    client.wait(Duration::from_secs(5), |event| match *event {
        Event::AvailableImage {
            subscriber_registration_id,
            ..
        } if subscriber_registration_id == subscription_id => Some(()),
        _ => None,
    });
    // Publish one message, so that the subscriber, which never moves, has not reached
    // the end of stream: the linger decides, not the subscriber's position.
    let reader = CountersReader::open(dir.path()).expect("opens");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut limit = reader.read_acquire(limit_id as u32).expect("limit");
    while limit < 64 {
        assert!(Instant::now() < deadline, "the limit did not rise: {limit}");
        std::thread::sleep(Duration::from_millis(1));
        limit = reader.read_acquire(limit_id as u32).expect("limit");
    }
    let log = LogBuffers::open(dir.path(), registration_id, true).expect("the log opens");
    let mut appender = log.appender().expect("an appender");
    let after = appender
        .offer(b"one message", limit, 0)
        .expect("the message is offered");
    assert!(after > 0, "the producer moved");
    let removed_at = Instant::now();
    let remove = client.correlation_id();
    client.send(&Command::RemovePublication {
        client_id: client.id,
        correlation_id: remove,
        registration_id: correlation_id,
    });
    // The subscriber never moves, so the linger decides: unavailable image after 5 s.
    let (stream_id, channel) = client.wait(
        spec::PUBLICATION_LINGER + Duration::from_secs(5),
        |event| match *event {
            Event::UnavailableImage {
                correlation_id,
                subscriber_registration_id,
                stream_id,
                channel,
            } if correlation_id == registration_id
                && subscriber_registration_id == subscription_id =>
            {
                Some((stream_id, channel.to_vec()))
            }
            _ => None,
        },
    );
    assert_eq!(stream_id, 5);
    assert_eq!(channel, IPC_CHANNEL);
    assert!(
        removed_at.elapsed() >= spec::PUBLICATION_LINGER,
        "the linger passed first"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while dir.path().join(&log_file_name).exists() {
        assert!(Instant::now() < deadline, "the log was not removed");
        std::thread::sleep(Duration::from_millis(1));
    }
    driver.close().expect("closes");
}

#[test]
fn a_silent_client_is_timed_out_and_told() {
    let dir = TestDir::new("timeout");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut client = Client::attach(dir.path());
    let (_, registration_id, _, _, log_file_name) = client.add_publication(9, false);
    assert!(dir.path().join(&log_file_name).is_file());
    // Nothing for longer than the client liveness timeout.
    let client_id = client.id;
    let timed_out = client.wait(
        spec::CLIENT_LIVENESS_TIMEOUT + Duration::from_secs(5),
        |event| match *event {
            Event::ClientTimeout { client_id: id } if id == client_id => Some(id),
            _ => None,
        },
    );
    assert_eq!(timed_out, client_id);
    assert_eq!(counter(dir.path(), CLIENT_TIMED_OUT), 1);
    // Its publication closed with it: the log goes once the linger has passed.
    let deadline = Instant::now() + spec::PUBLICATION_LINGER + Duration::from_secs(5);
    while dir
        .path()
        .join(format!("publications/{registration_id}.logbuffer"))
        .exists()
    {
        assert!(Instant::now() < deadline, "the log was not removed");
        std::thread::sleep(Duration::from_millis(5));
    }
    driver.close().expect("closes");
}

/// Writes a raw value into a counter's slot through the file, as a misbehaving client
/// would: the driver's mapping sees it.
fn poison_counter(dir: &Path, counter_id: i32, value: i64) {
    use std::os::unix::fs::FileExt;
    let header = CountersReader::open(dir).expect("opens").header();
    let offset = header
        .slot_offset(u32::try_from(counter_id).expect("a counter id"))
        .expect("a slot");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join("counters"))
        .expect("the counters file opens for writing");
    file.write_at(&value.to_le_bytes(), offset as u64)
        .expect("the slot is written");
}

fn wait_image(client: &mut Client, subscription_id: i64) -> (i64, i32) {
    client.wait(Duration::from_secs(5), |event| match *event {
        Event::AvailableImage {
            correlation_id,
            subscriber_registration_id,
            subscriber_position_counter_id,
            ..
        } if subscriber_registration_id == subscription_id => {
            Some((correlation_id, subscriber_position_counter_id))
        }
        _ => None,
    })
}

fn wait_counter(dir: &Path, id: cavalry_shm::counters::CounterId, at_least: i64) -> i64 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let value = counter(dir, id);
        if value >= at_least {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "{} did not reach {at_least}: {value}",
            id.record()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_second_subscriber_never_cleans_under_a_slower_one() {
    let dir = TestDir::new("two-subscribers");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut client = Client::attach(dir.path());
    let (_, publication_id, _, limit_id, _) = client.add_publication(31, false);
    let subscription_a = client.add_subscription(31);
    let (_, position_a) = wait_image(&mut client, subscription_a);
    let reader = CountersReader::open(dir.path()).expect("opens");
    assert_eq!(reader.read_acquire(position_a as u32).expect("a"), 0);

    // The publisher writes one message that A has not read.
    let window = i64::from(spec::IPC_PUBLICATION_WINDOW);
    let deadline = Instant::now() + Duration::from_secs(5);
    while reader.read_acquire(limit_id as u32).expect("limit") < window {
        assert!(Instant::now() < deadline, "the limit did not rise");
        std::thread::sleep(Duration::from_millis(1));
    }
    let log = LogBuffers::open(dir.path(), publication_id, true).expect("the log opens");
    let mut appender = log.appender().expect("an appender");
    let after = appender
        .offer(b"for A", window, 0)
        .expect("the message is offered");
    assert!(after > 0);

    // B joins at the producer's position; A's unread message stays, and the limit stays
    // A's position + window.
    let subscription_b = client.add_subscription(31);
    let (_, position_b) = wait_image(&mut client, subscription_b);
    assert_eq!(
        reader.read_acquire(position_b as u32).expect("b"),
        after,
        "B joined at the producer"
    );
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        reader.read_acquire(limit_id as u32).expect("limit"),
        window,
        "the limit follows A"
    );
    let readable = LogBuffers::open(dir.path(), publication_id, false).expect("opens read-only");
    let mut term_reader = readable.reader(0).expect("a reader at A's position");
    let mut messages = Vec::new();
    term_reader
        .poll(16, |body, _| messages.push(body.to_vec()))
        .expect("A polls");
    assert_eq!(
        messages,
        vec![b"for A".to_vec()],
        "A still reads its message"
    );
    driver.close().expect("closes");
}

/// The record id of one publication's `publication.position` gauge: the driver allocates
/// it with the key `spec/counters-file.md` gives, so a reader finds it by type and key.
fn find_position_gauge(dir: &Path, correlation_id: i64, session_id: i32, stream_id: i32) -> u32 {
    let reader = CountersReader::open(dir).expect("the counters file opens");
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&correlation_id.to_le_bytes());
    key[8..12].copy_from_slice(&session_id.to_le_bytes());
    key[12..].copy_from_slice(&stream_id.to_le_bytes());
    (0..reader.capacity())
        .find(|&id| {
            reader.record(id).ok().flatten().is_some_and(|record| {
                record.type_id() == PUBLICATION_POSITION.0 && record.key()[..16] == key[..]
            })
        })
        .expect("the publication.position gauge is allocated")
}

#[test]
fn the_publication_position_gauge_follows_the_publisher() {
    let dir = TestDir::new("position-gauge");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut client = Client::attach(dir.path());
    let (correlation_id, publication_id, session_id, limit_id, _) =
        client.add_publication(51, false);
    let gauge = find_position_gauge(dir.path(), correlation_id, session_id, 51);
    let reader = CountersReader::open(dir.path()).expect("opens");
    assert_eq!(
        reader.read_acquire(gauge).expect("gauge"),
        0,
        "nothing published"
    );

    // A subscriber, so that the limit rises and the publisher may write.
    let subscription = client.add_subscription(51);
    let _ = wait_image(&mut client, subscription);
    let window = i64::from(spec::IPC_PUBLICATION_WINDOW);
    let deadline = Instant::now() + Duration::from_secs(5);
    while reader.read_acquire(limit_id as u32).expect("limit") < window {
        assert!(Instant::now() < deadline, "the limit did not rise");
        std::thread::sleep(Duration::from_millis(1));
    }
    let log = LogBuffers::open(dir.path(), publication_id, true).expect("the log opens");
    let mut appender = log.appender().expect("an appender");
    let after = appender
        .offer(b"moved", window, 0)
        .expect("the message is offered");

    // The driver loads the producer position at most once every two milliseconds and
    // stores the gauge from that load (ADR 0022, ADR 0032), so the gauge reaches the
    // position well inside the deadline; the test asserts the value, not the interval.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let value = reader.read_acquire(gauge).expect("gauge");
        if value == after {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "publication.position stayed at {value}, not {after}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    driver.close().expect("closes");
}

#[test]
fn a_subscriber_position_outside_its_bound_evicts_the_client() {
    let dir = TestDir::new("evict");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut client = Client::attach(dir.path());
    let (_, _, _, _, log_file_name) = client.add_publication(33, false);
    let subscription_id = client.add_subscription(33);
    let (_, position_id) = wait_image(&mut client, subscription_id);

    // A misaligned position: not a multiple of 32.
    poison_counter(dir.path(), position_id, 8);
    let client_id = client.id;
    let told = client.wait(Duration::from_secs(5), |event| match *event {
        Event::ClientTimeout { client_id: id } if id == client_id => Some(id),
        _ => None,
    });
    assert_eq!(told, client_id);
    assert_eq!(
        wait_counter(
            dir.path(),
            cavalry_shm::counters::generated::LOG_BUFFER_REJECTED_POSITION,
            1
        ),
        1
    );
    assert_eq!(
        counter(dir.path(), CLIENT_TIMED_OUT),
        0,
        "an eviction is not a timeout"
    );

    // The client's registrations are gone: removing them is unknown, and its publication
    // closed, so a new subscription of the stream gets no image from it.
    let remove = client.correlation_id();
    client.send(&Command::RemoveSubscription {
        client_id: client.id,
        correlation_id: remove,
        registration_id: subscription_id,
    });
    assert_eq!(client.wait_error(remove).0, 2);
    let deadline = Instant::now() + spec::PUBLICATION_LINGER + Duration::from_secs(5);
    while dir.path().join(&log_file_name).exists() {
        assert!(
            Instant::now() < deadline,
            "the evicted client's log was not removed"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    driver.close().expect("closes");
}

#[test]
fn the_client_registry_is_bounded_and_says_so() {
    let dir = TestDir::new("clients");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut client = Client::attach(dir.path());
    // The attached client is one, once it has sent a command; crafted client ids fill
    // the rest.
    client.add_subscription(1);
    for n in 1..spec::MAX_CLIENTS {
        let correlation_id = client.correlation_id();
        client.send(&Command::AddSubscription {
            client_id: 1_000_000 + n as i64,
            correlation_id,
            stream_id: 1,
            channel: IPC_CHANNEL,
        });
        client.wait(Duration::from_secs(5), |event| match *event {
            Event::SubscriptionReady { correlation_id: id } if id == correlation_id => Some(()),
            _ => None,
        });
    }
    let one_too_many = client.correlation_id();
    client.send(&Command::AddSubscription {
        client_id: 2_000_000,
        correlation_id: one_too_many,
        stream_id: 1,
        channel: IPC_CHANNEL,
    });
    let (code, message) = client.wait_error(one_too_many);
    assert_eq!(code, 10, "{message}");
    assert!(message.contains("client registry"), "{message}");
    assert_eq!(wait_counter(dir.path(), DRIVER_REGISTRY_FULL, 1), 1);
    // Not registered: its keepalive would have created it; a known client still works.
    let known = client.add_subscription(2);
    assert!(known > 0);
    driver.close().expect("closes");
}

#[test]
fn the_subscription_registry_is_bounded_and_says_so() {
    let dir = TestDir::new("subscriptions");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut client = Client::attach(dir.path());
    for _ in 0..spec::MAX_SUBSCRIPTIONS {
        client.add_subscription(3);
    }
    let one_too_many = client.correlation_id();
    client.send(&Command::AddSubscription {
        client_id: client.id,
        correlation_id: one_too_many,
        stream_id: 3,
        channel: IPC_CHANNEL,
    });
    let (code, message) = client.wait_error(one_too_many);
    assert_eq!(code, 10, "{message}");
    assert!(message.contains("subscriptions registry"), "{message}");
    assert_eq!(wait_counter(dir.path(), DRIVER_REGISTRY_FULL, 1), 1);
    driver.close().expect("closes");
}

#[test]
fn the_image_registry_is_bounded_and_says_so() {
    let dir = TestDir::new("images");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut client = Client::attach(dir.path());
    // 64 exclusive publications times 32 subscriptions: 2048 images, the capacity.
    let publications = 64;
    let subscriptions = spec::MAX_IMAGES / publications;
    for _ in 0..publications {
        client.add_publication(77, true);
    }
    for _ in 0..subscriptions {
        let subscription_id = client.add_subscription(77);
        for _ in 0..publications {
            wait_image(&mut client, subscription_id);
        }
    }
    // The next subscription is registered, but its first image does not fit.
    let one_more = client.add_subscription(77);
    let (code, message) = client.wait_error(one_more);
    assert_eq!(code, 10, "{message}");
    assert!(message.contains("images registry"), "{message}");
    assert!(wait_counter(dir.path(), DRIVER_REGISTRY_FULL, 1) >= 1);
    driver.close().expect("closes");
}

// ---- recording ---------------------------------------------------------------------

/// `start recording` on `stream_id`, answered with `recording ready`: (recording id,
/// session id, recording.position counter id, recording.synced_position counter id).
fn start_recording(client: &mut Client, stream_id: i32) -> (i64, i32, i32, i32) {
    let correlation_id = client.correlation_id();
    client.send(&Command::StartRecording {
        client_id: client.id,
        correlation_id,
        stream_id,
        channel: IPC_CHANNEL,
    });
    client.wait(Duration::from_secs(5), |event| match *event {
        Event::RecordingReady {
            correlation_id: id,
            recording_id,
            session_id,
            recording_position_counter_id,
            recording_synced_position_counter_id,
        } if id == correlation_id => Some((
            recording_id,
            session_id,
            recording_position_counter_id,
            recording_synced_position_counter_id,
        )),
        _ => None,
    })
}

/// `stop recording`, answered with `operation success`.
fn stop_recording(client: &mut Client, recording_id: i64) {
    let correlation_id = client.correlation_id();
    client.send(&Command::StopRecording {
        client_id: client.id,
        correlation_id,
        recording_id,
    });
    client.wait(Duration::from_secs(5), |event| match *event {
        Event::OperationSuccess { correlation_id: id } if id == correlation_id => Some(()),
        _ => None,
    });
}

/// The `recording stopped` event for `recording_id`: its stop position.
fn wait_stopped(client: &mut Client, recording_id: i64) -> i64 {
    client.wait(Duration::from_secs(10), |event| match *event {
        Event::RecordingStopped {
            recording_id: id,
            stop_position,
        } if id == recording_id => Some(stop_position),
        _ => None,
    })
}

/// Waits up to five seconds for the counter at `id` to hold `expected`.
fn wait_value(dir: &Path, id: i32, expected: i64) {
    let reader = CountersReader::open(dir).expect("the counters file opens");
    let id = u32::try_from(id).expect("a counter id");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let value = reader.read_acquire(id).expect("the counter reads");
        if value == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "counter {id} stayed at {value}, not {expected}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// `n` messages of 32 bytes through the publication's own log; returns the position after.
fn publish(dir: &Path, publication_id: i64, limit_id: i32, n: u8) -> i64 {
    let reader = CountersReader::open(dir).expect("the counters file opens");
    let limit = u32::try_from(limit_id).expect("a counter id");
    let deadline = Instant::now() + Duration::from_secs(5);
    while reader.read_acquire(limit).expect("limit") < 64 * i64::from(n) {
        assert!(Instant::now() < deadline, "the limit did not rise");
        std::thread::sleep(Duration::from_millis(1));
    }
    let log = LogBuffers::open(dir, publication_id, true).expect("the log opens");
    let mut appender = log.appender().expect("an appender");
    let mut after = 0;
    for i in 0..n {
        after = appender
            .offer(&[i; 32], reader.read_acquire(limit).expect("limit"), 0)
            .expect("the message is offered");
    }
    after
}

/// The full case of the commands (invariant 1): a recording starts with its two
/// counters, whose labels name it; the publication is connected by the recording alone;
/// recording.position follows the publisher and recording.synced_position follows within
/// the sync cadence; `stop recording` is answered once the catalog holds the stop, every
/// client hears `recording stopped`, and the segment file holds the log's bytes.
#[test]
fn a_recording_starts_follows_the_publisher_and_stops_on_command() {
    let dir = TestDir::new("recording");
    let log_dir = dir.path().join("log");
    let driver = Driver::start(dir.path(), &log_dir).expect("starts");
    let mut client = Client::attach(dir.path());
    let (_, publication_id, session_id, limit_id, _) = client.add_publication(61, true);
    let (recording_id, session, position_id, synced_id) = start_recording(&mut client, 61);
    assert_eq!(recording_id, 0, "the first slot");
    assert_eq!(session, session_id, "the event names the session recorded");
    let reader = CountersReader::open(dir.path()).expect("opens");
    for (id, name) in [
        (position_id, "recording.position"),
        (synced_id, "recording.synced_position"),
    ] {
        let record = reader
            .record(u32::try_from(id).expect("a counter id"))
            .expect("the record reads")
            .expect("the counter is allocated");
        assert_eq!(record.label(), format!("{name} 0 {session_id} 61"));
        assert_eq!(&record.key()[..8], &0i64.to_le_bytes());
    }
    assert_eq!(wait_counter(dir.path(), RECORDING_STARTED, 1), 1);

    let after = publish(dir.path(), publication_id, limit_id, 100);
    wait_value(dir.path(), position_id, after);
    wait_value(dir.path(), synced_id, after);

    stop_recording(&mut client, recording_id);
    assert_eq!(wait_stopped(&mut client, recording_id), after);
    assert_eq!(wait_counter(dir.path(), RECORDING_STOPPED, 1), 1);
    let catalog = CatalogReader::open(&log_dir).expect("the catalog opens");
    let descriptor = catalog
        .get(0)
        .expect("reads")
        .expect("the slot holds the recording");
    assert_eq!(descriptor.state(), State::Valid);
    assert_eq!(descriptor.stop_position(), after);
    // The segment holds every frame as published: a 32-byte header and the body
    // `publish` wrote, in order, up to the stop.
    let segment = std::fs::read(log_dir.join("0-0.rec")).expect("the segment reads");
    assert_eq!(after, 100 * 64, "100 frames of 64 bytes");
    for i in 0..100usize {
        let frame = &segment[i * 64..(i + 1) * 64];
        assert_eq!(&frame[..4], &64i32.to_le_bytes(), "frame {i}: length");
        assert_eq!(&frame[32..], &[i as u8; 32], "frame {i}: body");
    }

    // Stopped: nothing written, operation success.
    stop_recording(&mut client, recording_id);
    driver.close().expect("closes");
}

/// A publication removed while recorded: the recorder reaches the end of stream, the
/// recording stops there, every client hears it, and only then the log file goes.
#[test]
fn a_closed_publications_recording_stops_at_the_end_of_stream_and_only_then_its_log_goes() {
    let dir = TestDir::new("recording-close");
    let log_dir = dir.path().join("log");
    let driver = Driver::start(dir.path(), &log_dir).expect("starts");
    let mut client = Client::attach(dir.path());
    let (registration, publication_id, _, limit_id, log_file_name) =
        client.add_publication(62, true);
    let (recording_id, _, _, _) = start_recording(&mut client, 62);
    let after = publish(dir.path(), publication_id, limit_id, 10);
    let log_path = dir.path().join(&log_file_name);
    assert!(log_path.is_file());

    let correlation_id = client.correlation_id();
    client.send(&Command::RemovePublication {
        client_id: client.id,
        correlation_id,
        registration_id: registration,
    });
    client.wait(Duration::from_secs(5), |event| match *event {
        Event::OperationSuccess { correlation_id: id } if id == correlation_id => Some(()),
        _ => None,
    });
    assert_eq!(wait_stopped(&mut client, recording_id), after);
    let deadline = Instant::now() + Duration::from_secs(5);
    while log_path.exists() {
        assert!(
            Instant::now() < deadline,
            "the log stayed after the recording stopped"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let catalog = CatalogReader::open(&log_dir).expect("the catalog opens");
    assert_eq!(
        catalog
            .get(0)
            .expect("reads")
            .expect("the slot")
            .stop_position(),
        after
    );
    driver.close().expect("closes");
}

/// Every refusal of the recording commands, with the spec's code and a message that
/// says what to do (`spec/client-protocol.md`, "Error codes").
#[test]
fn recording_commands_get_the_spec_error_codes() {
    let dir = TestDir::new("recording-errors");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut client = Client::attach(dir.path());

    let other_channel = client.correlation_id();
    client.send(&Command::StartRecording {
        client_id: client.id,
        correlation_id: other_channel,
        stream_id: 63,
        channel: b"cavalry:udp?endpoint=localhost:40123",
    });
    let (code, message) = client.wait_error(other_channel);
    assert_eq!(code, 1, "{message}");

    let no_publication = client.correlation_id();
    client.send(&Command::StartRecording {
        client_id: client.id,
        correlation_id: no_publication,
        stream_id: 63,
        channel: IPC_CHANNEL,
    });
    let (code, message) = client.wait_error(no_publication);
    assert_eq!(code, 20, "{message}");
    assert!(
        message.contains("no live publication on stream 63"),
        "{message}"
    );

    client.add_publication(63, true);
    let (recording_id, _, _, _) = start_recording(&mut client, 63);
    let again = client.correlation_id();
    client.send(&Command::StartRecording {
        client_id: client.id,
        correlation_id: again,
        stream_id: 63,
        channel: IPC_CHANNEL,
    });
    let (code, message) = client.wait_error(again);
    assert_eq!(code, 22, "{message}");
    assert!(
        message.contains(&format!("recording {recording_id}")),
        "{message}"
    );

    for unknown in [-1, recording_id + 1, i64::MAX] {
        let correlation_id = client.correlation_id();
        client.send(&Command::StopRecording {
            client_id: client.id,
            correlation_id,
            recording_id: unknown,
        });
        let (code, message) = client.wait_error(correlation_id);
        assert_eq!(code, 21, "{message}");
        assert!(message.contains("not in the catalog"), "{message}");
    }

    // A reserved type id in Cavalry's range is code 6, as any reserved id: 0x0108, the
    // first after version 3's retention commands (ADR 0031).
    let reserved = client.correlation_id();
    let mut message = [0u8; 16];
    message[..8].copy_from_slice(&client.id.to_le_bytes());
    message[8..].copy_from_slice(&reserved.to_le_bytes());
    client.send_raw(0x0108, &message);
    let (code, text) = client.wait_error(reserved);
    assert_eq!(code, 6, "{text}");

    stop_recording(&mut client, recording_id);
    assert_eq!(wait_stopped(&mut client, recording_id), 0);
    driver.close().expect("closes");
}

/// Closing the driver stops every live recording at what was written, so the catalog
/// holds a clean stop and the next driver on the directory repairs nothing.
#[test]
fn closing_the_driver_stops_its_recordings_and_the_next_open_repairs_nothing() {
    let dir = TestDir::new("recording-driver-close");
    let log_dir = dir.path().join("log");
    let driver = Driver::start(dir.path(), &log_dir).expect("starts");
    let mut client = Client::attach(dir.path());
    let (_, publication_id, _, limit_id, _) = client.add_publication(64, true);
    let (recording_id, _, position_id, _) = start_recording(&mut client, 64);
    let after = publish(dir.path(), publication_id, limit_id, 20);
    wait_value(dir.path(), position_id, after);
    driver.close().expect("closes");
    let catalog = CatalogReader::open(&log_dir).expect("the catalog opens");
    let descriptor = catalog.get(recording_id).expect("reads").expect("the slot");
    assert_eq!(
        descriptor.stop_position(),
        after,
        "stopped at what was written"
    );

    let again = Driver::start(dir.path(), &log_dir).expect("starts again");
    assert_eq!(
        counter(dir.path(), RECORDING_RECOVERED),
        0,
        "nothing to repair"
    );
    assert_eq!(counter(dir.path(), RECORDING_INDEX_REWRITTEN), 0);
    again.close().expect("closes");
}

/// The recovery pass counts what it did (invariant 4): a slot left live by a crash,
/// with no files, is stopped as an empty recording and its index written.
#[test]
fn the_recovery_pass_counts_what_it_did() {
    // The log directory beside the driver directory, not under it: a driver that finds
    // no live driver in its directory removes it before it opens the log.
    let root = TestDir::new("recording-recovery");
    let driver_dir = root.path().join("driver");
    let log_dir = root.path().join("log");
    let now = now_nanos();
    std::fs::create_dir_all(&log_dir).expect("the log directory is created");
    let mut catalog = CatalogWriter::open(&log_dir, now).expect("the catalog is created");
    catalog
        .add(&Recording {
            recording_id: 0,
            start_timestamp: now,
            start_position: 0,
            initial_term_id: 1,
            term_length: 64 * 1024 * 1024,
            mtu_length: 1408,
            session_id: 1,
            stream_id: 1,
            channel: "cavalry:ipc",
            source_identity: "cavalry:ipc",
        })
        .expect("a live slot, as a crash leaves it");
    drop(catalog);
    let driver = Driver::start(&driver_dir, &log_dir).expect("starts");
    assert_eq!(counter(&driver_dir, RECORDING_RECOVERED), 1);
    assert_eq!(counter(&driver_dir, RECORDING_INDEX_REWRITTEN), 1);
    let reader = CatalogReader::open(&log_dir).expect("the catalog opens");
    let descriptor = reader.get(0).expect("reads").expect("the slot");
    assert_eq!(descriptor.stop_position(), 0, "an empty recording");
    driver.close().expect("closes");
}

// ---- replay --------------------------------------------------------------------------

/// `start replay` of `recording_id` from `position` for `length` onto `stream_id`,
/// answered with `replay ready`: (replay session id, start position, session id).
fn start_replay(
    client: &mut Client,
    recording_id: i64,
    position: i64,
    length: i64,
    stream_id: i32,
) -> (i64, i64, i32) {
    let correlation_id = client.correlation_id();
    client.send(&Command::StartReplay {
        client_id: client.id,
        correlation_id,
        recording_id,
        position,
        length,
        replay_stream_id: stream_id,
        replay_channel: IPC_CHANNEL,
    });
    client.wait(Duration::from_secs(5), |event| match *event {
        Event::ReplayReady {
            correlation_id: id,
            replay_session_id,
            recording_id: recorded,
            start_position,
            session_id,
            stream_id: stream,
        } if id == correlation_id => {
            assert_eq!(
                replay_session_id, correlation_id,
                "the replay session id is the command's"
            );
            assert_eq!(recorded, recording_id);
            assert_eq!(stream, stream_id);
            Some((replay_session_id, start_position, session_id))
        }
        _ => None,
    })
}

/// The `replay stopped` event for `replay_session_id`: (position, reason).
fn wait_replay_stopped(client: &mut Client, replay_session_id: i64) -> (i64, i32) {
    client.wait(Duration::from_secs(10), |event| match *event {
        Event::ReplayStopped {
            replay_session_id: id,
            position,
            reason,
        } if id == replay_session_id => Some((position, reason)),
        _ => None,
    })
}

/// The full case through the threads (invariant 1): 100 messages recorded and
/// stopped, replayed from the start to the extent onto a stream a subscription joined
/// first; the subscription's image is the replay's publication at the start position,
/// its log holds every frame at the recording's position with the replay's ids, `replay
/// stopped` says reason 0 at the stop position, the counters moved, and once the
/// subscriber reaches the end the image goes and the log with it.
#[test]
fn a_replay_plays_a_stopped_recording_into_a_subscription_and_ends() {
    let dir = TestDir::new("replay");
    let log_dir = dir.path().join("log");
    let driver = Driver::start(dir.path(), &log_dir).expect("starts");
    let mut client = Client::attach(dir.path());
    let (_, publication_id, _, limit_id, _) = client.add_publication(71, true);
    let (recording_id, _, position_id, _) = start_recording(&mut client, 71);
    let after = publish(dir.path(), publication_id, limit_id, 100);
    wait_value(dir.path(), position_id, after);
    stop_recording(&mut client, recording_id);
    assert_eq!(wait_stopped(&mut client, recording_id), after);

    let subscription_id = client.add_subscription(2071);
    let (replay_session_id, start_position, session_id) =
        start_replay(&mut client, recording_id, NULL_POSITION, NULL_LENGTH, 2071);
    assert_eq!(start_position, 0);
    let (image_id, subscriber_counter) = wait_image(&mut client, subscription_id);
    assert_eq!(
        image_id, replay_session_id,
        "the image is the replay's publication"
    );
    assert_eq!(wait_counter(dir.path(), REPLAY_STARTED, 1), 1);
    let (position, reason) = wait_replay_stopped(&mut client, replay_session_id);
    assert_eq!((position, reason), (after, 0));
    assert_eq!(wait_counter(dir.path(), REPLAY_STOPPED, 1), 1);
    assert_eq!(wait_counter(dir.path(), REPLAY_ENDED, 1), 1);

    // The replay's log, read as a subscriber reads it: every frame at its position.
    let log = LogBuffers::open(dir.path(), replay_session_id, false).expect("the replay log opens");
    assert_eq!(log.header().session_id(), session_id);
    assert_eq!(log.header().stream_id(), 2071);
    assert_eq!(log.end_of_stream_position(), after);
    let mut reader = log.reader(0).expect("a reader");
    let mut frames = Vec::new();
    loop {
        let before = reader.position();
        let poll = reader
            .poll(64, |body, header| frames.push((body.to_vec(), *header)))
            .expect("the poll reads");
        if poll.position == before {
            break;
        }
    }
    assert_eq!(frames.len(), 100);
    for (i, (body, header)) in frames.iter().enumerate() {
        assert_eq!(body, &vec![i as u8; 32], "frame {i}: body");
        assert_eq!(header.term_offset(), i as i32 * 64, "frame {i}: position");
        assert_eq!(header.session_id(), session_id, "frame {i}: session");
        assert_eq!(header.stream_id(), 2071, "frame {i}: stream");
    }
    assert_eq!(reader.position(), after);
    drop(reader);
    drop(log);

    // The subscriber reaches the end: the image goes, then the log. The gauge the
    // driver keeps for the replay's publication is the one a publication has.
    let _ = find_position_gauge(dir.path(), replay_session_id, session_id, 2071);
    let reader = CountersReader::open(dir.path()).expect("the counters file opens");
    let record = reader
        .record(u32::try_from(subscriber_counter).expect("a counter id"))
        .expect("the record reads")
        .expect("the subscriber's counter is allocated");
    assert_eq!(
        record.label(),
        format!("subscription.position {replay_session_id} {session_id} 2071")
    );
    let writer =
        cavalry_shm::counters::CountersWriter::open(dir.path()).expect("the counters open");
    writer
        .counter(u32::try_from(subscriber_counter).expect("a counter id"))
        .expect("the counter")
        .store_release(after);
    client.wait(Duration::from_secs(5), |event| match *event {
        Event::UnavailableImage {
            correlation_id,
            subscriber_registration_id,
            ..
        } if correlation_id == replay_session_id
            && subscriber_registration_id == subscription_id =>
        {
            Some(())
        }
        _ => None,
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while dir
        .path()
        .join(format!("publications/{replay_session_id}.logbuffer"))
        .exists()
    {
        assert!(
            Instant::now() < deadline,
            "the replay's log was not removed"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    driver.close().expect("closes");
}

/// The replay commands' error codes through the threads: the channel (1), a length
/// below −1 (7), a recording not in the catalog (21), a position out of range (23), a
/// reserved id in Cavalry's range (6), and a stop of a replay nobody holds (operation
/// success).
#[test]
fn replay_commands_get_the_spec_error_codes() {
    let dir = TestDir::new("replay-errors");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("starts");
    let mut client = Client::attach(dir.path());
    let (_, publication_id, _, limit_id, _) = client.add_publication(72, true);
    let (recording_id, _, position_id, _) = start_recording(&mut client, 72);
    let after = publish(dir.path(), publication_id, limit_id, 4);
    wait_value(dir.path(), position_id, after);
    stop_recording(&mut client, recording_id);
    wait_stopped(&mut client, recording_id);
    let errors_before = counter(dir.path(), CLIENT_PROTOCOL_ERRORS);

    let send =
        |client: &mut Client, recording_id: i64, position: i64, length: i64, channel: &[u8]| {
            let correlation_id = client.correlation_id();
            client.send(&Command::StartReplay {
                client_id: client.id,
                correlation_id,
                recording_id,
                position,
                length,
                replay_stream_id: 2072,
                replay_channel: channel,
            });
            client.wait_error(correlation_id)
        };
    let (code, message) = send(
        &mut client,
        recording_id,
        NULL_POSITION,
        NULL_LENGTH,
        b"cavalry:udp?endpoint=x",
    );
    assert_eq!(code, 1, "{message}");
    let (code, message) = send(&mut client, recording_id, NULL_POSITION, -2, IPC_CHANNEL);
    assert_eq!(code, 7, "{message}");
    let (code, message) = send(&mut client, 9, NULL_POSITION, NULL_LENGTH, IPC_CHANNEL);
    assert_eq!(code, 21, "{message}");
    assert!(message.contains("recording 9"), "{message}");
    let (code, message) = send(
        &mut client,
        recording_id,
        after + 32,
        NULL_LENGTH,
        IPC_CHANNEL,
    );
    assert_eq!(code, 23, "{message}");
    assert!(message.contains(&format!("from 0 to {after}")), "{message}");
    assert_eq!(wait_counter(dir.path(), REPLAY_START_FAILED, 2), 2);
    assert!(
        message.ends_with("docs/reference/client-protocol.md#error-codes"),
        "{message}"
    );

    // A reserved id in Cavalry's range after this version's: code 6.
    let correlation_id = client.correlation_id();
    let mut raw = [0u8; 16];
    raw[..8].copy_from_slice(&client.id.to_le_bytes());
    raw[8..].copy_from_slice(&correlation_id.to_le_bytes());
    client.send_raw(0x0108, &raw);
    let (code, _) = client.wait_error(correlation_id);
    assert_eq!(code, 6);
    assert_eq!(
        CommandType::try_from(0x0104).ok(),
        Some(CommandType::StopReplay)
    );

    // A stop of a replay nobody holds: operation success, nothing done.
    let correlation_id = client.correlation_id();
    client.send(&Command::StopReplay {
        client_id: client.id,
        correlation_id,
        replay_session_id: 12345,
    });
    client.wait(Duration::from_secs(5), |event| match *event {
        Event::OperationSuccess { correlation_id: id } if id == correlation_id => Some(()),
        _ => None,
    });
    assert!(counter(dir.path(), CLIENT_PROTOCOL_ERRORS) >= errors_before + 5);
    driver.close().expect("closes");
}
