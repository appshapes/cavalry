# 0095. Truncate a member's log over a driver, recording by recording, then its record of terms

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. No page of `spec/` changes: `spec/cluster.md`, "Messages
  during an election", already says which commands a truncation is. It is what the
  Revisit of ADR 0083 asks for, and what ADR 0094's Consequences leave to it.

## Context

A member told a next term whose base is below its append position truncates to
that base first: the tail of an older term that no quorum recorded (ADR 0083). The
core does it through one operation of `Io`, `truncate_log(position)`, and calls it
only for a member that is out of the log. The consensus module refused it as
`unsupported`. It was the last operation the module refused; its own core is
appointed until the last change of ADR 0081 and never asks.

`spec/cluster.md` names the commands: `truncate recording` on the recording that
begins at or below the position and ends above it, `purge recording` on every
recording that begins above it, and every recorded term whose base is above the
position leaves the record of terms.

Three things were open.

**Which recordings.** A member's log is its recordings in position order. The
module knows them as the table of the recordings it stopped (ADR 0091). Since ADR
0094 two of them can overlap: the recording of a catch-up's replay and the live
one that began inside it.

**One step or several.** ADR 0083 left it to this change whether the module's
truncation is one step, or the core holds a member in canvass while one is
pending.

**Asked twice.** A leader sends its new leadership term again each leader
heartbeat interval while the member canvasses, and the core calls again when a
call faulted.

## Decision

**The kept table says which recordings, from its end.** The table is in the order
its recordings begin. The module purges each recording that begins above the
position, from the table's end, and forgets it. Then it truncates every recording
left that stopped above the position, which is one, or two where two overlap, and
sets that recording's stop in the table. A recording cut to where it begins holds
nothing to replay and is forgotten; it stays in the driver's catalog, empty.

**Then the record of terms.** `TermsFile::truncate_above(position)` deletes every
term whose base is above the position, each write synced. A term whose base is the
position stays, as the page has it. Then the member's log ends at the position,
and the module returns the highest term the record holds.

**One step.** Each command waits for the driver's answer, within the client's
response timeout: the driver answers `truncate recording` and `purge recording`
once the files and the catalog are written. So `truncate_log` returns the term, or
a fault. It returns that a step is under way in one case alone: the member still
holds a recording of the log or of a replay, which the core never asks of. The
core holds no member in canvass for a truncation.

**Asked twice changes nothing.** A truncation to where a recording already stops
is a command the driver answers with success, and the table already says so, so
it is not sent. A purge whose answer was never read is answered the second time
with "unknown recording", which the module takes as done. A fault part way
returns with the table cut as far as the commands went; Consequences say what the
member reports until it is asked again.

**Recordings first, the record after.** A member over a driver does not start on a
log it kept yet. When it does, what it reads back is the record of terms and the
catalog. A stop between the two steps then leaves a record that names a term whose
base is above where the log ends, which a start can see and cut, where the other
order would leave bytes above the record's last term that nothing names.

## Alternatives considered

- **Cut the record of terms first.** Lost, for the reason above.
- **Ask the catalog which recordings hold bytes above the position.** Lost for
  now. The catalog holds every recording of the driver, and which are this
  member's log is what the table knows (ADR 0091). The read of the catalog at a
  start fills the table, and comes with that change.
- **Truncate a recording that begins above the position to its start, and keep
  it.** Lost. The page says purge, and an empty recording at a position the log
  no longer reaches would be found by a later replay of this member's own.
- **Return that a step is under way and finish over several duty cycles.** Lost.
  The commands are answered within the response timeout, as every command of the
  module's is, and a core that waits needs a state it does not have (ADR 0083).

## Evidence

- `crates/cavalry-raft/src/module/tests.rs`, five tests over two drivers, the
  leader and the member each a module on a driver of its own.
  - A member holds term 0 in one recording, 0 to 192, and term 1 in another, 192
    to 384, with both terms in its record. Truncated to 288, the second recording
    stops there in the catalog and in the table, the record keeps both terms and
    the log ends at 288; asked again, nothing changes. Truncated to 192, where the
    second recording begins, that recording holds nothing and is forgotten, and
    term 1, whose base is not above 192, stays in the record. Truncated to 96, the
    first recording is cut and term 1 leaves the record.
  - Truncated from 384 to 96 in one call: the second recording is purged, gone
    from the catalog and from the table; the first is cut to 96; the record holds
    term 0; a replay of this member's own from 96 finds no recording. Asked again,
    nothing changes.
  - A purge whose answer was never read. The member's own client purges the
    second recording, 192 to 384, and the table still names it. Truncated to 96,
    the purge is answered "unknown recording" and taken as done: the second is
    gone from the table, the first is cut to 96, and the record holds term 0.
  - After a catch-up the member keeps the replay's recording, 0 to 288, and the
    live one, which began below 192. Truncated to 192, both stop there in the
    catalog and in the table.
  - A member that still follows the log, asked to truncate, does nothing and says
    a step is under way.
