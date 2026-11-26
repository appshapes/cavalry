# 0169. Count a cluster member's failed start, and say what no check holds

- **Status:** Accepted
- **Date:** 2026-11-26
- **Invariants affected:** 4, 6, and 1 for the rule the page states
- **Supersedes:** none

## Context

Feature 3 of the sixth slice, work item 363, asks that each fault of a cluster member's start be named by a finding. One is not.

`cavalry run` with the cluster's two keys set starts a driver and then the member's consensus module beside it. When the module's start fails, `cavalry run` prints the error and closes the driver. A start fails when the consensus module refuses the members string, the member id or the log directory (`member refused (…)`, `member stopped (…)`), or when the member's thread cannot start. A member's counters are a client's, added when the module starts and freed when it closes (`spec/counters-file.md`). So a member that never ran added none. The driver's file holds nothing a reader can tell from a driver that ran in no cluster. `cavalry doctor` names nothing, and the error is in one place only: the process's standard error, which a person who was not at the terminal never sees.

`spec/cluster.md`, "A member's start", says that a member whose host lost power while it ran a driver of durable log version 4 or before is not started on that log directory again. It gives the reason: that version kept no synced position. It does not say that no check holds the rule, or why none is built.

## Decision

`cavalry run` adds one to a new static counter of the driver, `run.member_refused`, id 204, when its cluster member's start fails, before it closes the driver, as it adds one to `run.member_given_up` when it gives a running member up. A static counter is the driver's own, so the count stays in the directory a closed driver leaves. `cavalry doctor`'s check of the static counters reports it as `WARN run.member_refused`, with the sentence `counters.toml` gives it. That sentence names the line `cavalry run` printed with the reason, where to read it again under systemd, the how-to's entry that explains each reason, and what to do. A program that embeds a member gets the same error from `ConsensusModule::start` and handles it itself; nothing is counted for it. `spec/cluster.md` says that no check holds a log directory a driver of durable log version 4 or before was writing at a power loss. Nothing in such a directory tells a recording the loss cut from one that stopped whole. None is built because Cavalry has made no release, so only a driver built from the history before version 5 writes such a directory.

## Alternatives considered

- **Doctor infers the refusal from what is missing.** This was the brief's suggestion: with the cluster's keys set and no cluster counter in the file, the member did not start. It lost because absence names nothing for certain. Doctor is given the keys by its own environment, which need not be the one `cavalry run` had. A driver that ran in no cluster leaves the same file. And the absence says nothing of the reason or of where it was printed. A counter written at the moment of the failure is the fact itself (`CLAUDE.md`, invariant 4: a counter beats a log line).
- **Write the error's text to a file in the driver's directory.** It would keep the reason itself. It lost because counters are the record `cavalry doctor` reads, and the text is already on standard error, which journald keeps for a service. A second channel for one message is a format to version and to read.
- **Have the consensus module count its own refusal in the driver's file.** That would count it in a program that embeds a member, too. It lost because a refused start has no client of its own to count with, and only the process that holds the driver writes a static counter (`cavalry_control::run`). An embedding program gets the error as a value it must handle anyway.
- **Build a check for a version 4 directory at a power loss.** It lost because the only sign is an index with no synced slot, and recovery also writes such an index when it rewrites one (`spec/durable-log.md`, "Time index"). A check would refuse directories that a driver of version 5 or later wrote, to catch a case that no released driver can make.

## Evidence

- `crates/cavalry/tests/cli.rs`, `a_member_whose_start_is_refused_stops_run_and_doctor_names_it`: `cavalry run` with a member id the members string does not hold exits 1 with `cavalry run: member refused (`. Then `cavalry doctor` on the directory the closed driver left reports `WARN run.member_refused`, naming the line, the how-to and the next step.
- `crates/cavalry-control/src/doctor.rs`, `a_member_whose_start_failed_is_reported_from_the_file_a_closed_driver_left`: the finding from the counter on a closed driver's file, and none at 0.
- `cavalry_control::run::member_refused`'s doctest: the count is in the closed driver's file.
- The repository has no release: no tag, and no released section in `CHANGELOG.md`.

## Consequences

- An operator who reads `cavalry doctor` after the process has gone learns that the member's start failed and where the reason is. The count stays until the next `cavalry run` remakes the directory.
- One more static counter, in the run section of `counters.toml`. A file of a driver older than it holds another counter in its record, and doctor's check says so, as for every static counter.
- A program that embeds a member and does not report its start's error still leaves no sign. Revisit if such programs become a supported way to run a member in production, as `cavalry run` is.
- The rule for a version 4 directory stays an operator's rule with no check. Revisit if a driver of that version is ever released.

## References

- Work item 472, a part of feature 3 of the sixth slice, work item 363.
- `run.member_given_up`, work item 257, which this counter follows.
- `spec/cluster.md`, "A member's start". ADR 0144, the synced position.
- `docs/how-to/run-a-cluster.md`, "When it does not work".
