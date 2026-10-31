# 0038. Repair a retention command's slot after a failed catalog write as after a file error

- **Status:** Accepted
- **Date:** 2026-10-31
- **Invariants affected:** 1, 4
- **Supersedes:** none; fills a gap in ADR 0031's retention commands

## Context

ADR 0031 gives each retention command a fixed order of catalog writes and file work, and
says that a file the driver could not read, remove, cut or extend is refused with code
12 after the driver has run recovery's rules for the slot, so that the slot and the
files agree before the refusal is answered (`spec/durable-log.md`, "The driver";
`spec/client-protocol.md`, "Commands"). It says nothing about a catalog write of the
command's that fails. The review of pull request 82 found what that silence leaves: a
truncate's last write, the stop as recovery would rebuild it, failing after the files
were cut leaves the slot reading "live, or being cut" over files already cut, with no
command in the registry to finish it, and the next `start replay` reads the recording
as live. Unspecified behaviour is a bug in the spec, not a freedom in the code
(CLAUDE.md, Protocol).

## Decision

A catalog write of a retention command's that fails is refused to the client with code
12, adds one to `recording.retention_failed`, and is followed by the same repair as a
file error: the replayer runs the file side of recovery's rules for the slot where the
files are open, the recorder makes the one catalog write those rules end in, and the
error's message says what the recording holds now. A failed last write of a truncate
thereby ends with the stop rebuilt from the files as cut, or, when the catalog refuses
the repair's write too, with a message that says so and sends the operator to `cavalry
doctor`. Both spec pages say so.

## Alternatives considered

- **Answer code 12 at once and leave the slot to the next start-up's recovery.** Lost:
  until the driver restarts the slot reads live, a `start replay` of it follows a synced
  counter the conductor has freed, and `cavalry doctor` names a recording that is
  stopped as live; invariant 1 says the absence is reported, not misreported.
- **Retry the catalog write before refusing.** Lost: a write that fails on a full or
  failing filesystem fails again, and the client's response timeout is the only bound a
  retry would have; the repair runs within the command's duty cycles and answers.

## Evidence

- `crates/cavalry-driver/src/conductor.rs`, the failed-catalog-write arm of
  `on_catalog_done`, takes the repair hops `RepairFiles` and `RepairSlot` that a file
  error takes (pull request 82, the second card 95 change). No test provokes a failed
  catalog write: the recorder writes the catalog through a handle opened at start,
  which a permission change cannot refuse, and the harness has no failing filesystem;
  the repair itself is tested by `a_repair_after_failed_file_work_counts_and_rebuilds_the_stop`
  in `mod replay`, which the failed write shares.

## Consequences

Easier: every path a retention command can fail on ends with the slot and the files in
agreement, and `cavalry doctor` reads a stopped recording as stopped. Harder: nothing;
the repair's two hops exist for the file error. Revisit when a failing filesystem can be
injected under the harness, to test the write that fails.

## References

- `spec/durable-log.md`, "The driver"; `spec/client-protocol.md`, "Commands" and "What
  the driver rejects"; ADR 0031.
