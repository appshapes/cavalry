# 0088. Catch up within a leader's term, and join the live log from the replay

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 3, 4, 6
- **Supersedes:** ADR 0087, as its Revisit asks, in one decision: when a leader
  stops a catch-up. "What stands of ADR 0087", below, lists the rest, which is
  unchanged. Changes rows 11 and 13 of `spec/cluster.md`, "Roles and the election":
  row 11 counts the catch-up position in `cluster.catchups` when it has left; row 13
  names the leader's position the live-add window is measured from, says that "no
  progress" is an append position that has not moved, says when a report carries
  the catch-up flag, and says which commit position the member must reach. Changes
  what the catch-up flag of append position
  means, and in "Messages outside an election" when a leader sends stop catch-up
  and when a follower takes it. Adds the steps of the catch-up to the row of
  `cluster.election_steps` in "Counters". The page's version stays 0: no message's
  layout changes, no member before this change sent the flag, and no member over a
  driver takes a vote yet (ADR 0081).

## Context

ADR 0081 listed the steps of the electing path. The fifth is the catch-up within a
leader's term, states 11 to 13. ADR 0087 built the leader that answers. This is the
member that asks.

Until this change a member that had joined its leader's term, and was behind the
leader's log, failed its election in follower replay, every election status
interval, for as long as the leader led. That was the end of five of the cluster's
tests: a leader that gave up the lead and came back, a member that replicated a term
it missed, a leader whose module had stood still. In two of them, with a member
stopped, the cluster could commit nothing until the member behind could join.

Rows 10 to 14 of the page say what the member does. Building it found three things
the rows left open.

**Which position of the leader's the live-add window is measured from.** Row 13 adds
the live log "once its append position is within the live-add window of the
leader's". The member knows two positions of its leader's: the log position of the
new leadership term that sent it to follower replay, and the commit position, which
the leader goes on sending. A leader whose election has closed sends no more new
leadership terms to a member that follows it, so the first goes stale as the leader
appends.

**What "no progress" is.** Row 13 fails the election "when the catch-up makes no
progress for the leader heartbeat timeout". A member that holds all of its leader's
log, and waits for stop catch-up, receives nothing.

**The registry has none of the three states**, nor the step from follower log init
to follower ready that a member already subscribed makes (ADR 0067).

**When the leader may stop the replay.** ADR 0087 had the leader stop a catch-up
when the member reported, with the catch-up flag, an append position at or past the
leader's own, and row 13 had the member set the flag on every report of the state.
The first review of pull request 229 found what that costs, and ADR 0087 states it:
a replay of a live recording reads no further than the recording has synced
(`spec/client-protocol.md`, "Commands", start replay), while the member's live image
joins at the leader's publication's position, which under load is ahead of what the
leader has synced. A member that reports the leader's synced position before the
leader has synced past where the member's live image joined is told to stop there.
The positions between never arrive: its append position stays below its live
recording's start, its election fails after the leader heartbeat timeout, and it
begins again, into the same race. ADR 0087's Revisit asks for a test that meets the
condition. `a_catch_up_is_not_stopped_while_the_leader_holds_what_it_has_not_synced`
in `tests/election.rs` meets it in memory, and with the member as row 13 had it the
member does not join.

## Decision

A member that elects catches up as rows 10 to 14 say, through the operations of
`Io` that a replication uses (ADR 0086) and the live log's.

**Follower replay.** A member whose append position is behind the leader's log
position goes to follower catch-up init. One whose leader is appointed has no
catch-up and fails there, as before.

**Follower catch-up init.** The member subscribes on its `catchup` endpoint and
sends catch-up position with its append position, tried each duty cycle until its
publication takes it. `cluster.catchups` counts it when it has left, and the member
goes to follower catch-up await. One that has not left within the leader heartbeat
timeout fails the election.

**Follower catch-up await.** The image of the leader's replay must join at the
position the member asked from. One that joins elsewhere fails the election,
counted in `cluster.join_position_mismatches`; so does none within the leader
heartbeat timeout.

**Follower catch-up.** The member reports its append position, when it moves and
every leader heartbeat interval. It adds the live log once
its append position is within the live-add window of the greater of the log
position and the commit position its leader told it: the commit position is the
one the leader goes on sending, and it never passes the leader's log. When it reads
stop catch-up for its term and its own id, it removes its catch-up subscription.
It goes on to follower log init when three things hold: stop catch-up was read; its
append position has reached the position the live image joined at; and its append
position has reached the commit position it had been told when it read stop
catch-up. An append position that has
not moved for the leader heartbeat timeout fails the election; the time runs from
when the replay's image joined, so what the image took to come, which is follower
catch-up await's to bound, is not counted against the replay's first byte.

