# 0167. Have doctor name a leader beside which no service runs

- **Status:** Accepted
- **Date:** 2026-11-25
- **Invariants affected:** 4, 6
- **Supersedes:** none

## Context

`spec/cluster.md`, "The service", "Timers", says that timers wait while no service
runs beside the leader that has told it of them, and that `cavalry doctor` on the
leader's host says so. Doctor did not. Its finding for a leader that is told of no
timer needs the gauge of the service beside the leader, `cluster.service_timers`,
to be above 0 while the leader holds none. A service that stops closes its
counters, so the gauge is gone exactly when the program beside the leader has
stopped. That is the common case: the leader keeps the timers it was last told of,
appends one event at the most for each, and the program beside every other member
waits. Work item 419's how-to told its reader to find the stopped program by
elimination. The review of pull request 367 asked for this to be closed, and work
item 464 carries it.

These facts bound what doctor can read on the leader's host.

- The member's table of timers is no state of the log (ADR 0151). A member that
  started again holds none, whatever the programs beside the other members hold.
- A service is a client of its member's driver, and it adds counters there, each
  keyed by the member's id: `cluster.replies_dropped` when it first opens the
  member's log; two for a start from a snapshot, before it reads the snapshot; and
  five together, two of its snapshots and three of its timers, at the first of its
  first snapshot, the first thing it has to tell its member of a timer and the
  first timer event it passes over (`spec/cluster.md`, "Counters", "Snapshots",
  "What a service tells its member"; ADR 0118, ADR 0159). It holds each to its
  close. The driver frees a client's counters when the client closes, and when it
  times out.
- So the first of them is in the file from a service's first open of the log to
  its end, whatever its program schedules or snapshots.
- The gauge `cluster.service_timers` alone does not say that a service runs: a
  program with no timer pending since its start has not added it.

## Decision

Doctor takes a member to have a service beside it when the driver's counters hold
any one of the eight counters that a service adds, keyed by that member's id. For a
member that leads with its election closed and has none, it prints
`cluster-no-service`. The line is a warning when the member's `cluster.timers` is
above 0: a service told it of those timers and is gone, so the program beside this
member has stopped. It says what waits, and to start the program. The line is a
note when the member holds no timer, or is of a build without the gauge: it says
that the timers and the replies of a program that runs beside the other members
wait, and that a cluster with no program yet has nothing to start. No counter is
added, no page of `spec/` changes, and no member or service does anything it did
not do.

## Alternatives considered

- **Narrow the spec's sentence to what doctor said.** It lost. Until a leader
  gives the lead up when no service runs beside it (work item 364), this line is
  the one thing that tells an operator why every program waits.
- **Warn when the leader holds timers and the gauge `cluster.service_timers` is
  absent.** It lost because the gauge's absence does not mean that no service
  runs. A program that was started again and has no timer pending does not add
  it, while its member still holds a timer whose event it appended and for which
  the stopped service sent no cancel. The warning would stay for as long as that
  member runs, beside a program that works.
- **Read the service's subscription on the service log stream, the
  `subscription.position` records of stream 104.** This was the first version of
  this change, and its independent read refused it. A position is one for each
  replay a subscription reads, and not one for a service's life: a service holds
  none while it loads a snapshot, which it reads on stream 109, and none between
  two replays. And any program that reads the committed log holds one, as
  `crates/cavalry/examples/cluster-service.rs` does and as
  `docs/how-to/read-the-committed-log.md` has its reader do, though it answers
  nothing and tells its member of no timer. The service's own counters have
  neither fault.
- **A sign of its own from the member: a gauge of when it was last told, or a
  heartbeat from the service.** A service with no timer pending tells its member
  nothing, so this needs a new message on the consensus module stream, a change
  to the wire, and a store by the member in every duty cycle. It is what work
  item 364 needs for the member itself to act, and doctor's check moves to that
  sign when it exists. For a line of doctor alone it lost to records the driver
  already keeps. In Aeron, a clustered service writes an activity timestamp into
  its cluster mark file as it works (`ClusteredServiceAgent`,
  `ClusterMarkFile.updateActivityTimestamp`), which is a sign from the service
  too; what in Aeron reads it was not verified for this ADR.
- **A warning for every leader with no service.** It lost to the note. A cluster
  that runs no program yet, as in steps 1 to 3 of the cluster tutorial, is not a
  fault, and the leader's host cannot tell it from a cluster whose other members'
  programs wait.
- **A check of its own name for each of the two lines.** It lost to one name with
  two levels, as `cluster.control_toggle` has (ADR 0147): the fact is one, and a
  dashboard that filters on the level sees the warning alone. The name is not
  `cluster.timers`, which is the name of the finding for a leader that holds no
  timer beside a service that has some.

## Evidence

All from one laptop, macOS on Apple silicon.

