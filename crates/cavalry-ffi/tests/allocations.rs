//! The allocation gate over the ABI's message path (CLAUDE.md, invariant 2 and gate
//! 2; ADR 0051): every per-message function, `cavalry_publication_try_claim`,
//! `cavalry_buffer_claim_set_reserved_value`, `cavalry_buffer_claim_commit`,
//! `cavalry_buffer_claim_abort`, `cavalry_publication_offer`,
//! `cavalry_subscription_poll` and `cavalry_header_values`, counted with
//! `stats_alloc`, allocations and reallocations both, over a window after the image
//! has joined, with none at all. Alone in its binary, so that no other test's threads
//! are counted.

use std::alloc::System;
use std::ffi::{CString, c_void};
use std::ptr;

use cavalry_driver::Driver;
use cavalry_ffi::*;
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

#[test]
fn the_message_path_allocates_nothing() {
    let dir = std::env::temp_dir().join(format!("cavalry-ffi-allocations-{}", std::process::id()));
    let log_dir = dir.with_extension("log");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&log_dir);
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
    }
    driver.close().expect("the driver closes");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&log_dir);
}