**The member says when the replay may stop.** A member in follower catch-up sets
the catch-up flag in a report only once it is on the live log: its live image has
joined, and its append position is at or past the position the image joined at.
Before that it reports without the flag. The report that first carries the flag is
sent in the duty cycle the member is first on the live log, whether or not its
position moved or an interval passed, so that the stop does not wait for one. The
member is the one that knows where its live image joined; the leader knows only
positions of its own, and none of them is that one.

**The leader stops on that word.** A leader that reads an append position with the
catch-up flag, for its term, asks the replay it runs to that member to stop, once,
and sends stop catch-up, at whatever position the member reports. This replaces
ADR 0087's comparison with the leader's own append position. With the member's
word the comparison guards nothing: the member holds every position below where
its live image joined, and the live log brings it the rest. It only delayed the
stop until the member's synced position was at or past the leader's, which on a
member whose sync is slower than its leader's, under a steady load, might never
be. Follower catch-up had no bound then. Now the replay brings the member to its
live recording's start at the speed of the leader's sync and the member's, and the
stop follows the first report after that.

**A member on the live log is owed no replay.** On that report the leader ends
what it held for the member as it does for one that canvasses: the replay it runs
is asked to stop, and a request it still owed the member, one whose start faulted
or one that waited for a replay being stopped, is dropped. The third review of
pull request 229 noted that such a replay would be started to a member that waits
for none, find no subscriber, end at the replay connect timeout, 5 s, and count a
catch-up nobody asked for.

**The commit position a member must reach is fixed at the stop.** It is the one
the member knew when it read stop catch-up, and the commit positions its leader
sends after that do not raise it. Row 13 had "the commit position it was told",
which rises with every commit: under a steady load, a member whose sync trails the
quorum's by more than a report's round trip would stay in follower catch-up, on the
live log and told to stop, for as long as the load lasted. The second review of
pull request 231 found it. A member on the live log is a follower like any other,
and a follower's append position trails the commit position whenever a quorum
syncs sooner than it does; nothing rests on it having caught up with a later
commit before its election closes.

**A stop catch-up before the flag is not an answer.** A member takes stop catch-up
only after it has reported with the flag in this catch-up. The leader sends it for
such a report alone, so one read sooner answers an earlier catch-up of the same
term; taking it would remove the subscription the replay arrives on.

**What stands of ADR 0087.** Everything but the comparison: a catch-up is served
from the leader's core as a replication is, one replay a member, stopped by a
canvass position, by a new request and in init; a catch-up position from the
leader's term base to its own append position starts a replay with no end, one
above it or below 0 ends what the leader held and starts nothing; every member is
served in every duty cycle and the first fault is returned after all have had
their turn; stop catch-up is sent for every report with the flag and the replay is
asked to stop once; `cluster.catchups` rises by one for each replay a leader
starts for a catch-up. ADR 0087's Status reads "Superseded by 0088", the one line
of an Accepted ADR that may change, so that a reader of it is sent here; its text
is the record of what was decided on 2026-11-11 and why.

**Follower log init, for a member already subscribed.** It goes to follower ready
with no second subscription, and its report there, without the catch-up flag,
closes its election. It does not pass follower log await: that state checks that a
new image joins at the member's append position, and this member's live image
joined at or past it by design, with the replay bringing the log up to it.

**The texts of two counters.** `cluster.join_position_mismatches` rises in follower
catch-up await too, and `cluster.elections_failed` in each of the three states: the
`doc` and the `doctor` text of the first and the `doctor` text of the second say so,
and what an operator does. That a member has no catch-up is said there of a member
started with `cavalry run`, whose leader is appointed, and no longer of the version.

**Counted, and registered.** The three states are in the machine `cluster_election`
with nine rows: the steps from follower replay through the three states to follower
log init, the step from follower log init to follower ready, and each state's
failure, follower catch-up await's with `cluster.join_position_mismatches` beside
`cluster.elections_failed`.

**In memory.** A leader's replay with no end brings a member that subscribed every
entry the leader's recording has synced from the position asked, each step, as the
leader appends and syncs, for as long as the leader holds its log publication. A
replay of either kind reads no further than the replaying member has synced, as a
replay over a driver does; before this change the in-memory replay read all the
leader had appended, and could not show the case above. A live image that a member
adds while it is behind joins above the member's log and brings it nothing until
the replay has brought the log to where it joined.

## Alternatives considered

