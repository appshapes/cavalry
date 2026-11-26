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
    CLUSTER_COMMIT_POSITION, COUNTERS_HELD, SUBSCRIPTION_POSITION,
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
        let _ = Command::new("kill")
            .arg("-CONT")
            .arg(self.0.id().to_string())
            .status();
        let _ = self.0.kill();
        let _ = self.0.wait();
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
