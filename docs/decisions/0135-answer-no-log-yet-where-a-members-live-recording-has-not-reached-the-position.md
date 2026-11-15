# 0135. Answer no log yet where a member's live recording has not reached the position

- **Status:** Accepted
- **Date:** 2026-11-15
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. Decides work item 352. Adds a case to ADR 0103's rule for
  the position its file gives to the live recording, which ADR 0116 made the
  member's own read too. Changes `spec/cluster.md`: a paragraph of "The service",
  and a clause of state 13's row in "Roles and the election", on where the append
  position is once a catch-up's replay has passed the live recording's start.
  Changes one clause of the committed log's row in `spec/c-abi.md`. The cluster
  page's version stays 0 and the C ABI stays at version 5: no message's layout
  changes, no file's format and no function of the C ABI.

## Context

`master`'s run 43736415925 at a2e76119 failed once in `gates (linux)`, job
132879885515, in `crates/cavalry-raft/tests/throttle.rs`, three members with one
throttled by a cgroup to 5% of a CPU. A member's process ended on its duty cycle's
error, the driver's refusal of a replay: `position 1097664 is not a multiple of 32
from 475008 to 642912`, error code 23. The same tree had passed that job an hour
before, and passed it again on the one re-run.

A service reads its member's log one recording at a time, and takes each position
from the first recording that the member's file of its log's recordings lists for
it, or from the member's live recording when the file lists none (ADR 0103). The
member's consensus module reads its own log in the same way, for its sessions (ADR
0116). Both open the log with `CommittedLog::open`, which started the replay and
returned the driver's refusal as its error.

A follower that joins late catches up through a replay of the leader's recording
(`spec/cluster.md`, state 13). It records the replay, and it records the live log
from where it joined it. Its append position is the end of what its recordings
hold, so it is the replay's recording's for as long as the replay is ahead. Two
things pace the two streams apart. The replay is sent to that follower alone. The
leader's log publication is `fc=min`: it goes no further than a receiver window
past its slowest receiver. With one member throttled, the live log stands still
while the replay runs on to the end of the leader's recording.

The catch-up then ends with the member's log ending in the replay's recording,
above everything its live recording has synced. The member's file lists the
replay's recording with the position it had synced when the file was written. A
reader that follows that recording to its stop asks for the position it stopped
at. No listed recording holds that position, so the rule gives the live recording,
which has not reached it, and the driver refuses a position above a recording's
extent with code 23.

Code 23 has a second meaning that must stay an error. A position below every
recording the member keeps, or one that a purge took, is refused with the same
code, and that refusal is how a service learns that its member's log no longer
reaches back to its position (ADR 0103, ADR 0132).

`cavalry run` tries a duty cycle that fails again every 100 ms and gives the
member up after 10 s of failures (ADR 0103). So this fault costs a member 10 s of
cycles 100 ms apart and then the member, for as long as its live log is behind.
The test's member ends at the first error.

## Decision

A position that the live recording has not reached is no log yet. `CommittedLog::open`
answers `None` for it, which the C ABI gives as 0 with a null handle, and the
caller asks again later, as it does while the member names no recording.

The open decides in two steps, both for a position that is a multiple of 32 and
that the file gives to the live recording at or above the live start position.
First it loads the live recording's `recording.synced_position` from a record it
checks to be that recording's after the load. While the value is below the
position, it answers `None` and sends the driver no command. It does the same for
the live recording of a member that writes no file. Second, when the driver holds
no such counter, the recording has stopped while the member names it still. The
open then asks for the replay, and when the driver refuses with code 23 and the
file says where the live recording begins, it answers `None` too: the recording
stopped below the position.

Every other refusal stays the caller's error, and so does code 23 for every other
position: one that is not a multiple of 32, one below the live start position, one
of a member that writes no file whose live recording has stopped, and one in a
recording that the file lists.

## Alternatives considered

- **Leave the refusal as the duty cycle's error.** Lost. A member whose live log
  lags behind its catch-up is degraded, and invariant 3 says that it does not
  fall over. A later duty cycle would not have been refused.
- **Answer no log for every code 23.** Lost. A purged position and a forgotten one
  are refused with the same code, and a service that waited there would wait for
  ever beside a log that is gone, with nothing reported (invariant 1).
- **Have the driver wait.** `start replay` would accept a position above a live
  recording's extent and begin when the recording reaches it. Lost. It changes a
  command of `spec/client-protocol.md`, holds one of the driver's 64 replays for a
  wait with no bound, and still needs an answer for a recording that stops below
  the position.
- **Have the member write its file again when the catch-up ends**, with the
  replay's recording's last stop. Lost. The position asked for is that stop, and
  no recording holds its own stop, so the rule gives the live recording all the
  same. It would mend only the positions below the stop.
- **Answer no log only up to the member's `cluster.append_position`**, and leave a
  position above it refused, as before. Lost, for a race. The member stores that
  counter once in a duty cycle, and the reader's position comes from a replay
  that plays what the driver has synced. A sync that lands between the two puts
  the reader's position above the stored value, and the open would return the
  refusal this decision removes.
- **Have the member skip the open by what it knows of its own live recording.**
  Lost. A service beside the member makes the same open and knows none of it.
- **Read the recording's bounds from the refusal's message.** The message names
  the retained position and the extent, which would tell a position above the
  stop from one that a purge took. Lost. A message is written for a person and
  promises no format.

## Evidence

- `crates/cavalry-client/tests/committed_log.rs`, a member played by a client
  against a driver. A recording of a replay holds 12 entries, to 768, and the live
  recording has synced 512. Before the change the open at 768 was `Refused {
  PositionOutOfRange, 23, "position 768 is not a multiple of 32 from 0 to 512 ..." }`.
  After it, three opens answer `None` and neither `replay.started` nor
  `replay.start_failed` moves. A position there that is not a multiple of 32 is
  still code 23. Once the live recording has synced 768, the open gives a handle,
  and the service reads every entry once, in order.
- The same file, the live recording stopped at 512 while the member names it.
  Three opens at 768 answer `None`, and `replay.start_failed` rises by 3. A
  position that is not a multiple of 32 is code 23. Once the member has stored −1
  and named its next recording, the open gives the entries from 768.
- The same file, the refusals that stay, each beside an open that answers `None`
  so that the refusal is the difference's. With no file and the live recording
  stopped at 512, the open at 768 is code 23. By a file whose live recording
  begins at 1,024, the open at 768 is code 23. And a position that a listed
  recording no longer holds, 640 in a recording cut to 512, is code 23. A unit
  test in `src/committed_log.rs` holds the two guards for each part of a member's
  log: a listed recording, the live one by a file, and the live one with no file.
- `crates/cavalry-raft/tests/partition.rs`, three members over three drivers
  through a relay, with no cgroup, in about 2 s. The relay loses the data frames
  sent to one follower's `log` endpoint while the leader appends 2,000 entries,
  so the leader's sender stops a receiver window past that follower. A second
  follower joins late and catches up to 201,696 while its live recording has
  synced 140,736. Before the change its duty cycle ended on `Refused {
  PositionOutOfRange, 23, "position 201696 is not a multiple of 32 from 140736 to
  140736 ..." }`, the failure of `master`'s run. After it the member runs for a
  quarter of a second of duty cycles with its live recording held still, none of
  which fails, a service beside it is answered `None`, and once the relay lets
  its log through the three members and their services hold the same 2,110
  entries.
- `crates/cavalry-raft/tests/throttle.rs` passes on the laptop, which has no
  cgroup and throttles the member with `SIGSTOP`. The pull request's body has its
  twenty runs on each of CI's runners.
- The cost of an open that answers `None` so, timed by the ignored test
  `an_open_that_answers_no_log_yet_timed` in a release build on an Apple M5 Pro
  laptop, macOS 26.6.1, at a load average of 1.5, over 10,000 opens: p50 8.5 µs,
  p99 19.4 µs, p99.99 101.3 µs, max 102.6 µs. In three such runs the p50 was 8.5
  to 9.2 µs and the max 62.7 to 102.6 µs. Each open reads the member's file and
  the counters' records and sends no command. No Linux host has timed it.
- Whether a purge can be answered as no log, read from the code. The first step
  cannot: the driver refuses a retention command on a live recording with code
  24, and a recording that is live was never purged, so a position above what it
  has synced is one it has not reached. The second step can, in one case. A
  recording that stopped while its member names it is no longer live to the
  driver, which then accepts a purge of it. For a position that such a purge
  took, at or above the live start position, the open answers `None` for as long
  as the member names the recording. Once the member has left its log, its file
  lists the recording, and code 23 is the caller's. `cluster.log_purge_position`'s
  rule is to purge a recording of a member's log while the member is stopped
  (`counters.toml`). A stopped member names no recording, so no purge made by the
  rule is answered so.

## Consequences

- A member whose catch-up ended ahead of its live log runs on. Its read of its
  own log waits at the end of the replay's recording until its live recording
  has synced past it, and reads on from there.
- A service beside such a member is answered `None` and asks again. Its program
  sees no error.
- A position past the end of a member's log was refused with code 23, and is now
  no log yet. `cluster-service --from` with a position that the member has not
  recorded waits where it printed `error 23`. The how-to's "When it does not
  work" says so, and names `cavalry doctor`'s `append position` to compare with.
- The example `cluster-service` stops at the first `None` after a handle that
  ended whole, and that `None` no longer means that the member left its log. Its
  last line said `the member's log ended` and now says `the member has no log
  from position`, and the how-to gives both causes and the same thing to do:
  start it again with `--from` that position.
- A caller that asks in every duty cycle pays the open's cost in each, 8.5 µs at
  p50 and 101.3 µs at p99.99 on the laptop, for as long as the live recording is
  behind. That is
  a follower whose live log is paced by a slower member. `Cavalry::committed_log`
  already asks for a pause between two calls.
- While a member names a recording that has stopped below the reader's position,
  each open is three commands to the driver, one of them refused and counted in
  `replay.start_failed`. A follower leaves the log of a leader it has lost within
  the leader heartbeat timeout, 10 s. A recording that stopped on a write failure
  may be named for longer, and `recording.write_failed` counts it.
- An operator who purges a recording that a running member still names, against
  the rule, has a service wait where it would have been refused, until the
  member leaves its log.
- Revisit: when a member's own read of its log should pause between two opens;
  when `start replay` can wait for a live recording to reach a position; when a
  client can ask the driver for a stopped recording's bounds, which would tell a
  purged position from one above the stop in the second step.

## References

- Work item 352 on the Brigade Board, "Find why a member's duty cycle ended on a
  replay its driver refused, in the throttle test on master".
- `spec/cluster.md`: "The service", "The log's recordings", "Roles and the
  election", state 13. `spec/c-abi.md`, the committed log's row.
  `spec/client-protocol.md`, "Commands", start replay, and "Error codes".
- `crates/cavalry-client/src/committed_log.rs` (`CommittedLog::open`,
  `synced_position`), `tests/committed_log.rs`;
  `crates/cavalry-raft/tests/partition.rs`, `tests/relay/mod.rs` (`hold_log`),
  `tests/throttle.rs`; `crates/cavalry-raft/src/module.rs` (`work_sessions`).
- [ADR 0103](0103-list-the-recordings-of-a-members-log-in-its-driver-directory-for-a-service-to-read-in-order.md),
  the file and its rule;
  [ADR 0116](0116-read-a-members-sessions-from-its-committed-log-and-serve-them-from-its-terms-base.md),
  the member's own read;
  [ADR 0132](0132-start-a-service-from-the-snapshot-its-member-names-when-its-program-asks.md),
  the purge rule and what a refused replay tells a service;
  [ADR 0088](0088-catch-up-within-a-leaders-term-and-join-the-live-log-from-the-replay.md)
  and
  [ADR 0094](0094-record-a-members-replays-over-a-driver-and-count-the-live-recording-once-the-log-reaches-it.md),
  the catch-up and its two recordings.
