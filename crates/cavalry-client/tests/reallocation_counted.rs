//! What the workspace's allocation gates count, held as a test (CLAUDE.md, invariant 2
//! and gate 2; card 211). Every allocation test opens a `stats_alloc` region and fails
//! when the region's `allocations` or its `reallocations` is above 0. `stats_alloc`
//! counts a `realloc`, a buffer grown past a capacity it already had, in
//! `reallocations` and not in `allocations`, so a gate that read `allocations` alone
//! passed a `Vec` or a `String` that grew inside its window; the UDP gates read it
//! that way until card 211. The bench's gate counts a reallocation as an allocation
//! (ADR 0047), so reading both is what makes the tests and the bench agree on what an
//! allocation is, and `just lint` refuses a test that reads one without the other
//! (`scripts/lint-workspace.py`, the allocations check).
//!
//! The checks below hold the counting allocator to what the gates rest on: a buffer
//! grown inside a window is counted, whether it had a capacity or not, and a window
//! with no growth in it counts nothing. If `stats_alloc` ever stops counting a
//! `realloc`, the first check says so before a gate goes quiet.
//!
//! A program with no test harness (`harness = false` in the crate's `Cargo.toml`), so
//! that the process has one thread: a region counts every thread of the process, and
//! the harness's own thread allocates while it starts and reports tests, which could
//! stand in for the count a check expects. `cargo test` runs it and fails on a panic;
//! it prints one line when every check has passed, and it takes no filter.
use std::alloc::System;
use std::hint::black_box;

use stats_alloc::{INSTRUMENTED_SYSTEM, Region, Stats, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// What every allocation gate asserts is 0: the allocations and the reallocations of
/// a region, added.
fn counted(change: &Stats) -> usize {
    change.allocations + change.reallocations
}

/// The case card 211 names: a `Vec::with_capacity(1)` pushed past its capacity inside
/// a window. `with_capacity` may give more than it is asked for, so the vector is
/// filled to the capacity it has before the window opens, and the one push inside the
/// window is the one that grows it. With `stats_alloc` 0.1.10 the window reads 0
/// allocations and 1 reallocation, which is why `allocations` alone is not the count.
fn a_vec_pushed_past_its_capacity_in_a_window_is_counted() {
    let mut grown: Vec<u64> = Vec::with_capacity(1);
    while grown.len() < grown.capacity() {
        grown.push(1);
    }
    black_box(&mut grown);
    let filled = grown.len();
    let region = Region::new(GLOBAL);
    grown.push(2);
    let change = region.change();
    assert_eq!(black_box(&grown).len(), filled + 1);
    assert!(
        counted(&change) >= 1,
        "a push past a vector's capacity inside a window was not counted, so an \
         allocation gate would pass a buffer that grows per message: {change:?}"
    );
}

/// The other half: a buffer grown from empty is an allocation, which the gates
/// counted before card 211 and still do.
fn a_vec_pushed_from_empty_in_a_window_is_counted() {
    let mut grown: Vec<u64> = Vec::new();
    black_box(&mut grown);
    let region = Region::new(GLOBAL);
    grown.push(1);
    let change = region.change();
    assert_eq!(black_box(&grown).len(), 1);
    assert!(
        counted(&change) >= 1,
        "a push into an empty vector inside a window was not counted: {change:?}"
    );
}

/// A window with no growth in it counts nothing, so a gate's 0 means the path under
/// it was quiet and not that the count is off. Tried a few times, so that a thread
/// the process was given from outside, a sanitizer's or a profiler's, has to allocate
/// in every window to fail it, and the message then says so.
fn a_push_within_the_capacity_in_a_window_counts_nothing() {
    const TRIES: usize = 8;
    let mut last = None;
    for _ in 0..TRIES {
        let mut sized: Vec<u64> = Vec::with_capacity(2);
        sized.push(1);
        black_box(&mut sized);
        assert!(sized.len() < sized.capacity());
        let region = Region::new(GLOBAL);
        sized.push(2);
        let change = region.change();
        assert_eq!(black_box(&sized).len(), 2);
        if counted(&change) == 0 {
            return;
        }
        last = Some(change);
    }
    panic!(
        "a push within a vector's capacity was counted in each of {TRIES} windows, so \
         another thread of this process allocates and no window here can be read: \
         {last:?}"
    );
}

fn main() {
    a_vec_pushed_past_its_capacity_in_a_window_is_counted();
    a_vec_pushed_from_empty_in_a_window_is_counted();
    a_push_within_the_capacity_in_a_window_counts_nothing();
    println!(
        "reallocation_counted: a buffer grown past its capacity and one grown from \
         empty are each counted in a window, and a window with no growth counts nothing"
    );
}
