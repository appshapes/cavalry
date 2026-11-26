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
// The one port these tests choose, from the one file that says how ports are taken
// and why, which is `cavalry-raft`'s.
#[path = "../../cavalry-raft/tests/ports/mod.rs"]
mod ports;

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
    // Continued, the child sent a keepalive before it read that it was timed out. The
    // driver registered nobody for it: a second client of that id would be one without
    // a lock, and its own timeout would put the first one's records out of use.
    for (counter, name) in [
        (
            cavalry_shm::counters::generated::CLIENT_WITHOUT_LOCK,
            "client.without_lock",
        ),
        (COUNTERS_RETIRED, "counters.retired"),
    ] {
        assert_eq!(
            owner
                .counters()
                .read(counter.record())
                .expect("a static counter"),
            0,
            "{name}"
        );
    }

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

/// The release word of record `record`, read from the counters file's bytes: the i64
/// at offset 8 of the record's value slot (`spec/counters-file.md`, "Value slot").
fn release_word(dir: &Path, client: &Cavalry, record: u32) -> i64 {
    use std::os::unix::fs::FileExt;
    let capacity = u64::from(client.counters().capacity());
    let offset = 128 + capacity * 256 + u64::from(record) * 128 + 8;
    let file = std::fs::File::open(dir.join("counters")).expect("the counters file");
    let mut word = [0u8; 8];
    file.read_exact_at(&mut word, offset).expect("the word");
    i64::from_le_bytes(word)
}

/// A handle stores its release word once. The driver reads the word, lets the record
/// go and gives it to another owner with the word at 0. A second store of the word,
/// from the drop of a handle that had already given the record up, would set that
/// owner's word, and the driver would then let that owner's record go at once when it
/// freed it, while the owner's thread could still store: the stray store again, one
/// owner on (ADR 0179). Here a client is closed, its subscription's next poll gives
/// its image's record up, another client is given the record, and only then is the
/// subscription dropped.
#[test]
fn a_handle_stores_its_release_word_once() {
    let dir = TestDir::new("once");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut publisher = Cavalry::connect_to(dir.path()).expect("the publisher connects");
    let mut subscriber = Cavalry::connect_to(dir.path()).expect("the subscriber connects");
    let mut subscription = subscriber
        .add_subscription("cavalry:ipc", STREAM + 3)
        .expect("the subscription");
    let mut publication = publisher
        .add_publication("cavalry:ipc", STREAM + 3)
        .expect("the publication");
    wait(
        "the subscriber has its image",
        Duration::from_secs(10),
        || {
            subscription.poll(|_, _| {}, 0).expect("polls");
            (subscription.image_count() == 1).then_some(())
        },
    );
    while publication.offer(&[0u8; 32]).is_err() {
        std::hint::spin_loop();
    }
    let record = position_record(&publisher).expect("the image's position");
    wait("the subscriber reads", Duration::from_secs(10), || {
        subscription.poll(|_, _| {}, 16).expect("polls");
        (publisher.counters().read(record).unwrap_or(0) == 64).then_some(())
    });

    // The client closes; the subscription is not dropped. Its next poll finds the
    // client closed and gives the image's record up: the word's one store.
    subscriber.close().expect("the subscriber's client closes");
    assert!(matches!(
        subscription.poll(|_, _| {}, 16),
        Err(Fault::Closed)
    ));
    assert_eq!(release_word(dir.path(), &publisher, record), 1);
    wait(
        "the driver lets the record go",
        Duration::from_secs(10),
        || {
            (publisher
                .counters()
                .read(COUNTERS_HELD.record())
                .expect("counters.held")
                == 0)
                .then_some(())
        },
    );
    // Another owner is given the record, with its word at 0.
    let mut others = Vec::new();
    let counter = wait(
        "the record is given out again",
        Duration::from_secs(10),
        || {
            let key = i32::try_from(others.len()).expect("a few");
            let counter = publisher
                .add_counter(CLUSTER_COMMIT_POSITION.0, &key.to_le_bytes(), "a counter")
                .expect("added");
            if counter.id() == record {
                return Some(counter);
            }
            others.push(counter);
            assert!(others.len() < 32, "no add took record {record}");
            std::thread::sleep(Duration::from_millis(200));
            None
        },
    );
    counter.store_release(OWNERS_VALUE).expect("stored");
    assert_eq!(release_word(dir.path(), &publisher, record), 0);

    // The closed client's subscription is dropped now, long after it gave the record
    // up. Nothing of it stores: not a position, and not the word a second time.
    assert!(matches!(
        subscription.poll(|_, _| {}, 16),
        Err(Fault::Closed)
    ));
    drop(subscription);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        release_word(dir.path(), &publisher, record),
        0,
        "the next owner's release word, after the first owner's handle was dropped"
    );
    assert_eq!(counter.get(), OWNERS_VALUE);

    counter.close();
    drop(others);
    drop(publication);
    publisher.close().expect("the publisher closes");
    driver.close().expect("the driver closes");
}

