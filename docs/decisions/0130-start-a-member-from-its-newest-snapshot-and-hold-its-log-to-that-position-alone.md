# 0130. Start a member from its newest snapshot, and hold its log to that position alone

- **Status:** Accepted
- **Date:** 2026-11-15
- **Invariants affected:** 1, 3, 4, 5
- **Supersedes:** none. Decides the third change of work item 230. Answers ADR
  0116's Revisit on where the consensus module's read of its log begins, and ADR
  0108's Revisit on a member whose list of recordings begins above 0. Changes
  `spec/cluster.md`: three paragraphs of "A member's start" in place of one, a
  sentence each of "Snapshots", "Sessions" and the list of refusals, a row of
  "Values", two rows of "Counters", which `counters.toml` gains as ids 165 and 166,
  and the row of `cluster.log_recordings_forgotten`. A sentence of
  `spec/durable-log.md`, "The driver". No file's format changes, no message's
  layout and no function of the C ABI.

## Context

A member takes snapshots of its sessions and lists them (ADR 0128, ADR 0129), and
starts as if it had none. Its consensus module reads its log from position 0 at
every start, so a start takes as long as the log is long, and a member that won an
election does not serve until that read has reached its term's base (ADR 0116).
For the same reason a member starts only on a log that it holds whole from
position 0: one whose list of recordings begins above 0, or names more than the
1,024 a member keeps, does not start again (ADR 0108), and no recording of a log
can be purged. A log directory lasts a member for 1,024 recordings, one for each
log it joins, each replay it catches up by and each term it leads.

A snapshot is the member's sessions at one position of its log. A member that
begins from it need read the log after that position only. The questions are which
snapshot, what a start does with one it cannot read, and which logs a member may
then start on.

## Decision

A member's start takes the state of its consensus module from the newest snapshot
of it that the member can read whole, and its read of its log begins at that
snapshot's log position. It can read a snapshot whole when the catalog holds the
snapshot's recording, of the snapshot stream, stopped and with bytes; the driver
plays it back, on the log replay stream, within 5 s; and it holds the snapshot the
list names, a begin marker with the entry's position and term, the sessions in
the order of their ids, the module's state and an end marker, with sessions that
the member's table takes whole. A snapshot that fails one of these is passed
over for the one listed before it, counted in `cluster.snapshots_passed_over`. A
member never begins at a snapshot's position with another state than that
snapshot's. With none, the table is empty and the read begins at position 0, as
before. The position the read begins at is the gauge
`cluster.snapshot_start_position`, −1 for none.

A member then starts on a log that it holds whole from that position, and need
hold nothing below it: a list whose floor is at or below the position, a list
whose first recordings were purged when the first one held begins at or below the
position, and a list of more than 1,024 recordings whose newest 1,024 begin at or
below it. A log that begins above the position is refused, `log-forgotten`, and
the refusal says how many snapshots the start passed over: one that the driver
did not play back in time is read by the next start. A service still reads its
member's log from position 0.

## Alternatives considered

- **Refuse the start when the newest listed snapshot cannot be read.** Lost. The
  member's log is whole or it is not, whatever a snapshot's recording has become,
  and an older snapshot, or none, gives the same state after a longer read. A
  start that refused would take a member out for a recording that an operator
  purged by mistake. Passing over is counted, and `cavalry doctor` reports a count
  above 0 as a warning, with what became of the snapshot's recording.
- **Read the snapshot's recording from its files, with no replay.** Lost. The
  consensus module is a client of its driver (ADR 0063) and reads no segment file
  itself. The replay is the path a snapshot will take to another host. What it
  costs a start, 0.1 s to 0.3 s on a loaded laptop, is about the read of 64 MiB of
  log, a quarter of the snapshot interval (Evidence).
- **A stream of its own for the snapshot's replay.** Lost. The log replay stream is
  the module's own, no replay runs on it before the start's read of the log
  begins, and the module's read takes its own replay's session alone. Aeron loads
  a snapshot on its replay stream as well.
- **Keep the rule that a log begins at 0, and purge nothing.** Lost. It is the rule
  that makes a log directory last 1,024 recordings and a log grow for ever, which
  is what the snapshot is for.
