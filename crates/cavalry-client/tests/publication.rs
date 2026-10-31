//! Publications against a real driver in this process: the full case first, back-pressure
//! at the window with a subscriber that never reads, then the shared and exclusive
//! sessions, the refusals with the spec's error codes, and closing
//! (`spec/log-buffer.md`, "Claim, commit and abort"; `spec/client-protocol.md`,
//! "Commands" and "Error codes"; ADR 0021; CLAUDE.md, invariants 1 and 3).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, ClientError, Offer};
use cavalry_driver::{Driver, spec};
use cavalry_proto::client_protocol::{Command, ErrorCode, Event, IPC_CHANNEL};
use cavalry_shm::broadcast::BroadcastBuffer;
use cavalry_shm::command_ring::CommandRing;

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("cavalry-client-pub-{}-{test}", std::process::id()));
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

fn now_nanos() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos()),
    )
    .unwrap_or(i64::MAX)
}

/// A subscriber by hand, through the protocol, until the next commit brings
/// `add_subscription`: it adds a subscription and never polls, so its image's position
/// stays where it joined and the publisher meets the window.
fn add_idle_subscription(dir: &Path, stream_id: i32) {
    let ring = CommandRing::open(dir, true).expect("the ring opens");
    let mut producer = ring.producer(spec::DRIVER_TIMEOUT).expect("a producer");
    let broadcast = BroadcastBuffer::open(dir, false).expect("the buffer opens");
    let mut receiver = broadcast.receiver().expect("a receiver");
    let client_id = producer.next_correlation_id();
    let correlation_id = producer.next_correlation_id();
    let command = Command::AddSubscription {
        client_id,
        correlation_id,
        stream_id,
        channel: IPC_CHANNEL,
    };
    let mut buffer = vec![0u8; 256];
    let length = command.write(&mut buffer).expect("encodes");
    producer
        .write(
            command.command_type() as i32,
            &buffer[..length],
            now_nanos(),
        )
        .expect("written");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut ready = false;
        receiver
            .receive(|type_id, message| {
                if let Ok(Event::SubscriptionReady {
                    correlation_id: id, ..
                }) = Event::parse(type_id, message)
                    && id == correlation_id
                {
                    ready = true;
                }
            })
            .expect("receives");
        if ready {
            return;
        }
        assert!(Instant::now() < deadline, "no subscription ready");
        std::thread::sleep(Duration::from_millis(1));
    }
    // The subscriber's keepalives are not needed: the test is shorter than the liveness
    // timeout, and an image whose client is timed out would still hold its position.
}

fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    done()
}

