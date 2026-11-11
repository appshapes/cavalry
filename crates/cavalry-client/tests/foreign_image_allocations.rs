//! The allocation gate over an image that is announced to a client and is not its own
//! (CLAUDE.md, invariant 2 and gate 2; card 253). The driver announces every image to
//! every client of its directory, so a client's conductor reads `available image` for
//! its neighbours' subscriptions as often as they gain images, and it must allocate
//! nothing for them: every allocation gate that counts a process with two clients in
//! it, a cluster member's module and its service for one, otherwise fails whenever a
//! conductor reads a neighbour's image inside the window.
//!
//! The test plays the driver, so that nothing else in the process has work to do: it
//! creates the three files of a driver directory with the driver's sizes, keeps the
//! heartbeat, and transmits. A client connects, and `stats_alloc` counts while the
//! test announces two hundred images of a subscription the client does not hold, each
//! with the source identity `cavalry:ipc`, and then `client timeout` with the client's
//! id. The conductor reads the buffer in order, so the client closing with
//! `client-timeout` says that it has read every image before it, and the window ends
//! there. `stats_alloc` counts a `realloc` apart from an allocation, so the test adds
//! the two, as the bench's gate does (ADR 0047). Alone in its binary, so that no other
//! test's threads are counted (`cargo test` runs a binary's tests in parallel).
use std::alloc::System;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, Closed};
use cavalry_driver::spec;
use cavalry_proto::client_protocol::Event;
use cavalry_shm::broadcast::BroadcastBuffer;
use cavalry_shm::command_ring::CommandRing;
use cavalry_shm::counters::CountersFile;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// Images announced before the window, so that every buffer a first read grows has
/// grown.
const WARM_UP: usize = 20;
/// Images announced inside the window.
const IMAGES: usize = 200;
/// How long any wait in this test may last before it fails with a message.
const DEADLINE: Duration = Duration::from_secs(30);

/// The driver directory this test makes; it goes with the test.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "cavalry-foreign-image-allocations-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the driver directory is made");
        Self(dir)
    }
}

impl Drop for Dir {
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

#[test]
fn a_conductor_allocates_nothing_for_an_image_that_is_another_clients() {
    let dir = Dir::new();
    // The driver's three files, in the driver's order and with its sizes. The counters
    // file is held open for the client to read, and nothing is written to it.
    let _counters =
        CountersFile::create(&dir.0, spec::COUNTERS_CAPACITY).expect("the counters file");
    let ring = CommandRing::create(&dir.0, spec::COMMAND_RING_CAPACITY).expect("the ring");
    let consumer = ring
        .consumer(spec::COMMAND_RING_UNBLOCK_TIMEOUT)
        .expect("the ring's consumer");
    consumer.store_heartbeat(now_nanos());
    let broadcast =
        BroadcastBuffer::create(&dir.0, spec::BROADCAST_CAPACITY).expect("the broadcast buffer");
    let mut transmitter = broadcast.transmitter().expect("the transmitter");

    let client = Cavalry::connect_to(&dir.0).expect("the client connects");

    // An image of subscription 6, which this client never added: an IPC publication's,
    // so its source identity is `cavalry:ipc`, eleven bytes. Encoded once, here.
    let image = Event::AvailableImage {
        correlation_id: 7,
        session_id: 1,
        stream_id: 1001,
        subscriber_registration_id: 6,
        subscriber_position_counter_id: 14,
        log_file_name: b"publications/7.logbuffer",
        source_identity: b"cavalry:ipc",
    };
    let image_type = image.event_type() as i32;
    let mut image_bytes = [0u8; 128];
    let image_length = image.write(&mut image_bytes).expect("the image encodes");
    let timeout = Event::ClientTimeout {
        client_id: client.client_id(),
    };
    let timeout_type = timeout.event_type() as i32;
    let mut timeout_bytes = [0u8; 32];
    let timeout_length = timeout
        .write(&mut timeout_bytes)
        .expect("the timeout encodes");

    for _ in 0..WARM_UP {
        transmitter
            .transmit(image_type, &image_bytes[..image_length])
            .expect("the image is transmitted");
    }
    // The conductor idles with a backoff of a millisecond at most; fifty are enough
    // for it to have read the warm-up, and the window does not depend on it.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        client.closed(),
        None,
        "the client is open before the window"
    );

    let region = Region::new(GLOBAL);
    let mut transmitted = true;
    for _ in 0..IMAGES {
        consumer.store_heartbeat(now_nanos());
        transmitted &= transmitter
            .transmit(image_type, &image_bytes[..image_length])
            .is_ok();
        std::thread::sleep(Duration::from_micros(100));
    }
    transmitted &= transmitter
        .transmit(timeout_type, &timeout_bytes[..timeout_length])
        .is_ok();
    let started = Instant::now();
    while !client.is_closed() && started.elapsed() < DEADLINE {
        consumer.store_heartbeat(now_nanos());
        std::thread::sleep(Duration::from_millis(1));
    }
    let change = region.change();

    assert!(transmitted, "an event was not transmitted");
    assert_eq!(
        client.closed(),
        Some(Closed::ClientTimeout),
        "the conductor read the buffer up to its own client timeout"
    );
    assert_eq!(
        change.allocations + change.reallocations,
        0,
        "a conductor allocated for {IMAGES} images of a subscription that is not its \
         client's: {change:?}"
    );
}
