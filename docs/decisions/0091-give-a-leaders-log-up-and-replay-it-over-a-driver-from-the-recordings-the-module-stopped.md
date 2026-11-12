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
The core's steps are on `master`. The module's `Io` refuses eight operations as
`unsupported` and sends no catch-up position. This is the first slice of that I/O:
the four a leader needs. A
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
catch-up, it follows the live recording, and a position in a recording that has
stopped starts nothing: a catch-up is within the term the member leads (ADR 0087),
and a member behind that term replicates, with an end. A catch-up may begin where
the live recording ends now, with nothing to bring yet. No recording holding the
position starts nothing and returns `false`: the position is below what the table
still holds, or above the member's log.

**One replay a member, by its handle.** The module holds the handle of the replay
it started for each other member of the cluster, seven at most. `stop_replay` sends
`stop replay` for it.
A replay asked for a member whose last one has not ended stops that one first and
starts once it has stopped. The core leaves that order to the I/O for one case: a
leader that asked a replay to stop, held an election and leads again does not wait
for the stop before it serves the member (`Io::start_replay`). Over a driver the
order needs no state: the driver answers `stop replay` once its replayer has
written the replay's last frame (`spec/client-protocol.md`, "Commands"), the module
waits for that answer, and a second stop of one replay is answered at once.
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
- **Count a recording the table forgets.** Not in this change. Nothing reads the
  table until the module moves to the election (ADR 0081): an appointed leader
  serves no replay, so a table that fills, on a follower that has left its leader's
  log 1,024 times, costs nothing yet. A counter is a name and an id that stay, and
  this bound is to be revisited before any member can meet it: once the table is
  filled from the catalog its bound is the catalog's (Consequences, below). If the
  change that moves the module keeps a bound under which a recording can be
  forgotten, the counter and its `doctor` text come with that change. A member that
  asks for a position its leader no longer replays counts it on its own side, in
  `cluster.log_replications_unavailable`.
- **Close the publication first and let the recording end with it.** Lost. The
  module would have to tell a recording the driver ended from one still running,
  and the log's end would be whatever the recorder had written when the stream
  ended. Asking for the stop makes the end a position the driver names.
- **Replay across recordings in the module.** Lost. The core already serves a
  replication one recording at a time and starts the next when the member has
  reported the end of the last (ADR 0085); the module would repeat that rule.

## Evidence

- `crates/cavalry-raft/src/module/tests.rs`, six tests that drive the module's
  `Io` over a driver, with the other member played by the test: a subscription on
  its `log` endpoint and one on its `catchup` endpoint.
  - A leader appends two entries and gives its log up. Its publication, its
    recording and its term are gone; its append position is the recording's stop;
    the table holds that recording from 0 to the stop. A second call does nothing.
    The member then leads term 1 from that position, on a publication created
    there and a recording of its own, which the module refused before; given up,
    the table holds both recordings in order.
  - With the two recordings kept: a replay from 0 to the end of the second joins
    the member's subscription at 0, brings the first recording's two entries and
    ends by itself at the first recording's stop. One from there to the same end
    brings the third entry and ends at the end asked. One from the middle of the
    first recording to its stop brings one entry. Each end is handed over once.
    The log's end, a position past it, a member the cluster does not have, and a
    replay with no end from a recording that has stopped start nothing.
  - A replay with no end joins at 0 on the live recording, brings the entry
    appended before it and the one appended after, and ends when the leader stops
    it, at the second entry's end and not by itself. A stop for a member with no
    replay does nothing.
  - A replay with no end asked while one runs to the same member, from where the
    leader's log ends: the first replay's image goes from the member's
    subscription, which it does only once it is stopped; the second's joins at the
    log's end and brings the entry appended after; the module says no end of the
    first. With the stop taken out of `start_replay` the test fails at the first
    replay's image, which stays.
  - A replay that no subscription takes ends after the replay connect timeout,
    5 s, at the position it began at, and not by itself.
  - The table holds 1,024 recordings without growing: one more forgets the oldest,
    whose position then finds no recording, and the others are found in order.
- The six ran fifteen times in a row on a macOS laptop with no failure, 5.3 s a
  run, most of it the replay connect timeout.
- `cargo test -p cavalry-raft`: 144 passed. The tests over three drivers, `cluster`,
  pass unchanged: the module's core is appointed and calls none of the four.
- `do_work` storing the counters before it returns the core's fault is read from
  the code and not shown by a test: no fault of an appointed core is one the
  module's tests can make after a duty cycle has changed a count.

## Consequences

- A leader over a driver can give its log up and lead a second term in one
  process, and can replay what it holds to a member's catch-up endpoint. Nothing
  asks it to until the module moves to the election.
- The module refuses four operations where it refused eight, `truncate_log`,
  `add_catchup`, `catchup_image` and `remove_catchup`, and still sends no catch-up
  position. They are the member's side and the next slices.
- A member whose table has forgotten a recording cannot replay it. With 1,024
  recordings that is a member that has led or joined a log more than a thousand
  times in one process.
- Revisit when a member over a driver starts on a log it kept: the table is then
  filled from the catalog, and its bound is the catalog's to give.
- Revisit when the module moves to the election (ADR 0081): if a recording can still
  be forgotten then, that change adds the counter for a recording the table forgets
  and its `doctor` text (Alternatives considered).

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
