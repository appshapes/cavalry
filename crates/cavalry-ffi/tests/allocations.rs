//! The allocation gate over the ABI's message path (CLAUDE.md, invariant 2 and gate
//! 2; ADR 0051): every per-message function, `cavalry_publication_try_claim`,
//! `cavalry_buffer_claim_set_reserved_value`, `cavalry_buffer_claim_commit`,
//! `cavalry_buffer_claim_abort`, `cavalry_publication_offer`,
//! `cavalry_subscription_poll`, `cavalry_header_values`,
//! `cavalry_committed_log_poll` and `cavalry_committed_log_entry_position`, counted
//! with `stats_alloc`, allocations and reallocations both, over a window after the
//! image has joined, with none at all. The committed log's two have a window of their
//! own, beside three cluster members in this process whose duty cycles run inside it
//! (ADR 0070). Alone in its binary, so that no other test's threads are counted.

use std::alloc::System;
use std::ffi::{CString, c_void};
use std::net::UdpSocket;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use cavalry_driver::Driver;
use cavalry_ffi::*;
use cavalry_raft::{ConsensusModule, ElectionState};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

static mut SEEN: usize = 0;

unsafe extern "C" fn count(
    _clientd: *mut c_void,
    _buffer: *const u8,
    _length: usize,
    header: *const cavalry_header_t,
) {
    // SAFETY: the header is valid for the call; the counter is this test's, on one
    // thread.
    unsafe {
        let mut values = std::mem::zeroed::<cavalry_header_values_t>();
        assert_eq!(cavalry_header_values(header, &mut values), 0);
        SEEN += 1;
    }
}

static mut ENTRIES: usize = 0;

unsafe extern "C" fn count_entry(
    _clientd: *mut c_void,
    _buffer: *const u8,
    _length: usize,
    header: *const cavalry_header_t,
) {
    // SAFETY: the header is valid for the call; the counter is this test's, on one
    // thread.
    unsafe {
        let mut values = std::mem::zeroed::<cavalry_header_values_t>();
        assert_eq!(cavalry_header_values(header, &mut values), 0);
        assert!(cavalry_committed_log_entry_position(header) >= 0);
        ENTRIES += 1;
    }
}

/// Loopback ports for a cluster's endpoints, from 20000 to 31999, as `cavalry-raft`'s
/// `tests/cluster.rs` takes its own and for its reason: below the range the operating
/// system gives a socket bound to port 0, which starts at 32768 on Linux and at 49152
/// on macOS. A follower binds its `log` endpoint only when it joins the log, and
/// meanwhile every publication of the drivers in this process has a send socket bound
/// to port 0, which could be given a port that was chosen by binding port 0 and
/// releasing it: the member's bind then fails with `Address already in use`, and it
/// never joins. Each port is free when it is chosen, and each is chosen once in this
/// process, so that tests that run at once never share one. The process id picks
/// where the choice starts, so that two test processes on one host start apart.
fn free_ports(count: usize) -> Vec<u16> {
    const FIRST: u32 = 20_000;
    const SPAN: u32 = 12_000;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let start = (std::process::id() % 60) * 200;
    let mut sockets = Vec::with_capacity(count);
    for _ in 0..SPAN {
        if sockets.len() == count {
            break;
        }
        let offset = (start + NEXT.fetch_add(1, Ordering::Relaxed)) % SPAN;
        let port = u16::try_from(FIRST + offset).expect("below 32000");
        if let Ok(socket) = UdpSocket::bind(("127.0.0.1", port)) {
            sockets.push(socket);
        }
    }
    assert_eq!(
        sockets.len(),
        count,
        "free loopback ports from {FIRST} to {}",
        FIRST + SPAN - 1
    );
    sockets
        .iter()
        .map(|socket| socket.local_addr().expect("bound").port())
        .collect()
}

