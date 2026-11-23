# 0148. Keep a member whose truncation faulted out of the log until it finishes the truncation

- **Status:** Accepted
- **Date:** 2026-11-23
- **Invariants affected:** 1, 3, 4, 6
- **Supersedes:** of ADR 0095's Consequences, "Until the leader's next new leadership
  term asks again and the truncation completes, the member reports an append position
  and a log term that its recordings may no longer hold", for what the member may do
  with them meanwhile; and of ADR 0108's Consequences, the entry that leaves a member
  that leads before a leader has asked again to list a recording past the cut. No
  message changes, so `spec/cluster.md` stays at version 1, and no file changes, so
  `spec/durable-log.md` stays at version 6.

## Context

A cluster member truncates its log on a new leader's word, when it holds an
uncommitted tail of an older term that the leader's log lacks (ADR 0083). Over a
driver that is one command for each recording, every `purge recording` from the last
recording back and then each `truncate recording`, then the member's log recordings
file and then its record of terms, each write synced (ADR 0095, ADR 0108). When a
command or a write faults after an earlier command was sent, `truncate_log` in
`crates/cavalry-raft/src/module.rs` returns with its table of recordings cut as far as
the commands went, and its file and its record of terms as far as its steps went.
Where the log ends is as it was. The fault comes back out of the member's duty cycle, `cavalry run` prints it and
runs the next cycle, and the member goes on reporting the append position and the log
term it had.