- **Take any recording missing from the catalog as purged.** Lost. A recording
  missing between two that are held, or missing above the snapshot's position, is
  a directory that is not as the member left it, and a start that took it would
  report a log shorter than the member had reported (ADR 0108). Only recordings at
  the head of the list, below a recording that begins at or below the snapshot's
  position, are a purge.
- **Have the member purge its log below its snapshot by itself.** Lost for now. It
  is a retention policy, which ADR 0031 leaves to an operator's commands, and a
  service beside the member still reads the log from position 0. The Revisit names
  both.
- **Restore a session's activity clock and what a leader owed it.** Lost. They are
  a leader's own. A member that leads next counts each session's timeout from when
  it begins to serve and sends each a new leader event, as it does for sessions
  it read from the log (ADR 0115).
- **An empty table when the table cannot take a snapshot's sessions.** Lost, after
  it was built. The start had chosen the snapshot, held the log to its position,
  and then filled the table; a table that refused a session was left empty and the
  member read on from the snapshot's position. No snapshot of this version holds
  more sessions than the table, one twice, or a channel longer than a session
  holds, so no test reached it. A member that did reach it would hold no session
  where the others hold some, from that position on: a state that its log does
  not give (invariant 1). The table now fills as the snapshot is chosen, and a
  snapshot it refuses is passed over as one that cannot be read.
- **A refusal of the start when the table cannot take a snapshot's sessions.** Lost
  for the reason the first alternative lost: the log is whole, and an older
  snapshot, or none, gives the right state.

## Evidence

- `crates/cavalry-raft/tests/snapshot.rs`, three members over three drivers on
  the loopback, with two clients' sessions open. A member that stops and starts
  again begins its read at the snapshot's position before its first duty cycle,
  holds both sessions after one cycle, with no leader and no commit position yet,
  reads the three entries that follow, and the next snapshot's bytes are the same
  on the three members. A member whose first recording of its log was purged
  below the snapshot, by a client's `purge recording` on a driver over its
  directories, starts from the snapshot: its list leaves the recording out, the
  list's floor is where the next one begins, it counts no truncation finished in
  `cluster.truncations_finished`, it reads on to the log's end, and
  the next snapshot's bytes are again the same on the three. With the recordings
  of both its snapshots purged as well, that member is refused, `log-forgotten`,
  and the refusal names the two snapshots passed over. A member whose log begins
  at 0 and whose newest snapshot's recording was purged starts from the one
  before it and counts one passed over; with both purged it starts from position
  0 and counts two. A member that started from a snapshot counts the sessions it
  took from it as opened.
- `crates/cavalry-raft/src/snapshot.rs`, the rule on a list and on a recording's
  messages, with no driver: the newest snapshot of the consensus module is read
  first and no other; each one that cannot be read is passed over and counted, to
  none; a service's entry is neither read nor counted; a snapshot that the table
  refuses is passed over for the one before it, and with both refused the start
  takes none. A recording is not the listed snapshot when it is cut short at any
  message, is of another position or term, has its markers the wrong way round or
  of a service's type id, has no module's state or has it before a session, or
  holds a session twice, out of the order of the ids, at or above the next
  session id, or below 0.
- `crates/cavalry-raft/src/module/tests.rs`, the start's rule on a list and a
  catalog given as tables: a floor at the snapshot's position and below it
  starts, and one above it is refused with both positions; two purged recordings
  below a snapshot start and are no truncation cut short, and with the list's last
  recording gone as well that one is; the same purge with no snapshot or an older one is
  refused as a recording missing, and so is a recording missing between two that
  are held, at any position; 1,026 recordings keep the newest 1,024 when those
  begin at the snapshot's position, and are refused one frame below it. The two
  tests of the rule as it was pass with the position at 0. The refusal's message
  says nothing of snapshots when none was passed over, and with two it says the
  count and to start the member once more first.
