# 0091. Give a leader's log up, and replay it over a driver, from the recordings the module stopped

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. No page of `spec/` changes: the operations are the core's
  `Io`, and what they do over a driver is what `spec/cluster.md` already says a
  leader does ("Messages outside an election"; state 0) with commands
  `spec/client-protocol.md` already has.

## Context

ADR 0081 built the election in the core, one step at a time, and left the consensus
module on the appointed leader until its I/O can do what an elected member needs.
The core's steps are on `master`. The module's `Io` refuses nine operations as
`unsupported`. This is the first slice of that I/O: the four a leader needs. A
leader that holds an election gives its log up in init (`remove_log`, ADR 0082), and
a leader serves a member that is behind with replays of its own recordings
(`start_replay`, `stop_replay`, `replay_stopped`; ADR 0085, ADR 0087).

The module's member is appointed until the last change of ADR 0081, so its core
asks for none of the four. They are built and tested now so that the change that
moves the module is the move alone.

Three things the module does not have yet decide the shape.

**It does not know its own recordings.** A member's log is its recordings of stream
100 in position order (`spec/cluster.md`, "The log"). The module holds the handle
of the recording it is making and one number, where the contiguous span of the
recordings it has stopped ends. A replay needs the recording that holds a position.

**A leader's recording outlives nothing.** The module can add a leader's log once:
a second term in one process is refused, because the publication and the recording
of the first are still held.

**A fault hides the counters.** `ConsensusModule::do_work` returns the core's fault
before it stores the member's counters, which ADR 0085 left to this change: a
replay that cannot be started faults every duty cycle while it is owed.

## Decision

**The module keeps the recordings it stopped.** Each recording of the log that the
module stops goes into a table with its recording id, the position it begins at and
the position it stopped at: a follower's when it leaves the log, a leader's when it
gives the log up. The table is in position order, since a member's recordings are
made in that order. It holds the last 1,024. When it is full the oldest is
forgotten: the recording stays in the driver's catalog and on the disk, and the
module no longer replays it.

**`remove_log` stops the recording, then closes the publication.** The recording's
stop position is where the member's log ends, as a follower's is when it leaves the
log (ADR 0060): what the leader appended and its recorder had not written is not in
its log, and it never reported it. The stop is asked once and waited for within the
stop timeout, 1 s; then the publication is closed, and the module forgets the term
it was for, so that the next term the member leads gets a publication of its own at
that term's base. A fault at either step leaves the rest for the next duty cycle,
which init tries again.

**`start_replay` replays one recording.** The module finds the recording that holds
the position: among the recordings it stopped, the one that begins at or below the
position and stopped above it; else the leader's live recording, when the position
is at or above the base its publication was created at. It asks the driver to
replay that recording from the position onto stream 100 at
`cavalry:udp?endpoint=<the member's catchup endpoint>`, the endpoint this member's
own members string gives the member (ADR 0085). With an end, the replay's length is
to that end or to the recording's stop, whichever comes first, so a replication
that spans two recordings is two replays, as the core serves it. With no end, a
catch-up, it follows the live recording. No recording holding the position starts
nothing and returns `false`: the position is below what the table still holds, or
above the member's log.

**One replay a member, by its handle.** The module holds the handle of the replay
it started for each member, eight at most. `stop_replay` sends `stop replay` for it.
`replay_stopped` reads the handle: once the driver has said the replay ended, it
returns the position reached and whether the reason was that the replay reached its
end, and forgets the handle, so the answer is given once. A replay that ended
because its publication had no subscriber, or failed, is one that did not end by
itself at its end.

**These calls wait for the driver.** `start replay` and `stop replay` are answered
commands, and the module waits for the answer within the client's driver timeout,
as it does for the publication and the recording of `add_log`. They run in an
election or for a member that is behind, never once an entry of the log.

**`do_work` stores the counters whatever the core returned.** The core's fault is
returned after the member's counters are stored, so a member that faults at a step
it tries again each cycle still shows its role, its state and its counts.

## Alternatives considered

- **Read the driver's catalog for the recording that holds a position.** Lost for
  now. The catalog holds every recording of the driver, and which of them are this
  member's log is what the module knows. A member that starts on a log it kept
  (ADR 0089) will have to read its recordings back from somewhere, and that is the
  catalog: the read comes with that change, and fills this table.
- **A table with no bound.** Lost (invariant 3). A member makes a recording each
  time it leads a term, joins a leader's log or receives a replay, for the life of
  its process.
- **Refuse a new recording when the table is full.** Lost. It would stop a member
  from leading or following to keep its ability to replay its oldest recording. A
  position the leader cannot replay is already a case the member's side has: its
  timer counts it in `cluster.log_replications_unavailable`.
- **Close the publication first and let the recording end with it.** Lost. The
  module would have to tell a recording the driver ended from one still running,
  and the log's end would be whatever the recorder had written when the stream
  ended. Asking for the stop makes the end a position the driver names.
- **Replay across recordings in the module.** Lost. The core already serves a
  replication one recording at a time and starts the next when the member has
  reported the end of the last (ADR 0085); the module would repeat that rule.

## Evidence

@@EVIDENCE@@

## Consequences

- A leader over a driver can give its log up and lead a second term in one
  process, and can replay what it holds to a member's catch-up endpoint. Nothing
  asks it to until the module moves to the election.
- The module refuses five operations where it refused nine: `truncate_log`,
  `add_catchup`, `catchup_image`, `remove_catchup` and the catch-up position it
  does not send. They are the member's side and the next slices.
- A member whose table has forgotten a recording cannot replay it. With 1,024
  recordings that is a member that has led or joined a log more than a thousand
  times in one process.
- Revisit when a member over a driver starts on a log it kept: the table is then
  filled from the catalog, and its bound is the catalog's to give.

## References

- `spec/cluster.md`, "The log", "Roles and the election", state 0, and "Messages
  outside an election".
- `spec/client-protocol.md`, "Commands": start replay, stop replay, stop recording,
  remove publication.
- [ADR 0081](0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md),
  the order in which the election reaches the module.
- [ADR 0082](0082-give-up-the-lead-on-a-higher-term-or-a-lost-quorum.md), a leader
  that gives its log up in init.
- [ADR 0085](0085-serve-a-replication-from-the-leaders-core-to-the-endpoint-the-members-string-names.md),
  the replays a leader's core asks for, and the endpoint they go to.
- [ADR 0060](0060-replicate-the-log-with-an-appointed-leader.md), where a member's
  log ends when it stops a recording.
