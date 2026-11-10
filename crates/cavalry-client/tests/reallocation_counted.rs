//! What the crate's allocation gates count, held as a test (CLAUDE.md, invariant 2 and
//! gate 2; card 211). Every allocation test of this crate opens a `stats_alloc` region
//! and asserts that `allocations + reallocations` is 0 over it. `stats_alloc` counts a
//! `realloc`, a buffer grown past a capacity it already had, in `reallocations` and not
//! in `allocations`, so a gate that read `allocations` alone passed a `Vec` or a
//! `String` that grew inside its window; the UDP gates read it that way until card 211.
//! The bench's gate counts a reallocation as an allocation (ADR 0047), so the sum is
//! what makes the tests and the bench agree on what an allocation is.
//!
//! The checks below pin the facts the gates rest on: a grown `Vec` is a reallocation
//! that `allocations` does not see, and the sum sees it. If `stats_alloc` ever counts
//! a `realloc` another way, the first check says so before a gate goes quiet.
//!
//! A program with no test harness (`harness = false` in the crate's `Cargo.toml`), so
//! that the process has one thread: a region counts every thread of the process, and
//! the harness's own thread allocates while it starts and reports tests, which a
//! window of one push cannot be told apart from. `cargo test` runs it and fails on a
//! panic; it prints one line when every check has passed.
use std::alloc::System;
use std::hint::black_box;

use stats_alloc::{INSTRUMENTED_SYSTEM, Region, Stats, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// What every allocation gate of the crate asserts is 0: the allocations and the
/// reallocations of a region, added.
fn counted(change: &Stats) -> usize {
    change.allocations + change.reallocations
}

/// The case card 211 names: a `Vec::with_capacity(1)` pushed past its capacity inside
/// a window. The allocator sees no allocation, only a reallocation, so a gate that
/// reads `allocations` alone passes it, and the sum the gates assert on does not.
fn a_vec_pushed_past_its_capacity_in_a_window_is_counted() {
    let mut grown: Vec<u64> = Vec::with_capacity(1);
    grown.push(1);
    black_box(&mut grown);
    let region = Region::new(GLOBAL);
    grown.push(2);
    let change = region.change();
    assert_eq!(*black_box(&grown), [1, 2]);
    assert_eq!(
        change.allocations, 0,
        "a push past a capacity the vector already had is no allocation to stats_alloc: \
         {change:?}"
    );
    assert_eq!(
        change.reallocations, 1,
        "a push past a capacity the vector already had is one reallocation: {change:?}"
    );
    assert_eq!(
        counted(&change),
        1,
        "the sum the allocation gates assert on sees the reallocation: {change:?}"
    );
}

/// The other half of the sum: a buffer grown from empty is an allocation, which the
/// gates counted before card 211 and still do.
fn a_vec_pushed_from_empty_in_a_window_is_counted() {
    let mut grown: Vec<u64> = Vec::new();
    black_box(&mut grown);
    let region = Region::new(GLOBAL);
    grown.push(1);
    let change = region.change();
    assert_eq!(*black_box(&grown), [1]);
    assert_eq!(
        (change.allocations, change.reallocations),
        (1, 0),
        "a push into an empty vector is one allocation and no reallocation: {change:?}"
    );
    assert_eq!(
        counted(&change),
        1,
        "the sum sees the allocation: {change:?}"
    );
}

/// A window with nothing to count counts nothing, so a gate's 0 means the path under
/// it was quiet and not that the count is off.
fn a_push_within_the_capacity_in_a_window_counts_nothing() {
    let mut sized: Vec<u64> = Vec::with_capacity(2);
    sized.push(1);
    black_box(&mut sized);
    let region = Region::new(GLOBAL);
    sized.push(2);
    let change = region.change();
    assert_eq!(*black_box(&sized), [1, 2]);
    assert_eq!(
        counted(&change),
        0,
        "a push within the capacity touches no allocator: {change:?}"
    );
}

fn main() {
    a_vec_pushed_past_its_capacity_in_a_window_is_counted();
    a_vec_pushed_from_empty_in_a_window_is_counted();
    a_push_within_the_capacity_in_a_window_counts_nothing();
    println!(
        "reallocation_counted: a reallocation, an allocation and a quiet window each \
         counted as the allocation gates read them"
    );
}
