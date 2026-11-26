# 0166. Have doctor name a leader beside which no service reads the log

- **Status:** Accepted
- **Date:** 2026-11-26
- **Invariants affected:** 4, 6
- **Supersedes:** none

## Context

`spec/cluster.md`, "The service", "Timers", says that timers wait while no service
runs beside the leader that has told it of them, and that `cavalry doctor` on the
leader's host says so. Doctor did not. Its one finding on timers needs the gauge
of the service beside the leader, `cluster.service_timers`, to be above 0 while the
leader holds none. A service that stops closes its counters, so the gauge is gone
exactly when the program beside the leader has stopped. That is the common case:
the leader keeps the timers it was last told of, appends one event at the most for
each, and the program beside every other member waits. Work item 419's how-to
told its reader to find the stopped program by elimination. The review of pull
request 367 asked for this to be closed, and work item 464 carries it.

Three facts bound what doctor can read on the leader's host.

- A service's counters do not say that it runs. A service adds them with its
  first snapshot, the first thing it has to tell its member of a timer, or the
  first timer event it passes over (`spec/cluster.md`, "What a service tells its
  member"; ADR 0159). A program that has nothing pending adds none.
- The member's table of timers is no state of the log (ADR 0151). A member that
  started again holds none, whatever the programs beside the other members hold.
- A service reads its member's log through a replay into a subscription of its
  own on the service log stream, stream 104 on `cavalry:ipc`, from its first read
  to its close (ADR 0066). Nothing else subscribes to that stream: the member's
  own replay is stream 103, and a snapshot's is stream 109. The driver keeps a
  `subscription.position` for each stream a subscription reads, keyed by the
  stream id, and frees it when the subscription closes or its client times out.

## Decision

Doctor counts the `subscription.position` records of stream 104 in the driver's
counters file. For a member that leads with its election closed and with no such
record, it prints one of two lines. When the member's `cluster.timers` is above 0,
a service told it of them and reads no more: `WARN cluster.timers`, with the count,
"no service reads its log on this host", what waits, and the instruction to start
the program beside this member. When the member holds no timer, or is of a build
without the gauge: `note cluster-no-service`, which says that the timers and the
replies of a program that runs beside the other members wait, and that a cluster
with no program yet has nothing to start. No counter is added, no page of `spec/`
changes, and no member or service does anything it did not do.

## Alternatives considered

- **Narrow the spec's sentence to what doctor said.** It lost. Until a leader
  gives the lead up when no service runs beside it (work item 364), this line is
  the one thing that tells an operator why every program waits.
- **Warn when the leader holds timers and the service's gauge is absent.** No new
  reading at all. It lost because the gauge's absence does not mean that no
  service runs. A program that was started again and has no timer pending adds
  no counter, while its member still holds a timer whose event it appended and
  for which the stopped service sent no cancel. The warning would stay for as
  long as that member runs, beside a program that works.
- **Have a service add its counters when it starts.** The gauge would then mean
  that a service runs. It lost: it changes what ADR 0159 fixed, when a service
  first asks its driver for anything beyond its subscription, for every program,
  and it says no more than the subscription's position already says, which a
  service holds whatever its program does.
- **A sign of its own from the member: a gauge of when it was last told, or a
  heartbeat from the service.** A service with no timer pending tells its member
  nothing, so this needs a new message on the consensus module stream, a change
  to the wire, and a store by the member in every duty cycle. It is what work
  item 364 needs for the member itself to act, and doctor's check moves to that
  sign when it exists. For a line of doctor alone it lost to a record the driver
  already keeps. In Aeron, a clustered service writes an activity timestamp into
  its cluster mark file as it works (`ClusteredServiceAgent`,
  `ClusterMarkFile.updateActivityTimestamp`); what in Aeron reads it was not
  verified for this ADR.
- **A warning for every leader with no reader.** It lost to the note: a cluster
  that runs no program yet, as in steps 1 to 3 of the cluster tutorial, is not a
  fault, and the leader's host cannot tell it from one whose other members'
  programs wait.

## Evidence

All from one laptop, macOS on Apple silicon.

- `conformance/python/howto_timers.py` runs the how-to's program beside three
  members of its own and now reads doctor at each step. Before any program runs,
  doctor beside the leader prints the note, and beside each follower it does not.
  With the program running beside every member it prints neither line. With the
  leader's program stopped by Ctrl-C it prints `WARN cluster.timers the leader,
  member 2, holds 1 of a service's timers, its cluster.timers, and no service
  reads its log on this host:`, and beside a follower nothing of it. Once the
  program runs again it prints neither line. The runner also fails when the
  how-to's page does not quote the warning's words.
- A program that is killed outright, with no close: in three runs of a copy of
  that runner that sends SIGKILL, the warning came 9.9 s, 9.8 s and 9.8 s after
  the kill. That is the client liveness timeout, 10 s
  (`spec/client-protocol.md`, "Liveness and timeouts"), after which the driver
  frees the client's subscription and its position. Three runs are no bound.
- `a_leader_beside_which_no_service_reads_its_log_is_named` in
  `crates/cavalry-control/src/doctor.rs` holds both lines word for word, and holds
  that a position on stream 103, 105 or 109 is no service's read, that a service's
  gauge that is still there changes nothing, and that a follower and a leader
  whose election is open get neither line.
- Neither line holds the words "cluster member", by which
  `docs/tutorials/run-a-cluster.md` picks each member's own note out of doctor's
  output; the test holds that too.

## Consequences

- The spec's sentence is true: doctor on the leader's host says that no service
  runs beside the leader, and names what waits.
- Every leader of a cluster that runs no program has one more note.
- A killed program is seen after the client liveness timeout, 10 s, and not
  before: until then its subscription's position is still in the file.
- A service that is finding its member's recording again, as after its member
  left the log and joined it again, may hold no replay for a moment. The warning
  asks for a second run of doctor before the operator acts.
- A second process of the same program that reads the member's log counts as a
  service, which it is: it tells the member of timers by the same rules.
- A program that is no service and subscribes to stream 104 on `cavalry:ipc` on
  a member's driver would hide both lines. The stream ids of
  `spec/cluster.md`, "Members and streams", are the cluster's.
- Doctor on a follower's host says nothing of a follower with no service: only
  that member's own program waits.
- Revisit when work item 364 gives the member its own sign that a service runs
  beside it: doctor then reads that sign, and this check goes.

## References

- Work item 464, which this decision was made in; work item 419, whose how-to
  found it; work item 364, which will give the member a sign of its own.
- `spec/cluster.md`, "The service", "Timers", "The leader's timer events";
  "Members and streams"; "Replies".
- [ADR 0066](0066-feed-the-committed-log-to-a-service-through-a-replay-of-the-members-recording.md),
  [ADR 0151](0151-give-a-clusters-service-timers-that-every-service-decides-from-the-log.md),
  [ADR 0159](0159-report-to-the-program-a-publication-the-driver-refuses-while-a-service-tells-its-member-of-a-timer.md).
- [Run a cluster of three members](../how-to/run-a-cluster.md), "When it does not
  work"; [Schedule a timer in a cluster's service](../how-to/schedule-a-services-timer.md),
  "When it does not work".
