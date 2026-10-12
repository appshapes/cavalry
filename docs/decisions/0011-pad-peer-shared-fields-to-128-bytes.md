# 0011. Pad peer-shared hot fields to 128 bytes

- **Status:** Accepted
- **Date:** 2026-10-11
- **Invariants affected:** 2
- **Supersedes:** none; amends the 64-byte figure in `.claude/rules/data-plane.md` and
  in `docs/explanation/why-cavalry.md`

## Context

`.claude/rules/data-plane.md` says anything shared between threads or processes is
padded or aligned to 64 bytes, and `docs/explanation/why-cavalry.md` writes it as
`#[repr(align(64))]`. The figure is the cache line of every x86-64 and most Arm
cores. It is not the unit the hardware moves. Intel's spatial prefetcher pulls pairs
of 64-byte lines, so two fields 64 bytes apart that two cores write still contend;
Zig's standard library says the same and sets its padding constant to 128 bytes on
x86-64 and aarch64 (`std.atomic.cache_line`, Zig 0.15.1); Rust's `crossbeam-utils`
pads to 128 on x86-64, aarch64 and powerpc64 for the same reason (`CachePadded`,
0.8.23).

Aeron already does this. Agrona spaces the tail, head, correlation and heartbeat
counters of its ring buffers `CACHE_LINE_LENGTH * 2` apart
(`RingBufferDescriptor.java`), gives every counter in the counters file a slot of
`CACHE_LINE_LENGTH * 2` "to avoid false sharing" (`CountersReader.COUNTER_LENGTH`),
and places the hot fields of the log metadata page at offsets 0, 128 and 256
(`LogBufferDescriptor.java`). ZigBolt aligns its positions to Zig's 128-byte
constant (`src/channel/ipc.zig:30-40`). The comparison in
`docs/explanation/should-zigbolt-inform-cavalry.md` is what surfaced the gap
between Cavalry's rule and both implementations.

## Decision

In every shared-memory layout, a field that one party writes and another party
reads in its hot loop occupies its own 128-byte slot, aligned to 128 bytes: the
tail counter of each term, each subscriber position, each publisher limit, each
consumer heartbeat, and each value in the counters file. Other shared state keeps
the 64-byte rule. The slot size is a constant in the layout's `spec/` section and
part of its version, since the offsets are shared across processes. In Rust the
slot is a `#[repr(C, align(128))]` wrapper in `cavalry-shm`, and `just lint` checks
that no `align(64)` wrapper holds a field the spec lists as peer-shared.

## Alternatives considered

- **Keep 64 bytes.** Lost: the contention it leaves is paid at the tail, which is
  where invariant 2 judges; two production systems and two standard libraries
  chose 128 on the same evidence.
- **Pick the size at runtime from the CPU.** Lost: the layout is shared across
  processes and fixed by the spec; a size chosen at runtime is a layout version by
  another name, and mixed versions must run during an upgrade.
- **256 bytes.** Lost: no source names a platform Cavalry targets that moves 256
  bytes; the counters file would double again for nothing measured.
- **128 bytes everywhere.** Lost: the cost is paid where two parties contend, and
  read-mostly layout fields gain nothing from the extra 64 bytes.

## Evidence

- Agrona `RingBufferDescriptor.java` and `CountersReader.java`, Aeron
  `LogBufferDescriptor.java`, at commit 32e531c on 2026-10-11.
- Zig 0.15.1 `lib/std/atomic.zig`, `cacheLineForCpu`: 128 for x86_64 and aarch64,
  citing the spatial prefetcher.
- `crossbeam_utils::CachePadded` 0.8.23: "On x86-64, aarch64, and powerpc64,
  N = 128", because "spatial prefetcher is pulling pairs of 64-byte cache lines at
  a time".
- ZigBolt commit b7eeb6b, `src/channel/ipc.zig:30-40` and
  `src/platform/config.zig:4-6`.
