# 0011. Pad peer-shared hot fields to 128 bytes

- **Status:** Proposed
- **Date:** 2026-10-11
- **Invariants affected:** 2
- **Supersedes:** none; when accepted, it amends the 64-byte figure in
  `.claude/rules/data-plane.md` and in `docs/explanation/why-cavalry.md`

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
- Not measured. Acceptance needs the log buffer feature's `just bench` run with
  64-byte and 128-byte slots on both CI runners, reported at p99.99 and max. Until
  then this ADR is Proposed.

## Consequences

Easier: the layouts match what Aeron users and Rust readers expect; one constant
in the spec answers "why 128".

Harder: the counters file is 128 bytes per counter; the data-plane rule and
`why-cavalry.md` carry two figures, 64 and 128, with the rule for which applies;
a lint must know which fields the spec calls peer-shared.

Revisit: if the measurement above shows no difference at p99.99 on either runner;
if a target platform appears with a different prefetch pair size.

## References

- `CLAUDE.md`: invariant 2; Data plane
- `.claude/rules/data-plane.md`: Memory and layout
- `docs/explanation/should-zigbolt-inform-cavalry.md`
- `docs/explanation/why-cavalry.md`, Build
- ADR 0007 (the log buffer and counters file features that apply this decision)
- Agrona and Aeron at 32e531c: <https://github.com/aeron-io/aeron>,
  <https://github.com/aeron-io/agrona>
- Zig `std.atomic.cache_line`:
  <https://github.com/ziglang/zig/blob/0.15.1/lib/std/atomic.zig>
- `crossbeam_utils::CachePadded`:
  <https://docs.rs/crossbeam-utils/latest/crossbeam_utils/struct.CachePadded.html>
- AppShapes Trello card 78