/// Over UDP a position's record is freed when the receiver says that its reader is
/// gone, a cycle or more after its subscription went. It is held for its writer all
/// the same: here the client is closed and its subscription is neither polled nor
/// dropped, so nothing has said that its thread is done, and the client's process
/// holds its lock.
#[test]
fn over_udp_a_position_is_held_until_its_writer_lets_it_go() {
    let dir = TestDir::new("udp");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let channel = format!("cavalry:udp?endpoint=127.0.0.1:{}", ports::free_port());
    let mut publisher = Cavalry::connect_to(dir.path()).expect("the publisher connects");
    let mut subscriber = Cavalry::connect_to(dir.path()).expect("the subscriber connects");
    let mut subscription = subscriber
        .add_subscription(&channel, STREAM + 4)
        .expect("the subscription");
    let publication = publisher
        .add_publication(&channel, STREAM + 4)
        .expect("the publication");
    wait("the image arrives", Duration::from_secs(20), || {
        subscription.poll(|_, _| {}, 0).expect("polls");
        (subscription.image_count() == 1).then_some(())
    });
    let held = |client: &Cavalry| {
        client
            .counters()
            .read(COUNTERS_HELD.record())
            .expect("counters.held")
    };
    assert_eq!(held(&publisher), 0);
    subscriber.close().expect("the subscriber's client closes");
    wait("the position is held", Duration::from_secs(10), || {
        (held(&publisher) == 1).then_some(())
    });
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        held(&publisher),
        1,
        "held for as long as the subscription is neither polled nor dropped"
    );
    drop(subscription);
    wait("the record is let go", Duration::from_secs(10), || {
        (held(&publisher) == 0).then_some(())
    });
    drop(publication);
    publisher.close().expect("the publisher closes");
    driver.close().expect("the driver closes");
}

/// A client forgets a subscription it removed when the driver has answered the
/// removal, and not before and not never: more subscriptions than a client may hold
/// at once are added and dropped in turn, and none is refused.
#[test]
fn a_removed_subscription_is_forgotten_once_the_driver_has_answered() {
    let dir = TestDir::new("forgotten");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("the client connects");
    // A client holds at most 1,024 subscriptions.
    for round in 0..1_100 {
        let subscription = client
            .add_subscription("cavalry:ipc", STREAM + 5)
            .unwrap_or_else(|error| panic!("round {round}: {error}"));
        drop(subscription);
    }
    assert_eq!(client.closed(), None);
    client.close().expect("the client closes");
    driver.close().expect("the driver closes");
}