#[test]
fn a_publisher_is_back_pressured_at_the_window_and_nothing_is_lost() {
    let dir = TestDir::new("window");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let mut publication = client
        .add_publication("cavalry:ipc", 1001)
        .expect("a publication");

    // No subscriber yet: not connected, and the limit holds the publisher at 0.
    assert!(!publication.is_connected());
    assert_eq!(publication.try_claim(8).unwrap_err(), Offer::NotConnected);
    assert_eq!(publication.position().expect("position"), 0);

    add_idle_subscription(dir.path(), 1001);
    assert!(
        wait_until(Duration::from_secs(5), || publication.is_connected()),
        "the driver connects the publication on the subscriber's image"
    );
    assert!(
        wait_until(Duration::from_secs(5), || publication.limit()
            >= i64::from(spec::IPC_PUBLICATION_WINDOW)),
        "the limit is the subscriber's position plus the window"
    );

    // Fill the window: 64-byte frames (32-byte header, 32-byte body) until the window
    // is full. Every refusal before that is a rotation, never a lost message.
    let body = [7u8; 32];
    let mut written: u64 = 0;
    let mut rotations = 0;
    let back_pressured = loop {
        match publication.offer(&body) {
            Ok(position) => {
                written += 1;
                assert_eq!(position, i64::try_from(written * 64).expect("fits"));
            }
            Err(Offer::AdminAction) => rotations += 1,
            Err(Offer::BackPressured) => break true,
            Err(other) => panic!("{other}"),
        }
        assert!(written < 10_000_000, "never back-pressured");
    };
    assert!(back_pressured);
    assert_eq!(
        written * 64,
        u64::from(spec::IPC_PUBLICATION_WINDOW),
        "exactly the window was written, {rotations} rotations"
    );
    assert_eq!(
        publication.offer(&body).unwrap_err(),
        Offer::BackPressured,
        "and it stays back-pressured while the subscriber does not read"
    );
    assert_eq!(publication.try_claim(32).unwrap_err(), Offer::BackPressured);
    assert_eq!(
        publication.position().expect("position"),
        i64::from(spec::IPC_PUBLICATION_WINDOW)
    );

    publication.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

#[test]
fn claim_writes_in_place_and_too_long_is_refused_before_anything_is_reserved() {
    let dir = TestDir::new("claim");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let mut publication = client
        .add_exclusive_publication("cavalry:ipc", 1002)
        .expect("a publication");
    assert!(publication.is_exclusive());
    add_idle_subscription(dir.path(), 1002);
    assert!(wait_until(Duration::from_secs(5), || publication.is_connected()));

    let max_payload = publication.max_payload_length();
    let max_message = publication.max_message_length();
    assert_eq!(max_payload, spec::IPC_MTU_LENGTH as usize - 32);
    assert!(max_message > max_payload);
    assert_eq!(
        publication.try_claim(max_payload + 1).unwrap_err(),
        Offer::TooLong {
            found: max_payload + 1,
            limit: max_payload
        }
    );
    let too_long = vec![0u8; max_message + 1];
    assert_eq!(
        publication.offer(&too_long).unwrap_err(),
        Offer::TooLong {
            found: max_message + 1,
            limit: max_message
        }
    );
    assert_eq!(
        publication.position().expect("position"),
        0,
        "nothing reserved"
    );

    let mut claim = publication.try_claim(5).expect("claims");
    claim.body().copy_from_slice(b"hello");
    let after = claim.commit();
    assert_eq!(after, 64, "a 5-byte body is one 64-byte frame");
    assert_eq!(publication.position().expect("position"), 64);

    // An offer of a message larger than one payload fragments it.
    let big = vec![1u8; max_payload * 2 + 1];
    let position = publication.offer(&big).expect("offers");
    assert!(position > 64 + i64::try_from(big.len()).expect("fits"));

    publication.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

#[test]
fn shared_publications_share_a_session_and_exclusive_ones_do_not() {
    let dir = TestDir::new("sessions");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let first = client
        .add_publication("cavalry:ipc", 1003)
        .expect("a publication");
    let second = client
        .add_publication("cavalry:ipc", 1003)
        .expect("a publication");
    assert_eq!(first.session_id(), second.session_id(), "shared");
    assert_eq!(first.registration_id(), second.registration_id());
    assert_eq!(first.stream_id(), 1003);
    assert_eq!(first.channel(), "cavalry:ipc");
    assert!(!first.is_exclusive());
    let exclusive = client
        .add_exclusive_publication("cavalry:ipc", 1003)
        .expect("a publication");
    assert_ne!(
        exclusive.session_id(),
        first.session_id(),
        "a session of its own"
    );
    assert_ne!(exclusive.registration_id(), first.registration_id());
    assert!(format!("{first:?}").contains("Publication"));
    drop(first);
    drop(second);
    drop(exclusive);
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

#[test]
fn a_refused_command_carries_the_drivers_code_and_message() {
    let dir = TestDir::new("refused");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let error = client
        .add_publication("cavalry:udp?endpoint=localhost:40123", 1004)
        .expect_err("not this driver's transport yet");
    match &error {
        ClientError::Refused {
            code,
            raw_code,
            message,
        } => {
            assert_eq!(*code, Some(ErrorCode::GenericError));
            assert_eq!(*raw_code, 11);
            assert!(message.contains("cavalry:udp"), "{message}");
        }
        other => panic!("{other}"),
    }
    assert_eq!(error.reason(), "refused");
    assert!(error.to_string().contains("error code 1"), "{error}");
    // The client is still fine after a refusal.
    let publication = client
        .add_publication("cavalry:ipc", 1004)
        .expect("a publication");
    publication.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

#[test]
fn a_closed_publications_log_goes_and_a_closed_clients_publication_refuses() {
    let dir = TestDir::new("closed");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let closing = client
        .add_publication("cavalry:ipc", 1005)
        .expect("a publication");
    let log = dir.path().join(format!(
        "publications/{}.logbuffer",
        closing.registration_id()
    ));
    assert!(log.is_file());
    closing.close().expect("closes");
    // The driver removes the log after its linger, since no subscriber holds it.
    assert!(
        wait_until(Duration::from_secs(8), || !log.is_file()),
        "the log lingered past 5 s"
    );

    // A publication still held when its client closes: open, it is not connected; once
    // the driver has gone and the client has seen it, every refusal is `closed`.
    let mut held = client
        .add_exclusive_publication("cavalry:ipc", 1006)
        .expect("a publication");
    assert!(!held.is_closed());
    assert_eq!(held.offer(b"before"), Err(Offer::NotConnected));
    driver.close().expect("the driver closes");
    assert!(wait_until(Duration::from_secs(3), || client.is_closed()));
    assert!(held.is_closed());
    assert!(!held.is_connected());
    assert_eq!(held.offer(b"after"), Err(Offer::Closed));
    let claim = held.try_claim(8).map(|_| ());
    assert_eq!(claim, Err(Offer::Closed));
    let refusal = Offer::Closed.to_string();
    assert!(refusal.contains("client is closed"), "{refusal}");
    assert!(format!("{held:?}").contains("closed: true"));

    let error = client
        .add_publication("cavalry:ipc", 1005)
        .expect_err("the client is closed");
    assert_eq!(error.reason(), "closed");
    assert!(error.to_string().contains("driver-closed"), "{error}");
}