- `conformance/python/howto_timers.py` runs the how-to's program beside three
  members of its own and reads doctor at each step. Before any program runs,
  doctor beside the leader prints the note, and beside each follower it does not.
  With the program running beside every member it prints neither line. With the
  leader's program stopped by Ctrl-C it prints the warning, and beside a follower
  nothing of it. Once the program runs again it prints neither line. The runner
  also fails when the how-to's page does not quote the warning's words.
- A program that is killed outright, with no close, is seen when its client has
  timed out. The driver times a client out 10 s after its last command, and a
  client that sends nothing else sends a keepalive every 500 ms
  (`spec/client-protocol.md`, "Liveness and timeouts"), so that is about 10 s
  after the kill. In three runs of a copy of
  the runner that sent SIGKILL where the runner sends Ctrl-C, the warning came
  9.6 s, 10.0 s and 9.7 s after the kill, by a clock that doctor's own run of
  about 0.1 s is part of. That copy is in no file of this change, and no test
  holds the kill.
- `a_leader_beside_which_no_service_runs_is_named` in
  `crates/cavalry-control/src/doctor.rs` holds both lines word for word. It names
  the eight counters in a list of its own and holds that each alone is a service
  that runs; that a leader which holds every other counter of the cluster that is
  keyed by a member, and none of the eight, has the warning; that another
  member's service's counter and a client's session's counter are none; and that a
  follower and a leader whose election is open get neither line.
- That a service adds its first counter whatever its program does is held by
  `crates/cavalry-client/tests/cluster_service.rs`, in
  `the_service_beside_a_terms_leader_answers_its_entries_and_no_other_terms` and
  the test after it: the service's `cluster.replies_dropped` is absent before the
  service's first open of the log and 0 after it, for a program that takes no
  snapshot and schedules no timer. The how-to's program schedules timers, so the
  runner does not show this.
- Neither line holds the words "cluster member", by which
  `docs/tutorials/run-a-cluster.md` picks each member's own note out of doctor's
  output; the test holds that too.

## Consequences

- The spec's sentence is true: doctor on the leader's host says that no service
  runs beside the leader, and what waits.
- Every leader of a cluster that runs no program has one more note.
- A leader whose member started again and whose program did not holds no timer,
  and gets the note and not the warning, though every program's timers may wait:
  the leader's host does not hold what would tell the two apart.
- A program that every host's operator stopped on purpose leaves the warning
  beside the leader until its member starts again or a program does. The line
  says "where the program runs beside the other members".
- A killed program is seen about 10 s after the kill, and not before.
- Doctor says nothing of a service that has opened its log once and reads no
  more: one whose handler does not return, one that loads a large snapshot, one
  whose later open of the log the driver keeps refusing. Its client is alive and
  its counters are in the file. Doctor's older finding, a leader that holds no
  timer beside a service that has some, names a part of these.
- A service that has just started has no counter until it has opened its
  member's log once, or begun a start from a snapshot. Both lines say to run
  doctor again. A service whose first open never succeeds, with no start from a
  snapshot before it, stays in that state: the driver's replays are all in use,
  the driver refuses the counter, or the position it starts from is purged. Doctor
  then says at every run that the program has stopped, where it runs and reads
  nothing. The program's own poll reports each of those refusals to it.
- With two services beside one member, both lines come only when both have
  stopped.
- A service of a build before ADR 0118 adds none of the eight, and this doctor
  gives its leader the note or the warning. A member, its driver and its service
  are of one build (`spec/cluster.md`, "Versions and the upgrade").
- A program that reads the committed log and is no service adds none of the
  eight counters, so doctor does not take it for one.
- A counter that a later version has a service add is added to doctor's list, or
  a service that has added that one alone is not seen. The list's comment says so.
- Doctor on a follower's host says nothing of a follower with no service: only
  that member's own program waits.
- Revisit when work item 364 gives the member its own sign that a service runs
  beside it: doctor then reads that sign.

## References

- Work item 464, which this decision was made in; work item 419, whose how-to
  found it; work item 364, which will give the member a sign of its own.
- `spec/cluster.md`, "The service", "Timers", "The leader's timer events";
  "Replies"; "Counters".
- [ADR 0118](0118-reply-to-a-session-from-the-service-beside-the-entrys-leader-and-wait-for-no-client-past-the-keepalive-interval.md),
  [ADR 0147](0147-ask-a-member-for-a-snapshot-with-a-control-toggle-that-the-asking-command-owns.md),
  [ADR 0151](0151-give-a-clusters-service-timers-that-every-service-decides-from-the-log.md),
  [ADR 0159](0159-report-to-the-program-a-publication-the-driver-refuses-while-a-service-tells-its-member-of-a-timer.md).
- [Run a cluster of three members](../how-to/run-a-cluster.md), "When it does not
  work"; [Schedule a timer in a cluster's service](../how-to/schedule-a-services-timer.md),
  "When it does not work".