- **Measure the live-add window from the log position of the new leadership term
  alone.** Lost. It is the leader's position when the member was told the term. A
  member far behind a leader that keeps appending would add the live log when it
  reached that old position, which can be more than the window behind the leader's
  position by then.
- **Fail a catch-up that receives nothing, whatever its append position does.**
  Lost. It is the same test for a member below its leader's log. For one that holds
  all of it, the leader's stop catch-up comes within a leader heartbeat interval of
  the member's report, and when it does not, the leader is gone and the election
  should fail.
- **Check the live image's join position again in follower log await.** Lost, for
  the reason in the decision: the image joined past the append position on purpose.
  The three conditions of follower catch-up are what make the join safe.
- **Keep the catch-up subscription until the member leaves follower catch-up.**
  Lost. The page has stop catch-up remove it, and the leader has stopped the replay
  by then.
- **End the catch-up at the leader's publication's position, as Aeron does.** Lost.
  It is the other way ADR 0087's Revisit names, and it is safe: the publication's
  position is never below where the member's live image joined. But a member's
  append position is a synced position, which under a steady load trails the
  leader's publication by the member's own sync, so the member stays in the
  catch-up, its replay running and its recordings written twice, until the leader's
  appends pause. It also needs an operation of `Io` that reads a position no other
  rule of the core uses. The member's word needs neither.
- **Keep ADR 0087's comparison beside the member's word.** Lost. It would be a
  second condition that makes nothing safer and leaves follower catch-up with no
  bound under load, as "The leader stops on that word" says.
- **Ask again when a catch-up is stopped short.** Lost. The member would have to
  tell a replay stopped short from one that is slow, and ask from a position the
  leader may by then have passed with a new term. Not being stopped short needs no
  second request.
- **Wait for the commit position the leader goes on sending, as Aeron does.** Lost.
  In Aeron, this is `Election.followerCatchup`, which waits for the notified commit
  position, and `onCommitPosition` raises it. It leaves the state with no bound
  under load, as "The commit position a member must reach is fixed at the stop"
  says, to hold back an election whose close changes nothing a commit depends on.
- **Mark every report of follower catch-up with the flag, as Aeron does, and add a
  second bit for "on the live log".** Lost. The leader has no use for the first
  meaning: it counts a member's append position toward a commit by its term and its
  position, with or without the flag. One bit with one meaning is less to get wrong.

## Evidence

- `crates/cavalry-raft/tests/follower_catchup.rs`, nine tests on one core through
  an `Io` that remembers what it was asked.
  - A member at its leader's term base, told the leader's log is 96 bytes on,
    subscribes, then asks from its append position, counted. The image joins there.
    It reports with the leader's term and no flag, adds the live log, and reports
    with the flag once the replay has brought it past where the live image joined.
    It reads stop catch-up, removes the catch-up subscription once, and goes through
    follower log init to follower ready with one live subscription in all. Its last
    report carries no flag and closes its election: four steps.
  - A member at 160 whose live image joined at 192 reports without the flag, and
    does not take a stop catch-up; at 192 it reports with the flag in that cycle. A
    member already at the join position when the image comes reports with the flag
    at once, with no move of its position and no interval passed.
  - A member 2 MiB behind adds the live log when it is 1,048,576 bytes from the
    leader's position, and not 32 bytes sooner.
  - A catch-up position that does not leave for the leader heartbeat timeout fails
    the election and counts no catch-up.
  - An image that joins elsewhere fails, counted; none within the timeout fails.
  - A stop catch-up for another term or another member is not read, nor one that
    comes before the member has reported with the flag. A member that
    was told to stop stays while the live image is not there, while its append
    position is below where the live image joined, and while it is below the commit
    position it had been told when it read stop catch-up.
  - A member told a commit position of 192 and then to stop stays while it is below
    192, and leaves at 192, though it has been told 256 since. With the member
    waiting for the commit position the leader goes on sending, the test fails: the
    member is still in follower catch-up at 192.
  - An append position that does not move for the leader heartbeat timeout fails
    the election; init then leaves the live log and removes the subscription.
  - A member whose replay's image joins 9.9 s after it asked is still in follower
    catch-up at the leader heartbeat timeout after the asking, and fails at the
    timeout after the joining. With the time run from the asking, the test fails:
    the member has failed its election and is in init at the timeout after the
    asking.
  - An appointed member behind its leader's log fails in follower replay.
- `crates/cavalry-raft/tests/election.rs`, in memory, with the check after every
  step that no committed entry is lost. Five tests that ended with a member behind
  its leader's log now end with it joined, one catch-up counted on it, its log the
  leader's, and the next entry committed by every running member: the five-member
  history of pull request 218; the leader that was cut off; the leader that stood
  still; the leader that three of five could not hear; and the leader whose module
  stood still.
