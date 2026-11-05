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
the gate. So the one allocation card 176's runs counted was an allocation from empty,
not the growth of a vector that already had a capacity, and the new count can read
more than 1 on that runner if its loops also reallocate. `tracking-allocator` 0.4.0
implements `alloc` and `dealloc` and nothing
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
`stats_alloc` and fails it now.

On the first allocation the tracker counts, once per process, it captures
`std::backtrace::Backtrace` and keeps it. The failure line that ends a run with a
non-zero count is followed by that backtrace, frame by frame from the hook to the
thread's start, so the next failing run names the function and the thread. The
captures in Evidence show how the frames read on the release build:

- The top two frames are the hook's, `CountingTracker::allocated`, and the
  allocator's, `tracking_allocator`'s `alloc`, in every capture.
- The frame under them is the allocation: `alloc::fmt::format::format_inner` for a
  `format!`, `RawVecInner::finish_grow` over `RawVec<T>::grow_one` for a push past
  capacity.
- The first frame of a Cavalry crate under that is the function that allocated, or
  the function the release build inlined it into: a `format!` in `run_log`'s loop
  reads `cavalry_bench::run_pass`.
- The frame `std::sys::backtrace::__rust_begin_short_backtrace` names the thread. On
  the main thread, the frame above it is `cavalry_bench::main`. On a spawned thread,
  its type parameters are the closure the thread was spawned with and what the
  thread returns, `cavalry_bench::run_log::{closure#9}` and
  `Result<Histogram<u64>, String>` for the log rows' subscriber. The frame above it
  is the thread's loop when the build kept one, such as
  `<cavalry_driver::conductor::Conductor>::run`.
- The frames under it are the standard library's thread start and the C library's,
  the same on every thread. The thread's name is not among them.

The release build keeps symbol names and not line numbers, which is enough to name
the function. The backtrace is captured on the failure path alone: a run that counts
zero captures none. When the capture is not there by the time the harness reads it,
after the last row and up to five seconds later, the failure line is followed by `no
backtrace was captured for the first allocation` and says to run the command again.

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
  a hook aborts the run. The frames name the thread without it: the
  `__rust_begin_short_backtrace` frame carries the closure the thread was spawned
  with, which names the function that spawned it, and what the thread returns, which
  tells apart most threads one function spawns. Threads that return the same type
  are told apart by the closure's number and by the thread's loop in the frame
  above: the driver's conductor is `Driver::start_with_threads`'s `{closure#7}`,
  returning `()`, over `<cavalry_driver::conductor::Conductor>::run`. Revisited if a
  capture names a closure whose thread neither tells apart.

## Evidence

- The unit test `a_counting_window_counts_and_keeps_the_first_backtrace` opens a
  window, allocates, closes it, and reads a count of at least one and a captured
  backtrace; `the_failure_carries_the_first_allocations_backtrace` reads the frames in
  the failure's message; `a_counting_window_counts_a_vector_grown_past_its_capacity`
  grows a vector of capacity 1 by one element 64 times inside a window, each grow a
  reallocation, and reads a count of at least 64. The two tests that open a window
  run one at a time, since tracking is process-wide.
- Three real failures on the release build, on a 4-core Linux x86_64 runner on
  2026-11-05, each from a temporary allocation added for one run and then removed.
  Each failed the gate, and each printed the frames below under the failure line.
  - A `format!` once in the exclusive row's measured loop, on the publisher's thread,
    which is the main thread; `just bench --only exclusive --messages 100000` read
    `allocations per message: 0.00001 (1 in 100000 messages)`:

    ```text
       0: <cavalry_bench::CountingTracker as tracking_allocator::AllocationTracker>::allocated
       1: <tracking_allocator::allocator::Allocator<std::alloc::System> as core::alloc::global::GlobalAlloc>::alloc
       2: alloc::fmt::format::format_inner
       3: cavalry_bench::run_pass
       4: cavalry_bench::main
       5: std::sys::backtrace::__rust_begin_short_backtrace::<fn() -> std::process::ExitCode, std::process::ExitCode>
       6: std::rt::lang_start::<std::process::ExitCode>::{closure#0}
       7: std::rt::lang_start_internal
       8: main
       9: <unknown>
      10: __libc_start_main
      11: _start
    ```

  - A push past capacity once on the same row's subscriber thread, a `Vec<u64>` of
    capacity 1 that held one element: the same command read `1 in 100000 messages`.
    `stats_alloc`'s count would have read 0 for it, a reallocation.

    ```text
       0: <cavalry_bench::CountingTracker as tracking_allocator::AllocationTracker>::allocated
       1: <tracking_allocator::allocator::Allocator<std::alloc::System> as core::alloc::global::GlobalAlloc>::alloc
       2: <alloc::raw_vec::RawVecInner>::finish_grow
       3: <alloc::raw_vec::RawVec<u64>>::grow_one
       4: std::sys::backtrace::__rust_begin_short_backtrace::<cavalry_bench::run_log::{closure#9}, core::result::Result<hdrhistogram::Histogram<u64>, alloc::string::String>>
       5: <std::thread::lifecycle::spawn_unchecked<cavalry_bench::run_log::{closure#9}, core::result::Result<hdrhistogram::Histogram<u64>, alloc::string::String>>::{closure#1} as core::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
       6: <std::sys::thread::unix::Thread>::new::thread_start
       7: <unknown>
       8: <unknown>
    ```

  - A `Box::new` on every turn of the driver's conductor loop, in
    `cavalry-driver/src/conductor.rs`; `just bench --only claim_commit_32_client
    --messages 100000` read `12856 in 100000 messages`:

    ```text
       0: <cavalry_bench::CountingTracker as tracking_allocator::AllocationTracker>::allocated
       1: <tracking_allocator::allocator::Allocator<std::alloc::System> as core::alloc::global::GlobalAlloc>::alloc
       2: <cavalry_driver::conductor::Conductor>::run
       3: std::sys::backtrace::__rust_begin_short_backtrace::<<cavalry_driver::Driver>::start_with_threads<&std::path::Path, std::path::PathBuf>::{closure#7}, ()>
       4: <std::thread::lifecycle::spawn_unchecked<<cavalry_driver::Driver>::start_with_threads<&std::path::Path, std::path::PathBuf>::{closure#7}, ()>::{closure#1} as core::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
       5: <std::sys::thread::unix::Thread>::new::thread_start
       6: <unknown>
       7: <unknown>
    ```

  The `run_log` closures are counted in source order from 0, and `{closure#9}` is the
  subscriber's `spawn` closure. The two runs that allocated once counted exactly 1 on
  Linux. On macOS the C library's frames at the bottom may differ; the reading above
  does not depend on them. No capture has been taken on macOS yet.
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
