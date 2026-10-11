# 0010. Validate every peer-written field of a shared-memory layout before using it

- **Status:** Proposed
- **Date:** 2026-10-11
- **Invariants affected:** 1, 2, 3, 4
- **Supersedes:** none

## Context

The driver and every client on a host map the same files: the log buffers, the
counters file, and the command and event buffers between a client and the driver
conductor. In each layout some fields are written by one side and read by the
other: a publisher writes a term's tail, a subscriber writes its position, the
driver writes limits and counters, the creator writes the header. `cavalry-shm` is
one of the two crates where `unsafe` is permitted, and a pointer or an index derived
from a value another process wrote is undefined behaviour the moment that value is
out of range. The driver runs for months and outlives every client; a client may be
a buggy binding, a process of another user with access to the file, or an attacker.
`docs/explanation/why-cavalry.md` sells memory safety as a compliance feature.

Aeron's model trusts its peers. It checks the term length and the page size when a
log is mapped, caps an unpacked term offset at the term length, and stops reading
at a non-positive frame length, and otherwise assumes the client and the driver were
deployed by the same team (`LogBufferDescriptor.checkTermLength`, `checkPageSize`,
`termOffset`; `TermReader.read`).

ZigBolt's IPC channel shows both the failure class and a discipline that closes it.
A test in its tree forges a reader position in the last seven bytes of a term and a
frame length near the maximum; before the fix, a subtraction wrapped to about 4 GiB
and the consumer was handed a slice reaching about 2 GiB past the mapping
(`src/channel/ipc.zig:323-332`, test at `544-588`, commit b7eeb6b). Its fix
validates the header when attaching, checks the object's size before mapping it,
re-reads the header through the final mapping, and validates both positions before
every pointer derivation (`ipc.zig:61-75`, `124-166`, `186-192`, `290-296`). Its
flaw is the failure path: a corrupt header makes a poll return zero frames, the
answer an empty channel gives, with no error and no counter (`ipc.zig:293-296`).
`docs/explanation/should-zigbolt-inform-cavalry.md` records the comparison.

## Decision

Every shared-memory layout in `spec/` carries a table with one row per field: who
writes it, who reads it, and the bound the reader checks before using it. A reader
treats every field its peer can write as untrusted input. The checks are: the
header's magic and version before anything else, read through the final mapping;
each length a power of two within the spec's minimum and maximum; the object's size
from `fstat` at least the size the lengths imply, before the mapping is made; a
header re-read through the final mapping that must equal the probe; positions in
order, within the layout's span, and frame-aligned, before each claim and each poll;
and a frame length within the bytes left in its term before a slice is formed. A
check that fails increments a counter named for the layout and the field, returns an
error code to the caller that says which field failed, and never returns "no data".
The conductor acts on the counter by marking the client's image unavailable or
evicting the client, and `cavalry doctor` explains the counter and names this page.
The creator of a layout writes its header last with a release store, and a reader
acquires the magic before reading the rest. The checks are integer compares with no
syscall, no allocation and no lock on the per-message path; `fstat` runs once at
attach. Debug builds add assertions on the same bounds. The spec names the bounds;
the code copies them from the spec, never the other way round.

## Alternatives considered

- **Trust peers, as Aeron does.** Lost: Cavalry's driver is a shared service that
  outlives its clients and is marketed on memory safety; a client bug must not be a
  driver crash or an out-of-bounds read of another client's stream.
- **Validate in debug builds only.** Lost: the cost is a handful of compares, and
  the forged value arrives in production.
- **Validate at attach only.** Lost: positions and lengths change on every message;
  the ZigBolt bug above was a position, not a header field.
- **Report corruption as "no data", as ZigBolt does.** Lost: a reader would spin
  forever with nothing in the counters file to explain it, against invariants 1
  and 4.
- **Abort the driver on the first bad value.** Lost: invariant 3 says the driver
  degrades and does not fall over; the conductor evicts the client instead.
- **Checksums over the header.** Lost as a substitute: a checksum detects
  corruption but not a hostile peer that recomputes it, and it costs more than the
  compares; it can be added to a layout's header later without changing this
  decision.

## Evidence

- ZigBolt commit b7eeb6b, `src/channel/ipc.zig`: the validation at `61-75`,
  `124-166`, `186-192`, `290-296` and `323-332`; the tests at `544-588`, `643-684`
  and `686-725`; and the comment at `326-330` recording the pre-fix bug.
- Aeron commit 32e531c, `LogBufferDescriptor.java` and `TermReader.java`: the
  checks Aeron does make.
- Not measured. Acceptance needs the log buffer feature's `just bench` run with and
  without the checks, showing no p99.99 difference beyond run-to-run noise on both
  CI runners. Until then this ADR is Proposed.

## Consequences

Easier: a client cannot crash the driver or read past a mapping by writing into
shared memory; every such attempt is a counter `doctor` can name; the spec's table
tells a stranger which process owns each field.

Harder: every shared layout's spec section gains a table and every reader a
validation step; a new field in a layout is a spec change with a row, a check, a
counter and a test; the checks are code on the per-message path and must stay
compares only.

Revisit: when a layout is read only by the process that writes it, where the checks
can be debug-only; if the measured cost at p99.99 is above noise, which would mean
the checks are not compares only; when `spec/` gains a layout with a field no bound
can express.

## References

- `CLAUDE.md`: invariants 1 to 4; Unsafe; Data plane
- `.claude/rules/data-plane.md`: Memory and layout; Unsafe
- `docs/explanation/should-zigbolt-inform-cavalry.md`
- `docs/explanation/why-cavalry.md`, Build
- ADR 0007 (the log buffer feature that applies this decision)
- ADR 0008 and `spec/frame.md`: the data frame header the log buffer's terms
  hold, whose reader checks run in the same order this decision asks of every
  layout
- ZigBolt `src/channel/ipc.zig` and `src/platform/memory.zig` at b7eeb6b:
  <https://github.com/suenot/zigbolt>
- Aeron `LogBufferDescriptor.java`, `TermReader.java` at 32e531c:
  <https://github.com/aeron-io/aeron>
- AppShapes Trello card 78
