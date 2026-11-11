//! The allocation gate over a replay that follows a live recording (CLAUDE.md,
//! invariant 2 and gate 2; card 227): a publisher whose stream the driver records, a
//! replay of that recording with the length that follows it, and a subscriber on the
//! replay, with the driver's threads in this process under the counting allocator the
//! bench uses. The window is three seconds, so that the recorder syncs and the
//! replayer reads the recording's catalog slot again, each once a second, several
//! times, with messages published, recorded, replayed and read all the while, and no
//! allocation at all. The catalog read is why this gate exists: it built the
//! descriptor's two strings each time, two allocations a second on the replayer's
//! thread for every replay that follows a recording (ADR 0030 for the replay, ADR 0036
//! for the catalog read once a second). Alone in its binary, so that no other test's
//! threads are counted.

use std::alloc::System;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, FOLLOW_LENGTH, Offer};
use cavalry_driver::Driver;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// How long the counted window lasts: three of the replayer's catalog reads and three
/// of the recorder's syncs at the least.
const WINDOW: Duration = Duration::from_millis(3_200);

/// How long any wait in this test may last before it fails with a message.
const DEADLINE: Duration = Duration::from_secs(20);

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-client-replay-follow-alloc-{}-{test}",
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

#[test]
fn a_replay_that_follows_a_live_recording_allocates_nothing() {
    let dir = TestDir::new("follow");
    let driver_dir = dir.path().join("driver");
    let log_dir = dir.path().join("log");
    let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let mut client = Cavalry::connect_to(&driver_dir).expect("the client connects");
    let mut publication = client
        .add_publication("cavalry:ipc", 1001)
        .expect("the publication is added");
    let recording = client
        .start_recording("cavalry:ipc", 1001)
        .expect("the recording starts");
    let mut subscription = client
        .add_subscription("cavalry:ipc", 2002)
        .expect("the subscription is added");
    let replay = client
        .start_replay(
            recording.recording_id(),
            0,
            FOLLOW_LENGTH,
            2002,
            "cavalry:ipc",
        )
        .expect("the replay starts");
    let session = replay.session_id();

    // One message a cycle and one poll, the publisher paced so that the stream does
    // not outrun the recorder's sync by more than the replay log holds.
    let message = [7u8; 32];
    let mut offered = 0u64;
    let mut read = 0u64;
    let mut cycle = |offered: &mut u64, read: &mut u64| -> bool {
        match publication.offer(&message) {
            Ok(_) => *offered += 1,
            Err(Offer::BackPressured | Offer::AdminAction | Offer::NotConnected) => {}
            Err(_) => return false,
        }
        match subscription.poll(
            |_, header| {
                if header.session_id() == session {
                    *read += 1;
                }
            },
            64,
        ) {
            Ok(_) => {}
            Err(_) => return false,
        }
        std::thread::sleep(Duration::from_micros(200));
        true
    };

    // Before the count: the replay's image has joined, a sync has passed and messages
    // have gone the whole way round, so that every path has run once.
    let deadline = Instant::now() + DEADLINE;
    let warm = Instant::now() + Duration::from_millis(1_500);
    while read < 100 || Instant::now() < warm {
        assert!(
            Instant::now() < deadline,
            "the replay delivered {read} of {offered}"
        );
        assert!(cycle(&mut offered, &mut read), "an offer or a poll failed");
    }

    let (offered_before, read_before) = (offered, read);
    let end = Instant::now() + WINDOW;
    let mut failed = false;
    let region = Region::new(GLOBAL);
    while Instant::now() < end && !failed {
        failed = !cycle(&mut offered, &mut read);
    }
    let change = region.change();
    assert!(!failed, "an offer or a poll failed in the window");
    assert!(
        offered - offered_before > 1_000 && read - read_before > 1_000,
        "{} offered and {} read in the window",
        offered - offered_before,
        read - read_before
    );
    assert_eq!(
        replay.ended(),
        None,
        "the replay still follows the recording"
    );
    assert_eq!(
        (change.allocations, change.reallocations),
        (0, 0),
        "a replay that follows a live recording allocated: {change:?}"
    );

    drop(cycle);
    replay.stop(&mut client).expect("the replay stops");
    subscription.close().expect("the subscription closes");
    publication.close().expect("the publication closes");
    drop(recording);
    client.close().expect("the client closes");
    driver.close().expect("the driver closes");
}
