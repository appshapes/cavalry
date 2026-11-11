//! The allocation gate over the UDP receive path (CLAUDE.md, invariant 2 and gate 2; ADR
//! 0042): a publisher and a subscriber on one driver's `cavalry:udp` loopback, the
//! driver's five threads in this process, under `stats_alloc`, with ten thousand
//! messages crossing the wire into an image and out to the subscriber and no allocation
//! and no reallocation at all. `stats_alloc` counts a `realloc`, a buffer grown past a
//! capacity it already had, apart from an allocation, so the test adds the two: it
//! counts what the bench's gate counts (ADR 0047; card 211), and
//! `tests/reallocation_counted.rs` shows the sum sees a `Vec` that grows. Alone in its
//! binary, so that no other test's threads are counted (`cargo test` runs a binary's
//! tests in parallel).

use std::alloc::System;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, DataFrameHeader, Offer, Publication, Subscription};
use cavalry_driver::Driver;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

// This test's ports, from the one file that says how they are taken and why, which is
// `cavalry-raft`'s: `crates/cavalry-raft/tests/ports/mod.rs`.
#[path = "../../cavalry-raft/tests/ports/mod.rs"]
mod ports;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// The messages the gate sends: ten rounds of a thousand, each 32 bytes in a 64-byte
/// frame, every round drained by the subscriber before the next.
const ROUNDS: usize = 10;
const MESSAGES_PER_ROUND: u64 = 1000;

/// How long any wait in this test may last before it fails with a message.
const DEADLINE: Duration = Duration::from_secs(10);

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-client-udp-receive-alloc-{}-{test}",
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

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while !condition() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

/// Offers `message` until the publication takes it, within [`DEADLINE`]. Nothing here
/// allocates while the offer is taken or refused.
fn offer(publication: &mut Publication, message: &[u8]) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        match publication.offer(message) {
            Ok(_) => return,
            Err(Offer::NotConnected | Offer::BackPressured | Offer::AdminAction) => {
                assert!(
                    Instant::now() < deadline,
                    "the offer was refused for {DEADLINE:?}"
                );
                std::thread::sleep(Duration::from_micros(50));
            }
            Err(other) => panic!("{other}"),
        }
    }
}

/// Polls until `count` messages of this round have arrived, in order from `first`:
/// with `Subscription::poll`, or, when `session` names the image, with the bounded
/// poll of that image under the largest limit (ADR 0066). Nothing here allocates.
fn drain(subscription: &mut Subscription, first: u64, count: u64, session: Option<i32>) {
    let deadline = Instant::now() + DEADLINE;
    let mut next = first;
    let end = first + count;
    while next < end {
        let handler = |body: &[u8], _: &DataFrameHeader| {
            let mut sequence = [0u8; 8];
            sequence.copy_from_slice(&body[..8]);
            assert_eq!(u64::from_le_bytes(sequence), next);
            next += 1;
        };
        let delivered = match session {
            None => subscription.poll(handler, 64),
            Some(session) => subscription.bounded_poll(session, handler, i64::MAX, 64),
        }
        .expect("the poll reads");
        if delivered == 0 {
            assert!(
                Instant::now() < deadline,
                "the stream stopped at {next} of {end}"
            );
            std::thread::sleep(Duration::from_micros(50));
        }
    }
}

/// Nothing on the receive path allocates (CLAUDE.md, invariant 2): with the publisher,
/// the driver's five threads and the subscriber in this process, ten thousand messages
/// cross the loopback into an image under the counting allocator with no allocation
/// and no reallocation at all.
#[test]
fn the_receive_path_allocates_nothing_per_message() {
    let dir = TestDir::new("allocations");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let mut client = Cavalry::connect_to(dir.path()).expect("connects");
    let port = ports::free_port();
    let channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
    let mut subscription = client
        .add_subscription(&channel, 7)
        .expect("a subscription");
    let mut publication = client.add_publication(&channel, 7).expect("a publication");
    assert!(wait_until(DEADLINE, || publication.is_connected()));
    assert!(wait_until(DEADLINE, || {
        subscription.poll(|_, _| {}, 0).expect("the poll applies");
        subscription.image_count() == 1
    }));
    // Warm up: the first round, every thread's first cycle, the first status messages,
    // and a quiet second for the client's first keepalive and every first-time path.
    for i in 0..MESSAGES_PER_ROUND {
        let mut message = [0u8; 32];
        message[..8].copy_from_slice(&i.to_le_bytes());
        offer(&mut publication, &message);
    }
    drain(&mut subscription, 0, MESSAGES_PER_ROUND, None);
    let session = subscription.images()[0].session_id();
    std::thread::sleep(Duration::from_millis(1200));

    // Ten rounds of a thousand, each drained before the next; nothing in the region
    // allocates or grows a buffer: not the publisher, not the conductor, not the
    // sender's bursts, not the receiver's polls, inserts, scans and status messages,
    // and not the subscriber, which reads every other round with the bounded poll.
    let region = Region::new(GLOBAL);
    let mut sequence = MESSAGES_PER_ROUND;
    for round in 0..ROUNDS {
        for _ in 0..MESSAGES_PER_ROUND {
            let mut message = [0u8; 32];
            message[..8].copy_from_slice(&sequence.to_le_bytes());
            offer(&mut publication, &message);
            sequence += 1;
        }
        drain(
            &mut subscription,
            sequence - MESSAGES_PER_ROUND,
            MESSAGES_PER_ROUND,
            (round % 2 == 1).then_some(session),
        );
    }
    std::thread::sleep(Duration::from_millis(250));
    let change = region.change();
    assert_eq!(
        change.allocations + change.reallocations,
        0,
        "the receive path allocated or reallocated over {} messages: {change:?}; see \
         docs/how-to/run-the-bench.md, \"When it does not work\"",
        ROUNDS as u64 * MESSAGES_PER_ROUND
    );
    publication.close().expect("closes");
    subscription.close().expect("closes");
    client.close().expect("closes");
    driver.close().expect("the driver closes");
}
