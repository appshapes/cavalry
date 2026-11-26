//! A client that was stopped must not write into a counter another client now owns
//! (work item 506; CLAUDE.md, invariant 1; `spec/client-protocol.md`, "Liveness and
//! timeouts").
//!
//! A subscriber stores its image's position after each poll. The driver times out a
//! client that has sent nothing for the client liveness timeout, 10 s, frees its
//! counters, and gives a freed record to the next counter that is added once the
//! counter reuse timeout, 1 s, has passed. A client learns that it was timed out from
//! the `client timeout` event, which its conductor reads. So a process that was stopped
//! for longer than the two timeouts wakes with a thread that may poll, and store, before
//! its conductor has read the event.
//!
//! The test stops a child process that holds a subscription with `SIGSTOP`, sent to that
//! process by its id, as `cavalry-raft`'s throttle test does, and continues it with
//! `SIGCONT` once another client holds the record the child's position was in.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, Fault, Offer};
use cavalry_driver::Driver;
use cavalry_shm::counters::generated::{
    CLUSTER_COMMIT_POSITION, COUNTERS_HELD, COUNTERS_RETIRED, SUBSCRIPTION_POSITION,
};

/// The child's directory, when this process is the child.
const CHILD_DIR: &str = "CAVALRY_STRAY_STORE_DIR";
const STREAM: i32 = 5061;
/// What the second client stores in its counter, a value no position of this test is.
const OWNERS_VALUE: i64 = 7_000_000_007;

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-client-stray-{}-{test}",
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

/// The child, continued and ended when the guard drops: a test that fails must not
/// leave a stopped process behind, nor one that polls for ever.
struct Child(std::process::Child);