`spec/cluster.md` says nothing of that member until its leader's next new leadership
term asks again. Two things can come first (work item 277, found by the review of ADR
0108's second change):

- The member stands in an election with an end it no longer holds, and wins. As
  leader it cannot replay the span it purged to a follower that lacks it, so the
  cluster makes no progress under it until another member leads.
- It adds a log or joins one at the end it reports, so its log recordings file names a
  recording that begins past the end of the one before it. Its next start refuses that
  file, `listed-recording` (ADR 0108), and a fault that lasted one cycle has lost the
  member for good.

The same holds for a member that joins a leader's log, or replicates a term, at that
end: its new recording begins past the cut.

## Decision

A member whose truncation faults owes it. The core keeps the position it was asked to
truncate to, in memory alone, from the fault until a truncation to that position
finishes. The member fails its election at the fault, and at any later fault of it
that finds the member out of init. While the truncation is owed:

- the member asks for it again at the start of each duty cycle;
- before it reads a new leadership term it asks for it again too, and then reads the
  message against the log the finished truncation left;
- it does not go to nominate, and reads no new leadership term as long as the
  truncation is still owed.

So it neither leads, joins a log nor replicates a term on an end it no longer holds,
and its log recordings file never names a recording that begins past a cut. A
truncation asked while one is owed is asked only once that one has finished, so it
never cuts above the owed position, where the table may end lower than the position
asked. `cluster.truncations_faulted` counts each truncation that becomes owed.
`cluster.truncation_owed` gives the owed position, −1 while none is owed.
`cavalry doctor` names a member that owes one. A member started again owes nothing:
its start makes the log directory agree with itself, as after a stop inside a
truncation (ADR 0108): what the fault left uncut stays in its log, uncommitted, and the
next leader asks for its truncation.

## Alternatives considered

- **Wait for the leader to ask again, the rule the review proposed.** The member
  would remember that a truncation is owed and keep out of the log until a new
  leadership term asked for a truncation that then finished. It lost for two reasons.
  A leader whose log holds the member's tail never asks: a leader of a later term that
  the member's old leader's voters elected again holds what the member reports, so
  the member would wait until it was started again. And a fault that lasts says
  nothing after its first line: the member's cycles succeed while it canvasses, so
  `cavalry run` never gives it up and the log directory never gets the start that
  makes it agree with itself. Asking again each cycle finishes a fault that lasted
  one cycle at the next cycle, and keeps a fault that lasts failing every cycle, so
  that `cavalry run` gives the member up at its bound, and the next start makes its
  directory agree with itself.
- **Write that a truncation is owed into the log directory before the first
  command.** A field of the record of terms or of the log recordings file. It lost
  because a start already brings every state a faulted truncation leaves back to a log
  that agrees with its record, ADR 0108's finish of a truncation cut short. A field
  would change `spec/durable-log.md` and the upgrade gate to say nothing a start does
  not already know.
- **Report the end the table holds after the fault.** It lost because the member's
  record of terms and its file would still name what the table no longer holds, and
  a new leadership term is read against the record. Finishing the truncation is what
  makes the three agree.
- **Stop the member at the fault.** A fault of the duty cycle that `cavalry run` does
  not retry. It lost because the faults that end a truncation are often transient: a
  replay still reading a recording is refused `recording busy`, and a driver slow to
  answer is a response timeout. Stopping costs a start and leaves the cluster one
  member short meanwhile, where the next cycle finishes the truncation.

## Evidence

All on an Apple M5 Pro laptop, 18 cores, on 2026-11-23.

- **The search in memory**, `crates/cavalry-raft/tests/faulted_truncation_search.rs`:
  - The model: three members under drawn loss and delay. The leader is cut off from
    both others for 11 s to 19 s, so that it appends a tail the next leader lacks. A
    leader is halted within 300 ms of its election, in one election of three. There
    are halts, partitions, starts again on the log directory kept and recording ends.
    Truncations fault at a drawn member, at the second step, the file or the record of
    terms, for 1 to 3,000 attempts in a row.
  - The check of every step: `elected_cluster`'s, and three of its own. No member
    adds, joins or replays a log while its log is cut short of the end it reports, no
    member's log has a gap, and no member leads while it owes a truncation.
  - With the code before this decision, where a fault returned and nothing more,
    seeds 13 and 96 of the first 300 fail the first of the three. At seed 13, 40,437
    ms in, member 1 joined a log. At seed 96, 53,671 ms in, member 0 did. With this
    decision both pass, and they are two of the four seeds `just test` runs.
  - With this decision, 3,000 seeds of 60 s each in a release build pass, in 1,817
    s: 9,085 leaders cut off, 4,981 leaders halted after their election, 4,874
    truncations that became owed and 4,849 that then finished with the member
    running; 614 seeds faulted none. An earlier run of 1,052 seeds, on this change
    before its review's fixes, passed as well.
  - Before a leader halted just after its election was drawn, 40 seeds found nothing
    with the rule taken out. The member that a cut-off leader leaves to truncate is
    the old leader, and no voter that recorded the new term votes for its log of the
    older term. The gap needs the third member not yet to hold the new term, which a
    leader halted just after its election leaves.
- **On one core**, `crates/cavalry-raft/tests/faulted_truncation.rs`, four tests:
  - the truncation is owed, asked for each cycle, counted once and finished once;
  - a member that owes one neither nominates nor takes a leader;
  - a new leadership term is read once the owed truncation finishes, and never cut to
    above it;
  - a member that canvasses while its truncation is under way goes back to init at
    the next fault.

  Each fails with the rule taken out.
- **On a driver**, `module::tests::a_truncation_below_a_recording_purges_it_and_finishes_when_a_command_faulted`:
  - A replay that is still starting makes the driver refuse the cut of the first
    recording, code 24 `recording busy`.
  - The purge of the second recording is done. The end, the file and the record of
    terms are as they were.
  - Once the replay has stopped, the same truncation finishes.
- **The gates:** `just lint`, `just test` (704 s), `just docs`, `just lint-docs`,
  `just conformance` (139 s) and `just replay-check` (9 s) each exit 0.

## Consequences

- A duty cycle whose truncation faults again does nothing else. A member whose
  truncation goes on faulting stays in init, where the election failed it at the
  fault, and reads and sends nothing: the other members elect without it, as without
  a stopped member, until `cavalry run` gives it up at its bound and it is started
  again.
- A member whose truncation is under way, its recording not yet given up (ADR
  0103), canvasses meanwhile. It answers request vote and sends canvass position with
  the log it reported before the fault. That log holds at least what its recordings
  hold, so a vote it casts goes to no candidate whose log is behind its own. Its
  report can hold another member back from nominating at once, for one canvass
  deadline at most.
- A member that owes a truncation and is below its vote floor (ADR 0145) does both.
  The floor's rule answers a request vote. A new leadership term is read once the
  truncation has finished, and then by the floor's rule. Neither member nominates.
- A member that learned a commit position above the owed position would truncate
  nothing and stay owed, counted in `cluster.truncations_refused`. None can: a member
  that owes a truncation takes no leader, and only a member that follows reads a
  commit position.
- A fault of the drop that follows a truncation, ADR 0096's `drop_terms_above`, is
  as it was: the truncation finished, the log ends where it was cut, and the record
  holds a term above the one the leader named until the drop is asked again.
- `crates/cavalry-raft/src/memory.rs` cuts a member's log in the driver's order and
  can make a truncation fault at its second command, at its file or at its record of
  terms, so that a test in memory reaches the state a driver leaves.

## References

- Work item 277 on the Brigade Board, which carried this decision; a part of feature 3
  of the beta slice, work item 363 (ADR 0138).
- `spec/cluster.md`, "Messages during an election", "Roles and the election" state 1
  and "Counters".
- [ADR 0083](0083-truncate-to-the-next-terms-base-and-never-below-the-commit-position.md),
  [ADR 0095](0095-truncate-a-members-log-over-a-driver-recording-by-recording-then-its-record-of-terms.md),
  [ADR 0096](0096-answer-a-member-for-a-term-the-leaders-record-holds-and-drop-the-terms-its-leaders-lacks.md),
  [ADR 0108](0108-list-the-recordings-of-a-members-log-in-its-log-directory-and-start-a-member-on-the-log-it-kept.md),
  [ADR 0145](0145-withhold-the-vote-of-a-member-that-lost-its-log-until-it-holds-what-the-cluster-held.md).