- Each rule taken out fails a test: with only the first recording cut, the test of
  the two that overlap; with the record of terms left as it was, the first three;
  with a recording that begins above the position cut and not purged, the second;
  with a purge answered "unknown recording" returned as a fault, the third, whose
  call returns the driver's code 21.
- A sixth test, of the sync a member asks for. The mark of the last sync asked is
  one recording's. A member follows term 0 to 192, where its last request was made,
  truncates to 96 and follows term 1 from there: the new recording writes 192 with
  its first entry. Taken in hand, that recording has no mark, so its sync is asked;
  with the mark left at 192 the request was skipped and the entry counted toward a
  commit only at the recorder's own sync, up to a second later. The second review
  of pull request 241 noted the mark; a truncation is what makes the position
  repeat. With the reset taken out the test fails: the mark reads 192.
- `cargo test -p cavalry-raft`: 166 passed.
- Not shown: a command that faults part way, which is read from the code and which
  Consequences leave to the fault-injection tests of ADR 0077; and a stop between
  the two steps, which Consequences leave to the change that starts a member on a
  kept log.

## Consequences

- The module refuses no operation of the core's `Io`. What still stands between
  a member started with `cavalry run` and the election is card 267's fix and the
  change that moves the module to `Consensus::with_election`, with the
  fault-injection tests of ADR 0077.
- A truncation that faults part way leaves a window. Suppose a purge or a cut
  succeeds and a later command faults: a response timeout, or a refusal of the
  driver's. The call returns with the recordings and the table cut as far as the
  commands went, and with where the log ends (`stopped_at`) and the record of terms
  as they were. Until the leader's next new leadership term asks again and the
  truncation completes, the member reports an append position and a log term that
  its recordings may no longer hold. Under ADR 0083 it canvasses with them, votes
  with them and may nominate itself on them. A replay it serves of a position
  between where its table now ends and the end it reports finds no recording and
  starts nothing, so it sends no byte that was cut.
- That window is closed by the change that moves the module to
  `Consensus::with_election`, change 5 of ADR 0100, with the fault-injection tests
  of ADR 0077, change 7 of ADR 0100. It cannot open while the module's core is
  appointed, because that core never asks for a truncation. Change 5 is the first
  in which a member over a driver truncates. Change 7 is where a truncation that
  faults part way is tested, with what the member reports until the leader asks
  again.
- A recording the table has forgotten, when a member stopped more than 1,024 in
  one process, is not cut: it stays in the catalog above the position. Nothing
  reads it until a member starts on a kept log; the change that fills the table
  from the catalog must cut or purge what lies above the log's end.
- A recording the election was never told, which the module stopped and did not
  keep (ADR 0093), stays in the catalog as it was.
- A truncation of a very large recording waits for the driver, one segment file a
  duty cycle, on the thread that runs the member's duty cycle. The tail a member
  truncates is what no quorum recorded since it last followed a leader.
- Revisit when a member over a driver starts on a kept log: the order of the two
  steps is then what a start relies on, and needs a test that stops between them.
- Revisit if the fault-injection tests of ADR 0077 show a member elected on an end
  that its recordings no longer hold: the core then holds a member in canvass while
  a truncation is pending, the other choice ADR 0083 left.

## References

- `spec/cluster.md`, "Messages during an election", new leadership term; "The
  log".
- `spec/client-protocol.md`, "Commands": truncate recording, purge recording.
- [ADR 0083](0083-truncate-to-the-next-terms-base-and-never-below-the-commit-position.md),
  the truncation in the core.
- [ADR 0091](0091-give-a-leaders-log-up-and-replay-it-over-a-driver-from-the-recordings-the-module-stopped.md),
  the table of kept recordings, and
  [ADR 0094](0094-record-a-members-replays-over-a-driver-and-count-the-live-recording-once-the-log-reaches-it.md),
  the two that overlap.
- [ADR 0031](0031-truncate-purge-and-delete-a-stopped-recording.md), the two commands.
- [ADR 0100](0100-move-the-module-to-the-election-in-ordered-changes-around-one-that-takes-the-appointed-path-out.md),
  change 5, the move, and change 7, the fault-injection tests of
  [ADR 0077](0077-elect-the-leader-by-vote-and-close-a-ballot-on-the-members-heard.md),
  where a truncation that faults part way is tested.