impl Drop for Child {
    fn drop(&mut self) {
        // Only a child that has not ended yet: its id may be another process's after.
        if let Ok(None) = self.0.try_wait() {
            let _ = Command::new("kill")
                .arg("-CONT")
                .arg(self.0.id().to_string())
                .status();
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn wait<T>(what: &str, within: Duration, mut done: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + within;
    loop {
        if let Some(value) = done() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn signal(pid: u32, name: &str) {
    let status = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(pid.to_string())
        .status()
        .expect("kill runs");
    assert!(status.success(), "kill -{name} {pid}");
}

/// The record that holds a subscriber position, the only one of this test's driver.
fn position_record(client: &Cavalry) -> Option<u32> {
    let counters = client.counters();
    (0..counters.capacity()).find(|id| {
        counters
            .record(*id)
            .ok()
            .flatten()
            .is_some_and(|fields| fields.type_id() == SUBSCRIPTION_POSITION.0)
    })
}

#[test]
fn a_client_that_was_stopped_does_not_store_into_a_record_another_client_owns() {
    let dir = TestDir::new("stopped");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut publisher = Cavalry::connect_to(dir.path()).expect("the publisher connects");
    let mut publication = publisher
        .add_publication("cavalry:ipc", STREAM)
        .expect("the publication");

    let child = Command::new(std::env::current_exe().expect("this program's path"))
        .args([
            "a_subscriber_polls_until_its_client_is_closed",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_DIR, dir.path())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the child starts");
    let mut child = Child(child);
    let pid = child.0.id();
    let mut said = BufReader::new(child.0.stdout.take().expect("the child's output"));
    let mut line = String::new();
    wait("the child has its image", Duration::from_secs(30), || {
        line.clear();
        said.read_line(&mut line).expect("a line of the child's");
        // The test harness writes the test's name before the test's own first line.
        assert!(!line.is_empty(), "the child ended before it was ready");
        line.contains("subscriber: ready").then_some(())
    });

    // The child reads 1,000 messages, so its position is far from 0 and from the
    // second client's value.
    for _ in 0..1_000 {
        loop {
            match publication.offer(&[0u8; 32]) {
                Ok(_) => break,
                Err(Offer::NotConnected | Offer::BackPressured | Offer::AdminAction) => {
                    std::hint::spin_loop();
                }
                Err(other) => panic!("{other}"),
            }
        }
    }
    let record = position_record(&publisher).expect("the child's position counter");
    let position = wait(
        "the child reads what was published",
        Duration::from_secs(10),
        || {
            let value = publisher.counters().read(record).unwrap_or(0);
            (value >= 64_000).then_some(value)
        },
    );

    // Stopped: the child sends no keepalive, the driver times it out and frees the
    // record, and after the counter reuse timeout the record is free for the next add.
    signal(pid, "STOP");
    wait(
        "the driver times the stopped client out and frees its position",
        Duration::from_secs(20),
        || {
            publisher
                .counters()
                .record(record)
                .ok()
                .flatten()
                .is_none()
                .then_some(())
        },
    );
    let mut owner = Cavalry::connect_to(dir.path()).expect("the second client connects");
    let held = |client: &Cavalry| {
        client
            .counters()
            .read(COUNTERS_HELD.record())
            .expect("counters.held")
    };
    assert!(
        held(&owner) >= 1,
        "the stopped client's position is held for its writer"
    );
    // The driver gives an add the first record that is free, lowest first. The second
    // client waits out the counter reuse timeout and then adds counters, and keeps
    // them: none of them may be in the record the stopped client's thread can still
    // store into, however long that client has been stopped.
    std::thread::sleep(Duration::from_millis(1_500));
    let mut others = Vec::new();
    for key in 0..16i32 {
        let counter = owner
            .add_counter(
                CLUSTER_COMMIT_POSITION.0,
                &key.to_le_bytes(),
                "cluster.commit_position of the second client",
            )
            .expect("added");
        assert_ne!(
            counter.id(),
            record,
            "add {key} was given record {record}, which the stopped client's thread stores \
             its position, {position}, into when it goes on"
        );
        counter.store_release(OWNERS_VALUE).expect("stored");
        others.push(counter);
    }

    // Continued: the child's thread polls on and stores, into the record that is still
    // its own, until its conductor reads `client timeout`.
    signal(pid, "CONT");
    line.clear();
    said.read_line(&mut line).expect("the child's last line");
    let status = child.0.wait().expect("the child ends");
    assert!(status.success(), "the child: {line}");
    assert!(line.contains("subscriber: closed"), "{line}");
    for counter in &others {
        assert_eq!(
            counter.get(),
            OWNERS_VALUE,
            "the second client's counter in record {} after the stopped client went on; \
             the child said: {line}",
            counter.id()
        );
    }
    // The child's process has ended, so its lock is free: the driver takes it, lets the
    // record go, and after the counter reuse timeout gives it to the next add.
    wait(
        "the driver lets the stopped client's record go once its process has ended",
        Duration::from_secs(10),
        || (held(&owner) == 0).then_some(()),
    );
    let again = wait(
        "the record is given out again",
        Duration::from_secs(10),
        || {
            let counter = owner
                .add_counter(
                    CLUSTER_COMMIT_POSITION.0,
                    &99i32.to_le_bytes(),
                    "cluster.commit_position of the second client",
                )
                .expect("added");
            if counter.id() == record {
                return Some(counter);
            }
            counter.close();
            std::thread::sleep(Duration::from_millis(200));
            None
        },
    );
    assert_eq!(again.get(), 0, "an allocation sets the value to 0");

    again.close();
    drop(others);
    drop(publication);
    owner.close().expect("the second client closes");
    publisher.close().expect("the publisher closes");
    driver.close().expect("the driver closes");
}

/// A poll whose handler takes its time stores its position when the handler returns,
/// which may be long after the client's conductor has read `unavailable image` for
/// that image. The record must still be that subscriber's then: the driver may wait
/// for no thread but the one that stores (ADR 0179). Here the handler sleeps while its
/// publication ends, the linger passes, the driver frees the image's position, the
/// counter reuse timeout passes and the next publication on the stream is added.
#[test]
fn a_poll_inside_its_handler_stores_into_a_record_that_is_still_its_own() {
    let dir = TestDir::new("handler");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut publisher = Cavalry::connect_to(dir.path()).expect("the publisher connects");
    let mut subscriber = Cavalry::connect_to(dir.path()).expect("the subscriber connects");
    let mut subscription = subscriber
        .add_subscription("cavalry:ipc", STREAM + 1)
        .expect("the subscription");
    let mut first = publisher
        .add_publication("cavalry:ipc", STREAM + 1)
        .expect("the first publication");
    wait(
        "the subscriber has its image",
        Duration::from_secs(10),
        || {
            subscription.poll(|_, _| {}, 0).expect("polls");
            (subscription.image_count() == 1).then_some(())
        },
    );
    let record = position_record(&publisher).expect("the image's position");
    while first.offer(&[0u8; 32]).is_err() {
        std::hint::spin_loop();
    }

    let in_handler = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let go_on = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let poller = {
        let (in_handler, go_on) = (in_handler.clone(), go_on.clone());
        std::thread::spawn(move || {
            let mut delivered = 0;
            while delivered == 0 {
                delivered = subscription
                    .poll(
                        |_, _| {
                            in_handler.store(true, std::sync::atomic::Ordering::Release);
                            while !go_on.load(std::sync::atomic::Ordering::Acquire) {
                                std::thread::sleep(Duration::from_millis(5));
                            }
                        },
                        1,
                    )
                    .expect("polls");
            }
            subscription
        })
    };
    wait(
        "the poll is inside its handler",
        Duration::from_secs(10),
        || {
            in_handler
                .load(std::sync::atomic::Ordering::Acquire)
                .then_some(())
        },
    );

    // The publication ends. Its subscriber's position stands where it was, inside the
    // handler, so the driver waits out the linger, sends `unavailable image` and frees
    // the record. The subscriber's conductor reads the event meanwhile.
    first.close().expect("the first publication closes");
    wait(
        "the driver frees the ended image's position after the linger",
        Duration::from_secs(20),
        || {
            publisher
                .counters()
                .record(record)
                .ok()
                .flatten()
                .is_none()
                .then_some(())
        },
    );
    std::thread::sleep(Duration::from_millis(1_500));
    let mut second = publisher
        .add_publication("cavalry:ipc", STREAM + 1)
        .expect("the second publication");
    let next = wait(
        "the next image's position is allocated",
        Duration::from_secs(10),
        || position_record(&publisher),
    );
    assert_ne!(
        next, record,
        "the next image was given the record a poll inside its handler is about to store into"
    );
    assert_eq!(publisher.counters().read(next).expect("its value"), 0);

    // The handler returns, and the poll stores its position: into the held record.
    go_on.store(true, std::sync::atomic::Ordering::Release);
    let mut subscription = poller.join().expect("the poller ends");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        publisher.counters().read(next).expect("its value"),
        0,
        "the next image's position, which nothing has read, after the poll's store"
    );
    // The next poll takes the ended image out and the new one in, and the driver lets
    // the held record go.
    while second.offer(&[0u8; 32]).is_err() {
        std::hint::spin_loop();
    }
    let mut read = 0;
    wait(
        "the subscriber reads the next image",
        Duration::from_secs(10),
        || {
            read += subscription.poll(|_, _| {}, 16).expect("polls");
            (read == 1).then_some(())
        },
    );
    wait("the held record is let go", Duration::from_secs(10), || {
        (publisher
            .counters()
            .read(COUNTERS_HELD.record())
            .expect("counters.held")
            == 0)
            .then_some(())
    });

    drop(second);
    drop(subscription);
    subscriber.close().expect("the subscriber closes");
    publisher.close().expect("the publisher closes");
    driver.close().expect("the driver closes");
}

/// A client and a driver of the two sides of ADR 0179 do not attach to each other: a
/// client of this build refuses a counters file of version 0, which a driver before
/// that decision wrote, before it sends a command, and its error says that the driver
/// is the older side and what to do (`spec/client-protocol.md`, "Crossing to this
/// version"). So nothing of this client stores into a record such a driver could give
/// away, and such a driver learns of no client.
#[test]
fn a_client_refuses_an_older_drivers_counters_file_before_it_sends_a_command() {
    use std::os::unix::fs::FileExt;
    let dir = TestDir::new("version");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    // The running driver's file, with the version an older driver wrote: the header's
    // i32 at offset 8 (`spec/counters-file.md`, "Header").
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join("counters"))
        .expect("the counters file");
    file.write_all_at(&0i32.to_le_bytes(), 8)
        .expect("the version is written");
    // The command ring's correlation ids are taken in order, a client's id first and
    // then one for each command: two ids taken here, one before and one after, differ
    // by one when the client between them took none.
    let ring = cavalry_shm::command_ring::CommandRing::open(dir.path(), true).expect("the ring");
    let ids = ring
        .producer(cavalry_driver::spec::DRIVER_TIMEOUT)
        .expect("a producer");
    let before = ids.next_correlation_id();
    let error = match Cavalry::connect_to(dir.path()) {
        Ok(_) => panic!("a client attached to a counters file of version 0"),
        Err(error) => error.to_string(),
    };
    for part in [
        "version 0, this program reads version 1",
        "the driver of this directory is of an older build of Cavalry than this program",
        "Stop that driver and every client of it, and start them from this program's build, the \
         driver first",
    ] {
        assert!(error.contains(part), "{part}: {error}");
    }
    assert_eq!(
        ids.next_correlation_id(),
        before + 1,
        "the client that refused took no client id and sent no command"
    );
    assert_eq!(
        std::fs::read_dir(dir.path().join("clients"))
            .expect("the directory of the locks")
            .count(),
        0,
        "the client that refused made no lock file"
    );
    // The driver was sent nothing: the next client is its first.
    file.write_all_at(&1i32.to_le_bytes(), 8)
        .expect("the version is put back");
    let client = Cavalry::connect_to(dir.path()).expect("a client of this build attaches");
    let registered = client
        .counters()
        .read(cavalry_shm::counters::generated::COUNTERS_ALLOCATED.record())
        .expect("a static counter");
    assert!(registered > 0);
    client.close().expect("the client closes");
    driver.close().expect("the driver closes");
}

/// A client that holds no lock, here because the driver's directory has no `clients/`
/// for it, attaches all the same. What it lets go of by its release word is given out
/// again as any record. What it leaves with no word when it closes is out of use until
/// the driver starts again, counted in `counters.retired`: nothing tells the driver
/// that such a client can no longer store (`spec/client-protocol.md`, "The records a
/// client writes").
#[test]
fn what_a_client_without_its_lock_leaves_unreleased_is_out_of_use() {
    let dir = TestDir::new("no-lock");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    std::fs::remove_dir(dir.path().join("clients")).expect("the directory of the locks goes");
    let mut watcher = Cavalry::connect_to(dir.path()).expect("a client connects without it");
    let read = |client: &Cavalry, counter: cavalry_shm::counters::CounterId| {
        client
            .counters()
            .read(counter.record())
            .expect("a static counter")
    };
    let mut client = Cavalry::connect_to(dir.path()).expect("the client connects");
    let add = |client: &mut Cavalry, key: i32| {
        client
            .add_counter(CLUSTER_COMMIT_POSITION.0, &key.to_le_bytes(), "a counter")
            .expect("added")
    };
    let closed = add(&mut client, 1);
    let closed_record = closed.id();
    let leaked = add(&mut client, 2);
    let leaked_record = leaked.id();
    // One handle is closed, which stores its release word. The other is never dropped:
    // a handle some thread could still store through.
    closed.close();
    std::mem::forget(leaked);
    client.close().expect("the client closes");
    wait(
        "the driver sorts what the client left",
        Duration::from_secs(10),
        || {
            (read(&watcher, COUNTERS_RETIRED) == 1 && read(&watcher, COUNTERS_HELD) == 0)
                .then_some(())
        },
    );
    // After the counter reuse timeout the released record is given out again, and the
    // other never is.
    std::thread::sleep(Duration::from_millis(1_200));
    let mut given = Vec::new();
    for key in 10..26 {
        given.push(add(&mut watcher, key));
    }
    let ids: Vec<u32> = given.iter().map(cavalry_client::Counter::id).collect();
    assert!(ids.contains(&closed_record), "{closed_record} in {ids:?}");
    assert!(!ids.contains(&leaked_record), "{leaked_record} in {ids:?}");

    drop(given);
    watcher.close().expect("the watcher closes");
    driver.close().expect("the driver closes");
}

/// Publications that come and go under subscriptions that come and go, some dropped
/// before they polled an image they were told of: every record the driver held is let
/// go in the end, whoever stored its release word, the subscription's thread, a
/// dropped handle or the client's conductor (`spec/client-protocol.md`, "The records a
/// client writes").
#[test]
fn every_record_held_for_an_image_is_let_go_whoever_was_told_of_it() {
    let dir = TestDir::new("churn");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut publisher = Cavalry::connect_to(dir.path()).expect("the publisher connects");
    let mut subscriber = Cavalry::connect_to(dir.path()).expect("the subscriber connects");
    let mut steady = subscriber
        .add_subscription("cavalry:ipc", STREAM + 2)
        .expect("a subscription that stays");
    for round in 0..60 {
        let passing = subscriber
            .add_subscription("cavalry:ipc", STREAM + 2)
            .expect("a subscription that goes");
        let publication = publisher
            .add_publication("cavalry:ipc", STREAM + 2)
            .expect("a publication");
        match round % 3 {
            // Dropped at once: the driver may have announced an image for it already.
            0 => drop(passing),
            // Polled, so that it holds the image, then dropped.
            1 => {
                let mut passing = passing;
                for _ in 0..50 {
                    passing.poll(|_, _| {}, 1).expect("polls");
                }
                drop(passing);
            }
            // Dropped after its publication went.
            _ => {
                drop(publication);
                steady.poll(|_, _| {}, 1).expect("polls");
                drop(passing);
                continue;
            }
        }
        steady.poll(|_, _| {}, 1).expect("polls");
        drop(publication);
    }
    assert_eq!(
        subscriber.closed(),
        None,
        "the subscriber's client stays open"
    );
    wait(
        "every held record is let go",
        Duration::from_secs(20),
        || {
            steady.poll(|_, _| {}, 1).expect("polls");
            (subscriber
                .counters()
                .read(COUNTERS_HELD.record())
                .expect("counters.held")
                == 0)
                .then_some(())
        },
    );
    assert_eq!(
        subscriber
            .counters()
            .read(COUNTERS_RETIRED.record())
            .expect("counters.retired"),
        0
    );
    drop(steady);
    subscriber.close().expect("the subscriber closes");
    publisher.close().expect("the publisher closes");
    driver.close().expect("the driver closes");
}

/// The child of
/// [`a_client_that_was_stopped_does_not_store_into_a_record_another_client_owns`]: a
/// subscriber that polls without a pause until its client is closed, and then says how
/// many of its polls came back after the longest gap between two of them. On its own it
/// has nothing to do.
#[test]
#[ignore = "the child of a_client_that_was_stopped_does_not_store_into_a_record_another_client_owns"]
fn a_subscriber_polls_until_its_client_is_closed() {
    let Some(dir) = std::env::var_os(CHILD_DIR) else {
        return;
    };
    let mut client = Cavalry::connect_to(PathBuf::from(dir)).expect("the child connects");
    let mut subscription = client
        .add_subscription("cavalry:ipc", STREAM)
        .expect("the subscription");
    while subscription.image_count() == 0 {
        subscription.poll(|_, _| {}, 0).expect("polls");
        std::thread::sleep(Duration::from_millis(1));
    }
    println!("subscriber: ready");
    std::io::stdout().flush().expect("flushed");

    let mut last = Instant::now();
    let mut gap = Duration::ZERO;
    let mut after_gap = 0u64;
    // A bound, so that this process ends whatever its parent does: the parent stops it
    // for about 12 s, and the clock runs on while it is stopped.
    let end = last + Duration::from_secs(90);
    let fault = loop {
        match subscription.poll(|_, _| {}, 16) {
            Ok(_) => {
                let now = Instant::now();
                assert!(now < end, "the client was not closed in 90 s");
                let since = now.duration_since(last);
                last = now;
                if since > gap && since > Duration::from_secs(1) {
                    gap = since;
                    after_gap = 0;
                }
                after_gap += 1;
            }
            Err(fault) => break fault,
        }
    };
    assert!(matches!(fault, Fault::Closed), "{fault:?}");
    println!(
        "subscriber: closed as {:?}; {after_gap} polls returned after a gap of {gap:?}, each of \
         which stored the image's position",
        client.closed()
    );
    std::io::stdout().flush().expect("flushed");
}
