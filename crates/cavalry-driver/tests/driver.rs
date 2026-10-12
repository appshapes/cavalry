//! The driver on disk, driven by a client that attaches through cavalry-shm and speaks
//! cavalry-proto's client protocol: the full cases first, the directory protocol, every
//! command, a registry at capacity, the linger and the client timeout
//! (`spec/client-protocol.md`; CLAUDE.md, invariants 1, 3 and 4).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cavalry_driver::{Driver, IdleStrategy, spec};
use cavalry_proto::client_protocol::{Command, CommandType, Event, IPC_CHANNEL};
use cavalry_shm::broadcast::{BroadcastBuffer, Receiver};
use cavalry_shm::command_ring::{CommandRing, Producer};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    CLIENT_PROTOCOL_ERRORS, CLIENT_PROTOCOL_REJECTED_COMMAND, CLIENT_TIMED_OUT,
    DRIVER_REGISTRY_FULL,
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
    let driver = Driver::start(dir.path()).expect("the driver starts");
    for file in ["counters", "command-ring", "broadcast-buffer"] {
        assert!(dir.path().join(file).is_file(), "{file} exists");
    }
    let refused = Driver::start(dir.path()).expect_err("a second driver is refused");
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
    let again = Driver::start(dir.path()).expect("the next driver remakes the directory");
    let second_start = CommandRing::open(dir.path(), false)
        .expect("opens")
        .header()
        .start_timestamp();
    assert!(second_start > first_start, "the files were made anew");
    again.close().expect("closes");

    // A half-created directory, the ring missing, is a dead driver's too.
    std::fs::remove_file(dir.path().join("command-ring")).expect("remove the ring");
    let third =
        Driver::start_with(dir.path(), IdleStrategy::Yield).expect("starts on a half directory");
    assert!(dir.path().join("command-ring").is_file());
    third.close().expect("closes");
}

#[test]
fn a_publication_gets_a_log_and_a_subscription_gets_an_image_at_the_producer_position() {
    let dir = TestDir::new("pubsub");
    let driver = Driver::start(dir.path()).expect("starts");
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
    let driver = Driver::start(dir.path()).expect("starts");
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

    // Too short to carry a correlation id: no answer, only the counter.
    client.send_raw(
        CommandType::ClientKeepalive as i32,
        &client.id.to_le_bytes(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while counter(dir.path(), CLIENT_PROTOCOL_REJECTED_COMMAND) < 4 {
        assert!(
            Instant::now() < deadline,
            "rejected_command did not reach 4"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(counter(dir.path(), CLIENT_PROTOCOL_REJECTED_COMMAND), 4);
    assert_eq!(counter(dir.path(), CLIENT_PROTOCOL_ERRORS), 3);
    driver.close().expect("closes");
}

#[test]
fn the_publication_registry_is_bounded_and_says_so() {
    let dir = TestDir::new("capacity");
    let driver = Driver::start(dir.path()).expect("starts");
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
    let driver = Driver::start(dir.path()).expect("starts");
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
    let driver = Driver::start(dir.path()).expect("starts");
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