- Measured on 2026-10-11 with the slot-padding rows of `just bench` (ADR 0015): one
  thread stores to a word while another loads a word 0, 64 or 128 bytes away, both
  timed per operation, the pattern a subscriber's position and a publisher's limit
  make. The counters file's slot size is fixed by `spec/counters-file.md`, so the
  pattern is reproduced on plain memory rather than by rebuilding the file at 64 bytes.
  Three runs on each CI runner, 1,000,000 operations each: commit 984cb71 (run
  38170347896, loads only) and commit a2fe613 (run 38170750453, checked and unchecked
  builds, loads and stores):

  | runner | spacing | load p50 ns | load p99.99 ns (three runs) | store p50 ns | store p99.99 ns (two runs) |
  | --- | --- | --- | --- | --- | --- |
  | Linux x86_64, 4 vCPU | 0 | 30, 30, 100 | 50, 60, 872 | 20 | 30, 91 |
  | Linux x86_64, 4 vCPU | 64 | 20 | 30, 21, 60 | 20 | 40, 60 |
  | Linux x86_64, 4 vCPU | 128 | 20 | 21, 21, 21 | 20 | 30, 21 |
  | macOS aarch64, 6 vCPU | 0 | 42, 41, 41 | 167, 166, 167 | 42 | 167, 208 |
  | macOS aarch64, 6 vCPU | 64 | 41, 42, 41 | 208, 333, 292 | 42 | 9,711, 9,087 |
  | macOS aarch64, 6 vCPU | 128 | 0, 0, 0 | 84, 209, 291 | 0, 41 | 167, 625 |

  The `timer` floor is 20 to 21 ns on Linux and one 41.67 ns tick on macOS, where
  `Instant` cannot resolve a single load, so a macOS p50 of 0 means below one tick and
  of 41 means one tick. On macOS a word 64 bytes from a hot store costs the same as
  sharing its cache line at p50, one full tick on every load, and its stores reach 9 µs
  at p99.99 against 167 ns at 128 bytes: Apple silicon treats the 128-byte pair as a
  unit. On Linux the 64-byte spacing sits at the timer floor at p50 and is above it at
  p99.99 in two runs of three (30 and 60 ns against 21), level in the third; its stores
  are above the 128-byte spacing's in both runs. 128 bytes is at the floor on both
  runners in every run. Accepted on that; the Revisit clause's "no difference on either
  runner" did not happen. The amended rule is in `.claude/rules/data-plane.md` and
  `docs/explanation/why-cavalry.md`, and `just lint` refuses an atomic in an
  `align(64)` wrapper under a data-plane crate.

## Consequences

Easier: the layouts match what Aeron users and Rust readers expect; one constant
in the spec answers "why 128".

Harder: the counters file is 128 bytes per counter; the data-plane rule and
`why-cavalry.md` carry two figures, 64 and 128, with the rule for which applies;
a lint must know which fields the spec calls peer-shared, and the log buffer
feature builds that lint as the first feature to need it (`CLAUDE.md`, Gates).

Revisit: if the measurement above shows no difference at p99.99 on either runner;
if a target platform appears with a different prefetch pair size.

## References

- `CLAUDE.md`: invariant 2; Data plane
- `.claude/rules/data-plane.md`: Memory and layout
- `docs/explanation/should-zigbolt-inform-cavalry.md`
- `docs/explanation/why-cavalry.md`, Build
- ADR 0007 (the log buffer and counters file features that apply this decision)
- ADR 0008 and `spec/frame.md`: the data frame header that fills the terms
  whose tail counters take these slots
- Agrona and Aeron at 32e531c: <https://github.com/aeron-io/aeron>,
  <https://github.com/aeron-io/agrona>
- Zig `std.atomic.cache_line`:
  <https://github.com/ziglang/zig/blob/0.15.1/lib/std/atomic.zig>
- `crossbeam_utils::CachePadded`:
  <https://docs.rs/crossbeam-utils/latest/crossbeam_utils/struct.CachePadded.html>
- AppShapes Trello card 78