/// Client ids begin again with each driver, so the first client of a driver that
/// started has the id that the first client of the driver before it had. Their lock
/// files do not have one name: each carries which counters file its client has mapped,
/// and the two drivers' are two files for as long as a client of the first has its own
/// mapped. A client of the earlier driver that goes after the restart removes nothing
/// (ADR 0179).
///
/// And a client of the earlier driver that was stopped between taking its id and
/// making its lock file makes it after the restart, in the directory of the driver
/// that started. That file is made here by hand, with the name such a client gives
/// it, and locked. It is not the lock of the new driver's client of that id: that
/// client makes its own, the driver asks it and no other, and what the earlier
/// client's process does with its lock moves nothing of the new client's.
#[test]
fn a_client_of_the_driver_before_a_restart_removes_nothing_of_the_next_drivers() {
    let dir = TestDir::new("restart");
    let first_driver =
        Driver::start(dir.path(), dir.path().join("log")).expect("the first driver starts");
    let old = Cavalry::connect_to(dir.path()).expect("a client of the first driver");
    let id = old.client_id();
    let old_counters = old.counters().file_id();
    let old_name = old_counters.client_file(dir.path(), id, ".lock");
    assert!(old_name.is_file(), "the client's lock file");
    first_driver.close().expect("the first driver closes");

    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the next driver starts");
    assert!(
        !old_name.exists(),
        "a driver that starts removes what the last left"
    );
    // The earlier driver's client, continued between its id and its lock file: its
    // file, in the directory of the driver that started, under the id that driver
    // gives out next. `old` still has the earlier counters file mapped, as that client
    // does, so the file's name is no name of the new driver's.
    let stray = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(&old_name)
        .expect("the earlier client's file");
    stray.try_lock().expect("the earlier client's lock");

    let mut new = Cavalry::connect_to(dir.path()).expect("a client of the next driver");
    assert_eq!(new.client_id(), id, "ids begin again with each driver");
    let name = new
        .counters()
        .file_id()
        .client_file(dir.path(), id, ".lock");
    assert_ne!(name, old_name, "two drivers' counters files, two names");
    assert!(name.is_file(), "the new client made its own lock file");
    let counter = new
        .add_counter(CLUSTER_COMMIT_POSITION.0, &1i32.to_le_bytes(), "a counter")
        .expect("added");

    // The earlier driver's client goes now, closed and dropped.
    let _ = old.close();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        name.is_file(),
        "the next driver's client still has its lock file after the earlier client went"
    );
    // And the driver still knows the new client as one it can ask: closed with a
    // counter not given up, its record is held, and let go when the counter is dropped.
    let watcher = Cavalry::connect_to(dir.path()).expect("a third client");
    let read = |counter: cavalry_shm::counters::CounterId| {
        watcher
            .counters()
            .read(counter.record())
            .expect("a static counter")
    };
    new.close().expect("the new client closes");
    wait("the record is held", Duration::from_secs(10), || {
        (read(COUNTERS_HELD) == 1).then_some(())
    });
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(read(COUNTERS_HELD), 1, "held while its handle exists");
    assert_eq!(read(COUNTERS_RETIRED), 0);
    // The earlier client's process ends, and its lock is free. The new client was
    // closed and its file renamed, so a driver that took the earlier client's file for
    // the new client's would take the lock now and give the record away under the
    // counter that is still held here.
    drop(stray);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        read(COUNTERS_HELD),
        1,
        "another driver's client let its lock go, and this client's record is still held"
    );
    assert!(old_name.is_file(), "the driver does not touch that file");
    drop(counter);
    wait("the record is let go", Duration::from_secs(10), || {
        (read(COUNTERS_HELD) == 0).then_some(())
    });
    assert_eq!(read(COUNTERS_RETIRED), 0);
    // The last object of the client is gone, so its lock is free: the driver takes it
    // and removes the file, which only the driver does.
    let gone = watcher
        .counters()
        .file_id()
        .client_file(dir.path(), id, ".gone");
    wait(
        "the driver removes the file of the client that went",
        Duration::from_secs(10),
        || (!gone.exists() && !name.exists()).then_some(()),
    );
    assert_eq!(
        read(cavalry_shm::counters::generated::CLIENT_WITHOUT_LOCK),
        0,
        "every client held its lock, and a handle dropped after its client closed sent \
         the driver no command, which would have read as a new client's first"
    );
    watcher.close().expect("the third client closes");
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
    // Two handles that write no record, kept past the client's close.
    let publication = client
        .add_publication("cavalry:ipc", STREAM + 6)
        .expect("a publication");
    let subscription = client
        .add_subscription("cavalry:ipc", STREAM + 7)
        .expect("a subscription");
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
    // The driver forgot this client when it went, since it has no lock to wait for,
    // so a command that named it now would be a new client's first. A handle that is
    // dropped after its client closed sends none.
    let without_lock = cavalry_shm::counters::generated::CLIENT_WITHOUT_LOCK;
    assert_eq!(
        read(&watcher, without_lock),
        2,
        "the watcher and the client"
    );
    drop(publication);
    drop(subscription);
    // After the counter reuse timeout the released record is given out again, and the
    // other never is.
    std::thread::sleep(Duration::from_millis(1_200));
    assert_eq!(
        read(&watcher, without_lock),
        2,
        "a handle dropped after its client closed registered a client"
    );
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