- `a_catch_up_is_not_stopped_while_the_leader_holds_what_it_has_not_synced`, in the
  same file, at three seeds. The leader's sync is paused with one entry appended
  and not synced. A member that was cut off comes back, asks, and is brought what
  the leader has synced; its live image joins past that. For a second it stays in
  follower catch-up at the leader's append position, one catch-up counted and no
  election failed, the replay still running. When the leader syncs, it joins within
  the test's bound of a second, 6 ms at each seed, and all three commit the next
  entry. With the member setting the flag on every report, the test fails: "the
  member behind joins: not within 1000 ms".
- `crates/cavalry-raft/tests/catchups.rs`, the leader on one core, eleven tests: a
  report with the flag at 128, below the leader's own 160, stops the replay and is
  answered with stop catch-up; reports without the flag at 128 and at 160 stop
  nothing. A report with the flag drops a request whose start had faulted, and one
  that waited for a replay being stopped: no replay is started for either. Before
  that change the test fails, the start that faulted made a second time.
- `crates/cavalry-raft/tests/transitions.rs` makes each of the nine new rows.
- Every other test of the crate passes unchanged.

## Consequences

- A member that comes back behind its leader, by any of the ways the electing path
  has, ends on the leader's log: it truncates what no quorum recorded, replicates
  the terms it missed, and catches up within the leader's term.
- Leader ready still waits for every member the leader has heard to count, and for
  a quorum alone only once the leader heartbeat timeout has passed in that state.
  The page's row 8 closes it on a quorum at once. The wait was there because a
  member that missed the term's first entry could not come back; it can now, so the
  wait can go, in the next change to the core.
- No operation is added to `Io`. Over a driver the catch-up's recording and the
  live one overlap from where the live image joined, and `Io::append_position` is
  the end of what the two hold together, which is the module's to keep when its I/O
  for the election is built.
- The catch-up flag means less than Aeron's and more: not "this position was
  appended in a catch-up" but "this follower, in a catch-up, is on the live log".
  In Aeron, this is `APPEND_POSITION_FLAG_CATCHUP`, set on every append position a
  follower sends while it catches up, and the leader ends the catch-up when the
  follower's position reaches its log publication's. A capture of the two
  protocols differs in which reports carry the bit.
- Follower catch-up is bounded under load: by the replay reaching the member's live
  recording's start, then one report and its answer, then the commit position the
  member had been told at that answer, which the live log brings it to. One case
  is left with no bound but the replay's own progress, as row 13 had it: a live
  image that never joins while the replay goes on moving. The member is brought
  the whole log by the replay, reports, and counts toward commits, and stays in the
  state.
- Revisit with the module's I/O for the election, over drivers: a member's
  `Io::log_image` must give the join position of the live image while the catch-up
  subscription is still there, and `Io::append_position` the end of what the two
  recordings hold together. If either cannot be had from the driver, the member
  cannot know that it is on the live log, and this decision is taken again. And
  there, whether a live image that has not joined within the leader heartbeat
  timeout of its subscription should fail the election, as follower log await
  does, so that the case above is counted and bounded.

## References

- `spec/cluster.md`, "Roles and the election", rows 10 to 16, "Messages outside an
  election", "Values" (live-add window) and "Counters".
- [ADR 0087](0087-serve-a-catch-up-from-the-leaders-core-and-stop-it-at-the-leaders-own-append-position.md),
  the leader's half.
- [ADR 0086](0086-record-a-term-when-its-replication-begins-and-check-each-replays-image-against-the-position-last-sent.md),
  the operations of `Io` a member replicates and catches up through.
- [ADR 0081](0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md),
  the electing path and its steps.
- [ADR 0067](0067-register-the-elections-state-machine-with-its-core-and-count-each-step.md),
  the registry of the election's machine.
- `spec/client-protocol.md`, "Commands", start replay: a replay of a live recording
  reads up to the synced position and waits for it to move.
- The first review of pull request 229, which found the case:
  <https://github.com/appshapes/cavalry/pull/229>
- Aeron's `ConsensusModuleAgent.trackCatchupCompletion`, which stops a follower's
  catch-up replay when the follower's log position is at or past
  `logPublisher.position()`, read on 2026-11-12:
  <https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/ConsensusModuleAgent.java>
- Aeron's `Election.followerCatchup`, which adds the live destination when the
  member is near the greater of the catch-up join position and the notified commit
  position, read on 2026-11-11:
  <https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/Election.java>
