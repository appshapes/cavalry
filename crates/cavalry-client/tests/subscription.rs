//! Publish and subscribe through a real driver in this process: every message once and
//! in order (invariant 1), a late joiner sees nothing before its join, a slow subscriber
//! back-pressures its publisher and loses nothing, images come and go with publications
//! (`spec/log-buffer.md`, "Reading"; `spec/client-protocol.md`, "Events"; ADR 0021).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, Fault, Offer};
use cavalry_driver::{Driver, spec};

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("cavalry-client-sub-{}-{test}", std::process::id()));
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

/// Publishes `message` as soon as the publication lets it: a rotation or a window that
/// the subscriber has yet to free is tried again.
fn publish(publication: &mut cavalry_client::Publication, message: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match publication.offer(message) {
            Ok(_) => return,
            Err(Offer::NotConnected | Offer::BackPressured | Offer::AdminAction) => {
                assert!(Instant::now() < deadline, "the publisher never got through");
                std::hint::spin_loop();
            }
            Err(other) => panic!("{other}"),
        }
    }
}

#[test]
fn every_message_arrives_once_and_in_order() {
    let dir = TestDir::new("order");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let mut subscription = client
        .add_subscription("cavalry:ipc", 2001)
        .expect("a subscription");
    assert!(subscription.registration_id() >= 0);
    assert_eq!(subscription.stream_id(), 2001);
    assert_eq!(subscription.channel(), "cavalry:ipc");
    assert_eq!(subscription.image_count(), 0);
    assert!(!subscription.is_connected());
    let mut publication = client
        .add_publication("cavalry:ipc", 2001)
        .expect("a publication");

    // The image arrives through the conductor and joins on the next poll.
    assert!(
        wait_until(Duration::from_secs(5), || {
            subscription.poll(|_, _| {}, 1).expect("polls");
            subscription.is_connected()
        }),
        "the subscription connected"
    );
    assert_eq!(subscription.image_count(), 1);
    assert_eq!(
        subscription.images()[0].session_id(),
        publication.session_id()
    );
    assert_eq!(subscription.images()[0].source_identity(), "cavalry:ipc");
    assert!(wait_until(Duration::from_secs(5), || publication.is_connected()));

    let total: u64 = 100_000;
    let mut next: u64 = 0;
    let mut sent: u64 = 0;
    let mut polls = 0;
    while next < total {
        if sent < total {
            let body = sent.to_le_bytes();
            match publication.offer(&body) {
                Ok(_) => sent += 1,
                Err(Offer::BackPressured | Offer::AdminAction | Offer::NotConnected) => {}
                Err(other) => panic!("{other}"),
            }
        }
        let received = subscription
            .poll(
                |body, header| {
                    let value = u64::from_le_bytes(body.try_into().expect("8 bytes"));
                    assert_eq!(value, next, "in order, once");
                    assert_eq!(header.stream_id(), 2001);
                    assert_eq!(header.session_id(), publication.session_id());
                    next += 1;
                },
                64,
            )
            .expect("polls");
        polls += 1;
        assert!(polls < 50_000_000, "stuck at {next} of {total}");
        let _ = received;
    }
    assert_eq!(next, total);
    assert_eq!(
        subscription.images()[0].position(),
        publication.position().expect("position")
    );

    publication.close().expect("closes");
    subscription.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

/// The bounded poll (`spec/log-buffer.md`, "Reading"; ADR 0066): one image of the
/// subscription is read, the one of the session named, and no frame of it that begins
/// at or past the limit; the other image is not read by the call, the image's position
/// is stored after it, and a plain poll reads what is left of both.
#[test]
fn a_bounded_poll_reads_one_image_and_no_frame_at_or_past_its_limit() {
    let dir = TestDir::new("bounded");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let mut subscription = client
        .add_subscription("cavalry:ipc", 2005)
        .expect("a subscription");
    let mut first = client
        .add_exclusive_publication("cavalry:ipc", 2005)
        .expect("a publication");
    let mut second = client
        .add_exclusive_publication("cavalry:ipc", 2005)
        .expect("a second publication");
    assert!(
        wait_until(Duration::from_secs(5), || {
            subscription.poll(|_, _| {}, 0).expect("polls");
            subscription.image_count() == 2
        }),
        "both images joined"
    );
    let (a, b) = (first.session_id(), second.session_id());
    let position = |subscription: &cavalry_client::Subscription, session: i32| {
        subscription
            .images()
            .iter()
            .find(|image| image.session_id() == session)
            .expect("the image")
            .position()
    };
    // Five frames of 64 bytes on the first, at 0, 64, 128, 192 and 256, and three on
    // the second.
    for i in 0..5u64 {
        publish(&mut first, &i.to_le_bytes());
    }
    for i in 100..103u64 {
        publish(&mut second, &i.to_le_bytes());
    }
    assert_eq!(first.position().expect("position"), 320);

    let mut seen: Vec<(i32, u64)> = Vec::new();
    let mut read = |subscription: &mut cavalry_client::Subscription,
                    session: i32,
                    limit: i64,
                    fragments: usize| {
        let before = seen.len();
        let delivered = subscription
            .bounded_poll(
                session,
                |body, header| {
                    let value = u64::from_le_bytes(body.try_into().expect("8 bytes"));
                    seen.push((header.session_id(), value));
                },
                limit,
                fragments,
            )
            .expect("the bounded poll reads");
        assert_eq!(delivered, seen.len() - before);
        seen[before..].to_vec()
    };

    // A limit at a frame's first byte: the two frames below it, and not that frame.
    assert_eq!(read(&mut subscription, a, 128, 10), [(a, 0), (a, 1)]);
    assert_eq!(position(&subscription, a), 128, "the position is stored");
    assert_eq!(position(&subscription, b), 0, "the other image is not read");
    // One byte more: the frame that begins below the limit is delivered whole.
    assert_eq!(read(&mut subscription, a, 129, 10), [(a, 2)]);
    assert_eq!(position(&subscription, a), 192);
    // A limit at or below the image's position reads nothing and moves nothing.
    for limit in [192, 64, 0, -1, i64::MIN] {
        assert_eq!(read(&mut subscription, a, limit, 10), []);
    }
    assert_eq!(position(&subscription, a), 192);
    // A session the subscription holds no image of reads nothing. A driver's session
    // ids start at a random `i32`, so the made-up one is chosen to be neither image's:
    // the id after `a`, or the one after that when the first is `b`.
    let neither = if a.wrapping_add(1) == b {
        a.wrapping_add(2)
    } else {
        a.wrapping_add(1)
    };
    assert_eq!(read(&mut subscription, neither, i64::MAX, 10), []);
    // The fragment limit holds under the largest limit, which is no bound.
    assert_eq!(
        read(&mut subscription, b, i64::MAX, 2),
        [(b, 100), (b, 101)]
    );
    assert_eq!(position(&subscription, b), 128);

    // A plain poll reads what is left of both images, each in its order.
    let mut rest: Vec<(i32, u64)> = Vec::new();
    assert!(wait_until(Duration::from_secs(5), || {
        subscription
            .poll(
                |body, header| {
                    let value = u64::from_le_bytes(body.try_into().expect("8 bytes"));
                    rest.push((header.session_id(), value));
                },
                10,
            )
            .expect("polls");
        rest.len() == 3
    }));
    rest.sort_unstable();
    let mut expected = vec![(a, 3), (a, 4), (b, 102)];
    expected.sort_unstable();
    assert_eq!(rest, expected);
    assert_eq!(position(&subscription, a), 320);
    assert_eq!(position(&subscription, b), 192);

    // A closed client's subscription refuses the bounded poll as it refuses a poll.
    first.close().expect("closes");
    second.close().expect("closes");
    client.close().expect("closes");
    assert!(matches!(
        subscription.bounded_poll(a, |_, _| {}, i64::MAX, 1),
        Err(Fault::Closed)
    ));
    drop(subscription);
    driver.close().expect("the driver closes");
}

#[test]
fn a_late_joiner_sees_nothing_before_its_join_and_a_slow_one_loses_nothing() {
    let dir = TestDir::new("late");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let mut early = client
        .add_subscription("cavalry:ipc", 2002)
        .expect("a subscription");
    let mut publication = client
        .add_publication("cavalry:ipc", 2002)
        .expect("a publication");
    assert!(wait_until(Duration::from_secs(5), || {
        early.poll(|_, _| {}, 1).expect("polls");
        early.is_connected()
    }));
    for i in 0..10u64 {
        publish(&mut publication, &i.to_le_bytes());
    }

    // The late joiner's image starts at the producer's position: nothing before it.
    let mut late = client
        .add_subscription("cavalry:ipc", 2002)
        .expect("a subscription");
    assert!(wait_until(Duration::from_secs(5), || {
        late.poll(|_, _| panic!("a message from before the join"), 64)
            .expect("polls");
        late.is_connected()
    }));
    assert_eq!(
        late.images()[0].position(),
        publication.position().expect("position")
    );
    for i in 10..20u64 {
        publish(&mut publication, &i.to_le_bytes());
    }
    let mut late_seen = Vec::new();
    assert!(wait_until(Duration::from_secs(5), || {
        late.poll(
            |body, _| late_seen.push(u64::from_le_bytes(body.try_into().expect("8 bytes"))),
            64,
        )
        .expect("polls");
        late_seen.len() == 10
    }));
    assert_eq!(late_seen, (10..20).collect::<Vec<u64>>());

    // The early subscriber has not polled: the publisher is held at its window, then
    // continues when it reads, and every message is there.
    let mut written: u64 = 20;
    let deadline = Instant::now() + Duration::from_secs(20);
    let held = loop {
        match publication.offer(&written.to_le_bytes()) {
            Ok(_) => written += 1,
            Err(Offer::AdminAction) => {}
            Err(Offer::BackPressured) => break true,
            Err(other) => panic!("{other}"),
        }
        assert!(Instant::now() < deadline, "never back-pressured");
    };
    assert!(held);
    assert_eq!(
        written * 64,
        u64::from(spec::IPC_PUBLICATION_WINDOW),
        "held at exactly the window behind the slow subscriber"
    );
    let mut early_next: u64 = 0;
    assert!(wait_until(Duration::from_secs(20), || {
        early
            .poll(
                |body, _| {
                    let value = u64::from_le_bytes(body.try_into().expect("8 bytes"));
                    assert_eq!(value, early_next);
                    early_next += 1;
                },
                1024,
            )
            .expect("polls");
        early_next == written
    }));
    // The limit moves in steps of window ÷ 8 behind the slowest subscriber
    // (`spec/log-buffer.md`, "The driver"), and the late joiner is now the slowest: it
    // reads too, and then the publisher continues.
    assert!(wait_until(Duration::from_secs(20), || {
        late.poll(
            |body, _| late_seen.push(u64::from_le_bytes(body.try_into().expect("8 bytes"))),
            1024,
        )
        .expect("polls");
        late_seen.len() as u64 == written - 10
    }));
    assert_eq!(late_seen.last().copied(), Some(written - 1));
    assert!(
        wait_until(Duration::from_secs(5), || publication
            .offer(&written.to_le_bytes())
            .is_ok()),
        "the publisher continues once both subscribers read"
    );

    publication.close().expect("closes");
    early.close().expect("closes");
    late.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

#[test]
fn images_come_and_go_with_publications() {
    let dir = TestDir::new("images");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let mut subscription = client
        .add_subscription("cavalry:ipc", 2003)
        .expect("a subscription");
    let shared = client
        .add_publication("cavalry:ipc", 2003)
        .expect("a publication");
    let exclusive = client
        .add_exclusive_publication("cavalry:ipc", 2003)
        .expect("a publication");
    assert!(wait_until(Duration::from_secs(5), || {
        subscription.poll(|_, _| {}, 1).expect("polls");
        subscription.image_count() == 2
    }));
    let mut sessions: Vec<i32> = subscription
        .images()
        .iter()
        .map(|image| image.session_id())
        .collect();
    sessions.sort_unstable();
    let mut expected = vec![shared.session_id(), exclusive.session_id()];
    expected.sort_unstable();
    assert_eq!(sessions, expected);
    assert!(format!("{subscription:?}").contains("images: 2"));
    assert!(format!("{:?}", subscription.images()[0]).contains("Image"));

    // The exclusive publication closes: its image lingers while a subscriber has not
    // reached the end of stream, then goes; the subscription keeps the other.
    exclusive.close().expect("closes");
    assert!(
        wait_until(Duration::from_secs(8), || {
            subscription.poll(|_, _| {}, 64).expect("polls");
            subscription.image_count() == 1
        }),
        "the image went with its publication"
    );
    assert_eq!(subscription.images()[0].session_id(), shared.session_id());
    assert!(!subscription.images()[0].is_end_of_stream());

    subscription.close().expect("closes");
    shared.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}

#[test]
fn a_refused_channel_is_refused_and_a_closed_clients_subscription_refuses_to_poll() {
    let dir = TestDir::new("closed");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let error = client
        .add_subscription("cavalry:udp?endpoint=localhost:1", 2004)
        .expect_err("not this version's channel");
    assert_eq!(error.reason(), "refused");

    // A subscription still held when its client closes: open, it polls; once the driver
    // has gone and the client has seen it, every poll is `closed`.
    let mut subscription = client
        .add_subscription("cavalry:ipc", 2004)
        .expect("a subscription");
    assert!(!subscription.is_closed());
    assert_eq!(subscription.poll(|_, _| {}, 10), Ok(0));
    driver.close().expect("the driver closes");
    assert!(wait_until(Duration::from_secs(3), || client.is_closed()));
    assert!(subscription.is_closed());
    assert_eq!(subscription.poll(|_, _| {}, 10), Err(Fault::Closed));
    let refusal = Fault::Closed.to_string();
    assert!(refusal.contains("client is closed"), "{refusal}");
    assert!(format!("{subscription:?}").contains("closed: true"));
}