- What a start costs, timed in a release build on an Apple M5 Pro laptop, macOS
  26.6.1, by the ignored test `a_members_read_of_its_log_at_a_start_timed`: two
  runs at this change's tree, the laptop's load average at 11 to 13, and two
  earlier in the change's work, at 8. With no snapshot the driver's start and the
  member's took 286 ms and 450 ms, and 489 ms and 519 ms in the earlier runs. The
  member then read 64 MiB of its log in 0.222 s and 0.220 s, 288 MiB/s and 292
  MiB/s, and in 0.256 s and 0.365 s earlier. From a snapshot at the end of those
  64 MiB the two starts took 615 ms and 546 ms with the snapshot's recording
  played back, 441 ms and 526 ms earlier, and the member read none of the 64 MiB.
  So the replay of a snapshot costs a start 0.1 s to 0.3 s on a loaded laptop,
  which is within the spread between two starts, and saves it 0.22 s to 0.37 s
  for each 64 MiB of log below the snapshot.
- `cavalry-control`'s tests hold `cavalry doctor` to the new sentence of
  `cluster.log_recordings_forgotten`, and to a warning for
  `cluster.snapshots_passed_over` above 0 and none at 0.

## Consequences

- A member's start reads what follows its newest snapshot, 256 MiB of log at most
  beside what was appended since (ADR 0129), and a member that wins an election
  serves that much sooner.
- A log directory lasts a member for 1,024 recordings of its log after its newest
  snapshot, and no longer for 1,024 in all.
- A session that a start takes from a snapshot counts in
  `cluster.sessions_opened`, so that the count less `cluster.sessions_closed` is
  still the gauge `cluster.sessions` on a member that started from a snapshot. The
  two counts of such a member are of what it read since that snapshot, and are
  lower than those of a member that read the log from position 0.
- An operator can purge the recordings of a member's log that end below its
  snapshot, with the client's command, while the member is stopped. No command of
  `cavalry` does it yet, and nothing purges by itself.
- A service beside a member whose log no longer begins at 0 is refused its
  replay. Until a service takes snapshots, which is the next change of work item
  230, a purge below a member's snapshot is for members whose services need not
  start again from position 0. The page and the how-to say so.
- A member that is behind its leader's log by more than the leader holds, after a
  purge on the leader, receives nothing for the span that is gone, as before
  (`spec/cluster.md`, "Messages outside an election"). Revisit when a member must
  catch up from a peer's snapshot.
- Revisit: when a service takes snapshots, for the lowest position a member's log
  must reach back to; when a policy purges below snapshots (ADR 0031); when a
  snapshot is too long to play back in 5 s, which a consensus module's is not.

## References

- Work item 230 on the Brigade Board, "Catch a member up from a peer's recording,
  snapshot the service and follow as a standby".
- `spec/cluster.md`: "A member's start", "Snapshots", "Sessions", "Counters",
  "Values"; `spec/durable-log.md`, "The driver".
- `crates/cavalry-raft/src/module.rs` (`ConsensusModule::start`, `kept_log`),
  `src/snapshot.rs` (`load`, `newest`, `restore`), `src/sessions.rs`,
  `tests/snapshot.rs`,
  `src/module/tests.rs`; `counters.toml`; `docs/how-to/run-a-cluster.md`.
- [ADR 0128](0128-list-a-members-snapshots-in-a-file-of-their-own-in-its-log-directory.md)
  and
  [ADR 0129](0129-snapshot-a-members-sessions-when-its-read-of-its-log-reaches-the-leaders-action.md),
  the snapshots file and the snapshot;
  [ADR 0108](0108-list-the-recordings-of-a-members-log-in-its-log-directory-and-start-a-member-on-the-log-it-kept.md),
  a member's start on the log it kept;
  [ADR 0115](0115-keep-a-clusters-sessions-on-every-member-opened-and-closed-by-the-log.md)
  and
  [ADR 0116](0116-read-a-members-sessions-from-its-committed-log-and-serve-them-from-its-terms-base.md),
  a member's sessions and its read of its log;
  [ADR 0063](0063-run-the-consensus-module-as-a-client-and-lead-by-appointment.md),
  the module as a client of its driver;
  [ADR 0031](0031-truncate-purge-and-delete-a-stopped-recording.md), retention.
