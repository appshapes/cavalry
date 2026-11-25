# 0159. Report to the program a publication the driver refuses while a service tells its member of a timer

- **Status:** Accepted
- **Date:** 2026-11-25
- **Invariants affected:** 4, 6
- **Supersedes:** none. Corrects one sentence that ADR 0151 gave `spec/cluster.md`,
  "A service's snapshot", within version 2, and one of `spec/c-abi.md`, "Constants",
  within version 6. Nothing on the wire changes, and no declaration of the header.

## Context

ADR 0151 gave a service one more reason to hold its publication on the consensus
module stream: the schedule timers and cancel timers by which it tells its member
of its timers. `spec/cluster.md` says when a service adds its counters and that
publication, and why the counters come first: "so that a publication the driver
refuses there is counted". That was written for a snapshot, where the refusal is
the snapshot's failure and `cluster.service_snapshots_failed` counts it. ADR 0151
carried the sentence over to the two new moments, the first thing a service has to
tell of a timer and the first timer event it passes over. Neither is a snapshot. A
refusal there fails no snapshot, and a service has no counter that it would be
true to count it in.

The independent read of the service library, before work item 419's pull request
for it opened, found the library returning the refusal from the read and counting
it nowhere, against the page.

The same read found a sentence of `spec/c-abi.md` that the library makes untrue.
The page calls a timer event an entry of "the Rust `Other`". The Rust `Entry` now
has `Timer` for the event that is handed over, and hands the others to no program;
through version 6 of the C ABI every timer event still reaches a program as
`CAVALRY_CLUSTER_ENTRY_OTHER`, as it did.

## Decision

A publication on the consensus module stream that the driver refuses at a read is
reported to the program as the error of that read, and counted in no counter of
the service's. The service has added its counters by then, all five together, as
at a snapshot, and its next read with something to tell of a timer or an event to
pass over adds the publication again. The page says so, beside the one failure it
already named as not counted, a refused counter. The driver's own counter of the
commands it refuses, `client_protocol.errors`, counts both.

`spec/c-abi.md` says what a timer event is on each side: `_OTHER` through the C
ABI at version 6, every one, and in Rust `Entry::Timer` for the one handed over.

## Alternatives considered

**Count the refusal in `cluster.service_snapshots_failed`.** It is the counter the
old sentence had in mind. A snapshot that did not fail would be counted as one,
and `cavalry doctor` would send an operator to look for it.

**A counter of its own.** A sixth counter of a service, for a failure the program
is told of in the same call and the driver already counts. A program that does
not look at what its poll returned polls on, and the next poll that has something
to tell adds the publication again or returns the same error: nothing is lost
silently that a counter would find.

**Hold the words and return no error.** The service would then tell its member
nothing for as long as the driver refuses, with no sign of it but a leader that
holds no timer. The error is the sign, and the program's to act on: the driver's
registry of publications is full.

## Evidence

- `crates/cavalry-client/src/service.rs`, `tell_member`: the counters are added
  through `add_counters`, the two of a service's snapshots and the three of its
  timers together, before the publication, and a refusal of the publication is
  the read's error. `ClusterService::snapshot` adds the same five the same way and
  counts a refused publication as the snapshot's failure.
- `crates/cavalry-ffi/src/cluster_service.rs`: `Entry::Timer` and `Entry::Other`
  both map to `CAVALRY_CLUSTER_ENTRY_OTHER`, and the service that the C ABI makes
  is handed the timer events that the Rust library passes over, as entries of that
  kind. `just conformance` runs every client's cluster round against it.

## Consequences

- A service's read can return the driver's refusal of a publication where it
  returned only what its handler's replies and its log's read gave. A program that
  ends on any error of its poll ends there, and one that polls on is told again at
  each poll that has something to tell.
- A leader that holds no timer beside a service that has some pending, which
  `cavalry doctor` names, has one more cause to check: `client_protocol.errors` on
  that host.
- The C ABI's version 7, work item 419's next change, gives a program its timers
  and rewrites the sentence this ADR corrects.

## References

- Work item 419 on the Brigade Board, which this decision was made in.
- ADR 0151, whose page text this corrects; ADR 0131, a service's snapshot and its
  counters.
- `spec/cluster.md`, "Snapshots", "A service's snapshot"; `spec/c-abi.md`,
  "Constants", the cluster entry's kinds.
