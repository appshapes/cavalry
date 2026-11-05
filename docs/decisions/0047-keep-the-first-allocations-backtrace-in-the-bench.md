# 0047. Count the measured loops' allocations with a hook that keeps the first one's backtrace

- **Status:** Accepted
- **Date:** 2026-11-05
- **Invariants affected:** 2, 6
- **Supersedes:** none; replaces the counting allocator ADR 0015's Decision named,
  `stats_alloc`, with one that has a hook and counts a reallocation as an allocation,
  and changes nothing else of ADR 0015

## Context

The allocation gate of `CLAUDE.md`'s gate 2 is the harness's: the measured loops run
under a counting allocator and the run fails when they allocate (ADR 0015). The count
was `stats_alloc`'s, read as the change over a region around each measured loop, and
a failure said how many allocations and which benchmark's loops: `the measured loops
allocated 1 times, by benchmark udp 1`. It could not say where.

Card 176: the `gates (macos)` job's `just bench --messages 5000000` ends with
`allocations per message: 0.00000005 (1 in 20000000 messages)` on some runs and 0 on
others, with no change in between, and a re-run of the same commit passes. Seen on
2026-10-31 on pull request 77's head 2f82ec2 and on `master` at 068647d (run
41204043241), and on 2026-11-04 at a718d63; never on `gates (linux)`, `bench (linux)`
or `bench (grip)`; never on a laptop. One allocation in twenty million is a path a
measured loop takes once per process on that runner, a first-time format, a vector
past its capacity, a `std` call that allocates on macOS and not on Linux, or another
thread's allocation inside the window, and the count alone cannot tell them apart.
The card's first step is to make the allocator say where: the backtrace of the first
allocation it counts inside a measured loop, printed with the failure line.

`stats_alloc` has no hook: it counts and nothing else. A counting allocator of the
harness's own is an `unsafe` `GlobalAlloc` impl, which the rule keeps to `cavalry-shm`
and `cavalry-ffi` (`CLAUDE.md`, Unsafe), the reason ADR 0015 took a crate. The
`tracking-allocator` crate, version 0.4.0, read on 2026-11-05 from its source on
docs.rs, is a `GlobalAlloc` wrapper that calls a tracker for every allocation while
tracking is enabled, process-wide, through `enable_tracking` and `disable_tracking`;
the tracker's `allocated` is called with the address, the sizes and the thread's
allocation group, the root group when none was entered, and "all
allocations/deallocations that occur within the call to `AllocationTracker::allocated`
are ignored", so a tracker may allocate. With tracking disabled, `alloc` reads one
atomic and takes no thread-local. Its `tracing` features are optional and off here.

The two crates do not count the same calls. `stats_alloc` 0.1.10 counts `alloc` and
`alloc_zeroed` in `allocations` and `realloc` in `reallocations`, and the harness read
`allocations` alone. A `Vec` or `String` grown past a capacity it already had grows
through `realloc`, so the old count missed it, and counted only a growth from empty:
"a vector past its capacity", one of card 176's four causes, was mostly invisible to
the gate. `tracking-allocator` 0.4.0 implements `alloc` and `dealloc` and nothing
else of `GlobalAlloc`. The trait's default `realloc` allocates anew through `alloc`,
copies and frees, so every reallocation reaches the hook, a grow or a shrink; its
default `alloc_zeroed` is `alloc` and then a write of zeros, in place of `System`'s
`calloc`.

## Decision

The harness's global allocator is `tracking_allocator::Allocator<System>`, with a
tracker of the harness's own, `CountingTracker`, set once in `main`. A measured loop
runs inside a `Counting` window, which enables tracking when the warm-up ends and
disables it when the loop ends, and reads the window's count from a process-wide
counter the tracker increments; every thread's allocations count while a window is
open, the driver's and the subscriber's included, as before. A reallocation counts as
one allocation, which it did not before: the gate's rule, zero allocations per
message, is unchanged, and `.claude/rules/data-plane.md` names `Vec::push` among the
allocations it forbids, so the count now sees the growth the rule always covered. A
run whose loops reallocate and never allocate from empty passed the gate under
`stats_alloc` and fails it now. On the first allocation
the tracker counts, once per process, it captures `std::backtrace::Backtrace` and
keeps it; the failure line that ends a run with a non-zero count is followed by that
backtrace, frame by frame from the allocation to the thread's entry, so the next
failing run names the frame and the thread. The release build keeps symbol names and
not line numbers, which is enough to name the function. The backtrace is captured on
the failure path alone: a run that counts zero captures none.

## Alternatives considered

- **Keep `stats_alloc` and find the allocation by elimination.** Lost: three runs in
  five days on one runner, never reproduced on a laptop, and the count cannot say
  which of four causes it was; the gate is right to fail, and the next failure should
  carry its own answer.
- **A counting allocator of the harness's own, with the capture inside it.** Lost for
  ADR 0015's reason: an `unsafe` `GlobalAlloc` impl outside the two crates where
  `unsafe` is allowed.
- **Leave reallocations out of the count, as `stats_alloc`'s count did.** Lost: the
  hook cannot tell a reallocation from an allocation, since the crate has no `realloc`
  of its own, and a wrapper that did would be the `unsafe` impl above; and a vector
  grown inside a measured loop is an allocation the data-plane rule forbids, so the
  gate is right to count it.
- **Capture every allocation's backtrace, not the first.** Lost: a run that allocates
  once per message would capture millions of backtraces, each a symbolisation, and
  the first one is the frame to look at; a second cause shows once the first is fixed.
- **Line numbers in the release build, with debug info for the harness.** Lost for
  now: a profile change for every crate the harness links, built by both bench hosts
  and compared against baselines recorded without it; symbol names name the function,
  and a line is found from the name. Revisited if a name is not enough.
- **A thread name beside the backtrace.** Lost: `std::thread::current()` inside an
  allocator hook can panic while a thread's own data is being destroyed, and a panic in
  a hook aborts the run; the thread's entry is the backtrace's last frame.

## Evidence

- The unit test `a_counting_window_counts_and_keeps_the_first_backtrace` opens a
  window, allocates, closes it, and reads a count of at least one and a captured
  backtrace; `the_failure_carries_the_first_allocations_backtrace` reads the frames in
  the failure's message; `a_counting_window_counts_a_vector_grown_past_its_capacity`
  grows a vector of capacity 1 by one element 64 times inside a window, each grow a
  reallocation, and reads a count of at least 64. The two tests that open a window
  run one at a time, since tracking is process-wide.
- On the release build, on Linux on 2026-11-05, a vector of capacity 1 holding one
  element, pushed once inside the exclusive row's measured loop on the subscriber's
  thread, failed the run with `allocations per message: 0.00001 (1 in 100000
  messages)`, its frames going through `RawVecInner::finish_grow` to the hook;
  `stats_alloc`'s count read 0 for it. The temporary push was then removed.
- `just bench --messages 100000` on the laptop at this change reads `allocations per
  message: 0 (0 in 410000 messages)`, as before the change: one pass measures the two
  log rows and the two client rows of 100,000 each and the UDP row of a tenth of that,
  4 × 100,000 + 10,000. The histograms are in the commit message, inside the laptop's
  band.

## Consequences

Easier: a run that fails the allocation gate names the frame that allocated and the
thread it ran on; card 176's loop on the macOS runner has something to catch.

Harder: a measured loop that grows a vector past a capacity it already had fails the
gate, where under `stats_alloc` it passed; that is the gate doing its job, and a
change that trips it is fixed by sizing the vector at setup. Every allocation in the
harness's process carries the wrapper's header, a word, while a window is closed as
well, which no measured loop pays since it allocates nothing. A zeroed allocation is
an allocation and a write of zeros, not `calloc`, so the harness's setup writes its
zeroed buffers once where `calloc` could have mapped zero pages; no measured loop
makes one. The harness takes a crate under the Mozilla Public License 2.0 beside its
Apache-2.0 and MIT ones, as a build of the gate and never published (ADR 0003).

Revisit: when a captured backtrace does not name the frame, for line tables in the
harness's build; when the allocation is found and fixed, which closes card 176; at
`tracking-allocator` 1.0.

## References

- `CLAUDE.md`: invariant 2; Gates; Unsafe
- ADR 0015; AppShapes Trello card 176
- `bench/src/main.rs`, `CountingTracker` and `Counting`; `bench/Cargo.toml`;
  `docs/how-to/run-the-bench.md`, "When it does not work"
- `tracking-allocator` 0.4.0 (<https://docs.rs/tracking-allocator/0.4.0>), its
  `allocator.rs`, `token.rs` and `lib.rs`, read 2026-11-05