/// Three cluster members over three drivers on the loopback, their elections closed,
/// member 0 the leader.
fn cluster(dir: &Path) -> Vec<(Driver, ConsensusModule)> {
    let ports = free_ports(15);
    let members_string = (0..3)
        .map(|member| {
            let endpoints: Vec<String> = ports[member * 5..member * 5 + 5]
                .iter()
                .map(|port| format!("127.0.0.1:{port}"))
                .collect();
            format!("{member},{}", endpoints.join(","))
        })
        .collect::<Vec<_>>()
        .join("|");
    let mut members: Vec<(Driver, ConsensusModule)> = (0..3)
        .map(|id| {
            let driver_dir = dir.join(format!("member-{id}"));
            let log_dir = dir.join(format!("member-{id}-log"));
            let driver = Driver::start(&driver_dir, &log_dir).expect("the member's driver");
            let module = ConsensusModule::start(&driver_dir, &log_dir, &members_string, id)
                .expect("the member");
            (driver, module)
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !members
        .iter()
        .all(|(_, module)| module.election_state() == ElectionState::Closed)
    {
        assert!(Instant::now() < deadline, "the elections closed");
        work(&mut members);
    }
    members
}

fn work(members: &mut [(Driver, ConsensusModule)]) {
    for (_, module) in members.iter_mut() {
        module.do_work().expect("the member's duty cycle");
    }
}

#[test]
fn the_message_path_allocates_nothing() {
    let dir = std::env::temp_dir().join(format!("cavalry-ffi-allocations-{}", std::process::id()));
    let log_dir = dir.with_extension("log");
    let cluster_dir = dir.with_extension("cluster");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&log_dir);
    let _ = std::fs::remove_dir_all(&cluster_dir);
    // The cluster starts before either window, so that neither holds a start.
    let mut members = cluster(&cluster_dir);
    let member_dir = CString::new(cluster_dir.join("member-1").to_str().unwrap()).unwrap();
    let driver = Driver::start(&dir, &log_dir).expect("the driver");
    let dir_c = CString::new(dir.to_str().unwrap()).unwrap();
    let channel = c"cavalry:ipc";
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        let mut context = ptr::null_mut();
        assert_eq!(cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION), 0);
        assert_eq!(cavalry_context_set_dir(context, dir_c.as_ptr()), 0);
        let mut client = ptr::null_mut();
        assert_eq!(cavalry_init(&mut client, context), 0);
        let mut subscription = ptr::null_mut();
        assert_eq!(
            cavalry_add_subscription(&mut subscription, client, channel.as_ptr(), 1001),
            0
        );
        let mut publication = ptr::null_mut();
        assert_eq!(
            cavalry_add_publication(&mut publication, client, channel.as_ptr(), 1001),
            0
        );
        let mut claim = std::mem::zeroed::<cavalry_buffer_claim_t>();
        // Warm up: the image joins, the first frames cross, and the subscription has
        // applied its events, so the window below holds the message path alone.
        let mut warmed = 0;
        while warmed < 100 {
            let position = cavalry_publication_try_claim(publication, 32, &mut claim);
            if position >= 0 {
                assert_eq!(cavalry_buffer_claim_commit(&mut claim), position);
                warmed += 1;
            }
            assert!(cavalry_subscription_poll(subscription, Some(count), ptr::null_mut(), 16) >= 0);
        }
        while SEEN < 100 {
            assert!(cavalry_subscription_poll(subscription, Some(count), ptr::null_mut(), 16) >= 0);
        }
        let before = SEEN;
        let region = Region::new(GLOBAL);
        for i in 0..10_000u32 {
            let position = cavalry_publication_try_claim(publication, 32, &mut claim);
            if position < 0 {
                assert_ne!(position, CAVALRY_PUBLICATION_ERROR);
                // Back pressure: drain and try again, inside the window.
                assert!(
                    cavalry_subscription_poll(subscription, Some(count), ptr::null_mut(), 64) >= 0
                );
                continue;
            }
            assert_eq!(
                cavalry_buffer_claim_set_reserved_value(&mut claim, i64::from(i)),
                0
            );
            if i % 7 == 0 {
                assert_eq!(cavalry_buffer_claim_abort(&mut claim), position);
            } else {
                assert_eq!(cavalry_buffer_claim_commit(&mut claim), position);
            }
            let message = [i as u8; 32];
            let offered = cavalry_publication_offer(publication, message.as_ptr(), message.len());
            if offered < 0 {
                assert_ne!(offered, CAVALRY_PUBLICATION_ERROR);
            }
            assert!(cavalry_subscription_poll(subscription, Some(count), ptr::null_mut(), 8) >= 0);
        }
        while cavalry_subscription_poll(subscription, Some(count), ptr::null_mut(), 64) > 0 {}
        let change = region.change();
        let delivered = SEEN - before;
        assert_eq!(cavalry_publication_close(publication), 0);
        assert_eq!(cavalry_subscription_close(subscription), 0);
        assert_eq!(cavalry_close(client), 0);
        assert_eq!(cavalry_context_close(context), 0);
        assert!(
            delivered > 10_000,
            "the window delivered {delivered} fragments"
        );
        assert_eq!(
            change.allocations + change.reallocations,
            0,
            "the message path allocated across {delivered} fragments, {change:?}; see \
             .claude/rules/data-plane.md"
        );

        // The committed log's window: a service through the ABI beside member 1, a
        // follower. The leader appends an entry, the members run, and the C handler
        // is handed what a quorum committed, with its header's values and its
        // position, two thousand times.
        let mut context = ptr::null_mut();
        assert_eq!(cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION), 0);
        assert_eq!(cavalry_context_set_dir(context, member_dir.as_ptr()), 0);
        let mut client = ptr::null_mut();
        assert_eq!(cavalry_init(&mut client, context), 0);
        // A follower's recording starts with the leader's first frame, which can be
        // after its election closes.
        let deadline = Instant::now() + Duration::from_secs(60);
        while members[1].1.log_recording_id().is_none() {
            assert!(Instant::now() < deadline, "member 1 recorded the log");
            work(&mut members);
        }
        let mut log = ptr::null_mut();
        assert_eq!(cavalry_committed_log(&mut log, client, 0), 0);
        assert!(!log.is_null(), "member 1 has a recording of the log");
        let payload = [9u8; 8];
        let mut offered = 0;
        // Warm up: the replay's image joins and the first entries cross.
        let deadline = Instant::now() + Duration::from_secs(60);
        while ENTRIES < 100 {
            assert!(
                Instant::now() < deadline,
                "the service read its first entries"
            );
            if offered < 100 && members[0].1.offer(&payload).is_ok() {
                offered += 1;
            }
            work(&mut members);
            assert!(cavalry_committed_log_poll(log, Some(count_entry), ptr::null_mut(), 16) >= 0);
        }
        let before = ENTRIES;
        let region = Region::new(GLOBAL);
        let mut offered = 0;
        let deadline = Instant::now() + Duration::from_secs(120);
        while ENTRIES - before < 2_000 {
            assert!(
                Instant::now() < deadline,
                "the service read two thousand entries"
            );
            if offered < 2_000 && members[0].1.offer(&payload).is_ok() {
                offered += 1;
            }
            work(&mut members);
            assert!(cavalry_committed_log_poll(log, Some(count_entry), ptr::null_mut(), 16) >= 0);
        }
        let change = region.change();
        let read = ENTRIES - before;
        assert_eq!(cavalry_committed_log_close(log, client), 0);
        assert_eq!(cavalry_close(client), 0);
        assert_eq!(cavalry_context_close(context), 0);
        assert_eq!(
            change.allocations + change.reallocations,
            0,
            "the committed log's path allocated across {read} entries, {change:?}; see \
             .claude/rules/data-plane.md"
        );
    }
    driver.close().expect("the driver closes");
    drop(members);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&log_dir);
    let _ = std::fs::remove_dir_all(&cluster_dir);
}
