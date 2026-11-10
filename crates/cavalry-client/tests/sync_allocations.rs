//! The allocation gate over a sync asked for without a wait (CLAUDE.md, invariant 2 and
//! gate 2; ADR 0067): a driver, a client with a publication and its recording, and
//! `stats_alloc` over two hundred rounds of one message offered, the recorder writing
//! it, `Recording::request_sync`, and the synced position reaching it, with no
//! allocation and no reallocation at all, in any thread of the process: the client's
//! call and its conductor, which drops the answer, and the driver's conductor and
//! recorder, which hand the request over, sync and answer. A cluster member's
//! consensus module makes this call in its duty cycle, which is why it is held to the
//! data plane's rule. `stats_alloc` counts a `realloc` apart from an allocation, so the
//! test adds the two, as the bench's gate does (ADR 0047). Alone in its binary, so that
//! no other test's threads are counted (`cargo test` runs a binary's tests in parallel).
use std::alloc::System;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use cavalry_client::Cavalry;
use cavalry_driver::Driver;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// Rounds before the window, so that every buffer a first use grows has grown.
const WARM_UP: i64 = 50;
/// Rounds inside the window.
const ROUNDS: i64 = 200;
/// How long any wait in this test may last before it fails with a message.
const DEADLINE: Duration = Duration::from_secs(30);

/// The driver's directory and, beside it, its log directory; both go with the test.
struct Dirs {
    dir: PathBuf,
    log_dir: PathBuf,
}

impl Dirs {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("cavalry-sync-allocations-{}", std::process::id()));
        let log_dir = dir.with_extension("log");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&log_dir);
        Self { dir, log_dir }
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        let _ = std::fs::remove_dir_all(&self.log_dir);
    }
}

/// Spins until `done`, allocating nothing; panics with `what` after [`DEADLINE`].
fn spin_until(what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < DEADLINE, "{what}");
        std::hint::spin_loop();
    }
}

#[test]
fn a_sync_requested_without_waiting_allocates_nothing_in_the_client_or_the_driver() {
    let dirs = Dirs::new();
    let driver = Driver::start(&dirs.dir, &dirs.log_dir).expect("the driver");
    let mut client = Cavalry::connect_to(&dirs.dir).expect("the client");
    let mut publication = client
        .add_exclusive_publication("cavalry:ipc", 71)
        .expect("a publication");
    let recording = client
        .start_recording("cavalry:ipc", 71)
        .expect("a recording");
    let message = [7u8; 32];
    let mut round = |client: &mut Cavalry| {
        let mut after = 0;
        spin_until("the offer goes through", || {
            match publication.offer(&message) {
                Ok(position) => {
                    after = position;
                    true
                }
                Err(_) => false,
            }
        });
        spin_until("the recorder writes the message", || {
            recording.position() == after
        });
        // One request is enough: the recorder had written the message before the
        // request was sent, and it syncs what it has written when it takes a request.
        recording
            .request_sync(client)
            .expect("the command is written");
        spin_until("the synced position reaches the message", || {
            recording.synced_position() == after
        });
        after
    };
    for _ in 0..WARM_UP {
        round(&mut client);
    }
    let region = Region::new(GLOBAL);
    let mut last = 0;
    for _ in 0..ROUNDS {
        last = round(&mut client);
    }
    let change = region.change();
    assert_eq!(last, (WARM_UP + ROUNDS) * 64, "every message was recorded");
    assert_eq!(recording.synced_position(), last);
    drop(publication);
    client.close().expect("the client's close");
    driver.close().expect("the driver's close");
    assert_eq!(
        change.allocations + change.reallocations,
        0,
        "{ROUNDS} rounds of a message, a sync asked for without a wait and the synced \
         position reaching it allocated or reallocated: {change:?}; see \
         .claude/rules/data-plane.md"
    );
}
