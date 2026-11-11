//! An image that comes and goes faster than a client's conductor reads its events,
//! through a real driver in this process: the client stays open, since the race is
//! not its own (invariant 3), and nothing is called corrupt that is not (invariant 6).
//! `spec/log-buffer.md`, "Closing"; `spec/client-protocol.md`, "Events"; ADR 0078.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, Fault, NULL_POSITION};
use cavalry_driver::{Driver, spec};
use cavalry_proto::client_protocol::Command;
use cavalry_shm::command_ring::CommandRing;

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-client-announced-{}-{test}",
            std::process::id()
        ));
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

/// How many times each race is run. On `master` before ADR 0078 the first test closed
/// its client within the first few dozen rounds on a laptop.
const ROUNDS: usize = 300;

#[test]
fn a_publication_added_and_removed_with_nothing_published_leaves_a_subscribers_client_open() {
    let dir = TestDir::new("came-and-went");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut subscriber = Cavalry::connect_to(dir.path()).expect("the subscriber connects");
    let mut subscription = subscriber
        .add_subscription("cavalry:ipc", 2410)
        .expect("a subscription");
    let mut publisher = Cavalry::connect_to(dir.path()).expect("the publisher connects");
    for round in 0..ROUNDS {
        let publication = publisher
            .add_publication("cavalry:ipc", 2410)
            .expect("a publication");
        // Removed at once, with nothing published: its one subscriber is at its end.
        publication.close().expect("the publication closes");
        assert_eq!(
            subscriber.closed(),
            None,
            "round {round}: a peer's add and remove closed the subscriber's client"
        );
        // A poll now and then, as a subscriber polls; none is needed to stay open.
        if round % 7 == 0 {
            subscription.poll(|_, _| {}, 1).expect("polls");
        }
    }
    // The conductor reads what is left of the events, and the client is still open.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        subscription.poll(|_, _| {}, 1).expect("polls");
        assert_eq!(subscriber.closed(), None, "after the last round");
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(subscription);
    drop(driver);
}

#[test]
fn a_subscription_removed_before_its_image_is_read_leaves_its_client_open() {
    let dir = TestDir::new("left-at-once");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut publisher = Cavalry::connect_to(dir.path()).expect("the publisher connects");
    let _publication = publisher
        .add_publication("cavalry:ipc", 2411)
        .expect("a publication");
    let mut subscriber = Cavalry::connect_to(dir.path()).expect("the subscriber connects");
    for round in 0..ROUNDS {
        let subscription = subscriber
            .add_subscription("cavalry:ipc", 2411)
            .expect("a subscription");
        // Removed with no poll between: the driver frees the image's position counter
        // on the removal, and the conductor may read the image's announcement after.
        subscription.close().expect("the subscription closes");
        assert_eq!(
            subscriber.closed(),
            None,
            "round {round}: the client's own add and remove closed it"
        );
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(subscriber.closed(), None, "after the last round");
    drop(driver);
}

fn now_nanos() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos()),
    )
    .unwrap_or(i64::MAX)
}

/// A client that reads an image's announcement after the image's position counter is
/// gone reports the image to its subscription and stays open. The driver keeps the
/// counter for a second after the announcement, so the test takes it away the one way
/// the driver frees it sooner: it removes the subscription behind the client's back.
/// Two commands of the test's own go on the ring one after the other, an `add
/// publication` that gives the subscription an image and the `remove subscription`,
/// and the driver reads them in one pass, so the counter is freed a few microseconds
/// after the announcement, before the client's conductor has opened the log. The
/// subscription's next poll returns the fault `image` with the reason `gone`, once,
/// and polls on, and the client is open, which before ADR 0078 it was not. A round in
/// which the conductor was quicker than the driver has an image and no fault.
#[test]
fn an_image_whose_counter_is_gone_is_reported_to_its_subscription_and_the_client_stays_open() {
    const GONE_ROUNDS: usize = 100;
    let dir = TestDir::new("gone");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut subscriber = Cavalry::connect_to(dir.path()).expect("the subscriber connects");
    let ring = CommandRing::open(dir.path(), true).expect("the ring opens");
    let mut producer = ring.producer(spec::DRIVER_TIMEOUT).expect("a producer");
    let publisher_id = producer.next_correlation_id();
    let mut buffer = vec![0u8; 256];
    let mut write = |producer: &mut cavalry_shm::command_ring::Producer, command: Command<'_>| {
        let length = command.write(&mut buffer).expect("encodes");
        producer
            .write(
                command.command_type() as i32,
                &buffer[..length],
                now_nanos(),
            )
            .expect("written");
    };
    let mut gone = 0;
    for round in 0..GONE_ROUNDS {
        let stream_id = 2500 + i32::try_from(round).expect("a stream id");
        let mut subscription = subscriber
            .add_subscription("cavalry:ipc", stream_id)
            .expect("a subscription");
        let publication_id = producer.next_correlation_id();
        let add = Command::AddExclusivePublication {
            client_id: publisher_id,
            correlation_id: publication_id,
            stream_id,
            channel: b"cavalry:ipc",
        };
        let remove = Command::RemoveSubscription {
            client_id: subscriber.client_id(),
            correlation_id: producer.next_correlation_id(),
            registration_id: subscription.registration_id(),
        };
        write(&mut producer, add);
        write(&mut producer, remove);
        // The conductor reads the announcement within a few milliseconds: the poll
        // says the image was gone, or the image joined.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut faults = 0;
        while faults == 0 && subscription.image_count() == 0 {
            assert!(
                Instant::now() < deadline,
                "round {round}: neither an image nor a fault"
            );
            match subscription.poll(|_, _| {}, 1) {
                Ok(_) => std::thread::sleep(Duration::from_micros(200)),
                Err(Fault::Image {
                    position, reason, ..
                }) => {
                    assert_eq!(
                        (position, reason),
                        (NULL_POSITION, Fault::GONE),
                        "round {round}"
                    );
                    faults += 1;
                }
                Err(other) => panic!("round {round}: {other}"),
            }
        }
        // The fault is reported once; the next polls go on with nothing to read.
        for _ in 0..3 {
            assert_eq!(subscription.poll(|_, _| {}, 1), Ok(0), "round {round}");
        }
        gone += faults;
        assert_eq!(subscriber.closed(), None, "round {round}");
        drop(subscription);
        let remove = Command::RemovePublication {
            client_id: publisher_id,
            correlation_id: producer.next_correlation_id(),
            registration_id: publication_id,
        };
        write(&mut producer, remove);
    }
    assert_eq!(subscriber.closed(), None, "after the last round");
    assert!(
        gone * 2 > GONE_ROUNDS,
        "the conductor read the announcement after the removal in {gone} of {GONE_ROUNDS} \
         rounds, where the driver frees the counter microseconds after it announces"
    );
    drop(driver);
}
