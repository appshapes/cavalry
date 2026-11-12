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
use std::path::Path;
use std::ptr;
use std::time::{Duration, Instant};

use cavalry_driver::Driver;
use cavalry_ffi::*;
use cavalry_raft::{ConsensusModule, ElectionState, Role};
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

// A cluster's ports, from the one file that says how they are taken and why, which
// is `cavalry-raft`'s: `crates/cavalry-raft/tests/ports/mod.rs`.
#[path = "../../cavalry-raft/tests/ports/mod.rs"]
mod ports;
use ports::free_ports;

/// Three cluster members over three drivers on the loopback, their elections closed,
/// the member that leads first and its followers after it, whichever member the
/// cluster has for a leader (ADR 0100, change 3). They start in an order that does
/// not put the lowest id first.
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
    let mut members: Vec<(Driver, ConsensusModule)> = [1, 0, 2]
        .into_iter()
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
    let leader = members
        .iter()
        .position(|(_, module)| module.role() == Role::Leader)
        .expect("one member leads");
    members.swap(0, leader);
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
    // The service reads beside a follower, `members[1]`, in that member's directory.
    let follower = format!("member-{}", members[1].1.member_id());
    let member_dir = CString::new(cluster_dir.join(follower).to_str().unwrap()).unwrap();
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
