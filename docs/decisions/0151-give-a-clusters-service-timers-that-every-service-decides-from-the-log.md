# 0151. Give a cluster's service timers that every service decides from the log

- **Status:** Accepted
- **Date:** 2026-11-22
- **Invariants affected:** 1, 2, 3, 4, 6
- **Supersedes:** none. It does what ADR 0136's Revisit asks of the first version
  that adds an entry, "for when a leader may use it", and restates one sentence of
  `spec/cluster.md`, "Versions and the upgrade", that ADR 0136 wrote and ADR 0147
  took by its letter. It takes `spec/cluster.md` from version 1 to version 2, 0.2.0.

## Context

Feature 6 gives a cluster's service timers in six languages (ADR 0138, condition 2;
work item 419). In Aeron a service calls `scheduleTimer` and `cancelTimer`, which
send the consensus module a message each; the module's timer service holds the
timers; the leader appends a timer event when one expires, and every service is
handed it in `onTimerEvent`. The module's snapshot holds the pending timers. A
module that replays a timer event cancels that timer, and counts the event when it
holds no such timer, so that the service's schedule, when it arrives, is dropped.

That design rests on things Aeron has and Cavalry does not. An Aeron consensus
module and its services start together and the module tells them where to read.
Here a service is a client of its member's driver that starts before its member,
with it or after it, from position 0 or from its own snapshot, and a member does
not know which services read its log (ADR 0066; ADR 0132). A member can start
again while its service runs on, and two services can run beside one member. So
what a member holds of the timers cannot be a function of the log: it is whatever
the services beside it happened to send, in the order it arrived. With Aeron's rule
that every timer event is handed to the program, a member that is behind, such as
a new leader whose service has not yet read the last leader's events, hands a
timer over twice, and one that is ahead hands over a timer its program cancelled.

Invariant 1 asks more: a timer handed over once, at one position, the same beside
every member, across a change of leader and a start from a snapshot, and the same
state from a replay of the log.

## Decision

The service decides, from the log alone, and the member is an alarm clock.

1. **A service holds the pending timers**, each a correlation id and a deadline in
   the cluster's time, at most 1,024. Its program schedules and cancels from inside
   its handling of an entry and nowhere else, so the table is a function of the log,
   the same beside every member. A schedule of another id at the bound is refused
   to the program by the count of pending timers alone, so every member's service
   refuses the same call. A cancel answers nothing about whether the timer was
   pending.
2. **A timer event is handed to the program only when its timer is pending with a
   deadline at or before the event's timestamp**, and the timer is then pending no
   more unless the handler schedules it again. Every other timer event is passed
   over and counted. So nothing that any member appends can hand a timer over
   twice, before its deadline, or after its cancel.
3. **The member on a service's driver is told** by Aeron's schedule timer and
   cancel timer, templates 31 and 32 with Aeron's layouts, on the consensus module
   stream. It holds what it was told, at most 2,048, as soft state: in no snapshot,
   empty at a member's start. While it leads with its election closed it appends a
   timer event, template 20, for a timer whose deadline its wall clock has reached,
   and marks the timer so that it appends no second event for it.
4. **Three rules tell the member**, and nothing else does: an unsent schedule is
   sent at a later read; an unsent cancel is held, at most 1,024, and sent at a
   later read; and a service sends every pending timer again at the end of the
   history it read at its start and at each new leadership term event after it.
   While it reads history it tells its member nothing. A timer that the member
   holds and no service does costs one event that every service passes over, and
   stays marked in the member until its place is needed.
5. **A service's snapshot holds its timers**, as Aeron's timer record, template 104,
   between its client sessions and its section marker. A snapshot with no timer
   pending is byte for byte version 1's. The member's snapshot does not change, and
   no file of `spec/durable-log.md` does.
6. **A leader of version 2 appends a timer event whatever the versions of the other
   members.** Every version of major 0 names template 20 and accepts it; a service
   of version 0 or 1 has no call that schedules, so no timer is pending in it and
   the entry changes nothing for it, as before; a leader of version 0 or 1 appends
   none, so timers wait while one leads. The page's rule that a version of a major
   changes no check of a reader and nothing an entry does to the state is restated
   for a version that adds: it may add a message, a record of a snapshot or a use
   for an entry that every version names, where no member, client or service of an
   earlier version is sent what it rejects or computes another state than it did.

Where this departs from Aeron: a passed-over event is not handed to the program,
where Aeron hands over every event and its cancel is a request that an event can
overtake; the timers are in the service's snapshot and not the consensus module's;
and a program schedules from inside a handler alone, where Aeron's can schedule at
its start. A program that needs a timer from its beginning schedules it at the
first entry it is handed, which for a service that reads from position 0 is the
log's first new leadership term event.

## Alternatives considered

- **Aeron's design as it is: the member's table is the truth, and every event is
  handed over.** Lost, for the reason of "Context": here the table is not a function
  of the log, and Aeron's count of replayed events that met no timer is a map with
  no bound that pairs a count with an id, not an event with the schedule it is for.
- **Keep the member's table exact: stamp each schedule and cancel with the position
  of the entry that made it, and have the member drop what it has taken.** Lost. It
  adds fields to Aeron's two messages, needs the timers in the member's snapshot and
  a member that reads timer events itself in step with its service, and still has
  two tables that must agree. With rule 2 the member's table need not be right.
- **The timers as counters the service owns, which a leader finds by reading the
  counters file**, as the control toggle is found (ADR 0147). Lost. A leader that
  reads 16 records a millisecond finds a timer a second late, and one that reads
  the whole file each duty cycle reads 16,384 records for nothing most of the time.
- **Hold the pending timers in the member's snapshot**, as Aeron does. Lost. A member
  with no service beside it would then append events for timers it took from its
  snapshot, which is all it gains: such a leader answers no session either, and the
  cost is a new record in the consensus module's snapshot, which the log-directory
  gate holds across versions (ADR 0141).
- **Hand every timer event to the program**, and let rule 2 decide only what clears
  a timer. Lost. A program would then see an event after its cancel and one before
  the deadline it last scheduled, each only when a leader was behind: the rarest
  path of a program would be the one it never tested.
- **Have a leader append timer events only once every member is of version 2.**
  Lost. A member holds no peer's version (ADR 0137), and nothing needs the wait:
  what a service of version 1 does with the entry is what it did.
- **A new major.** Lost. Members of two majors form no cluster, so there would be no
  upgrade one member at a time (ADR 0136).
- **A cancel that is sent once and forgotten**, the first design here. Lost. A
  program that sets a timeout for each request and cancels it when the answer comes
  cancels as often as it schedules, and a member that missed those cancels fills
  with timers that are due hours later.
- **Schedule and cancel from anywhere**, as in Aeron. Lost. A call outside the
  handling of an entry is at no position of the log.

## Evidence

- `spec/vectors/cluster.json`, 325 vectors from `spec/vectors/cluster.py`: schedule
  timer, cancel timer and the snapshot's timer, each valid and with any values; a
  rejection for each on a stream that does not carry it, for a block length and for
  trailing bytes; and 13 vectors of a new kind, `timers`, each the steps a service
  takes and what follows: a timer handed once; an event before the deadline; an
  event of no timer and one after a cancel; a timer scheduled again for later and
  for earlier; one due at once; two events for one schedule; a timer that repeats;
  a handler that does not take its event; a handler that cancels its own timer;
  several timers; and two at the bound of 1,024. `crates/cavalry-proto/tests/cluster_conformance.rs` runs every one
  against `ServiceTimers`, which holds the table in itself and allocates nothing.
- A seeded search in memory, run before this ADR's first review, in the pattern of
  `crates/cavalry-raft/tests/lost_log_search.rs`. It is
  `crates/cavalry-raft/tests/timer_search.rs`, which lands in the change after this
  one with the member's table it needs, and it was run from the branch that holds
  both. Three members of `cavalry_raft::memory::Cluster` under loss and delay, with
  members halted, cut from each other and started again, and a leader cut off from
  both others; beside each a service as `spec/cluster.md` gives it, over
  `ServiceTimers`, and the member's timers. A program schedules and cancels twelve
  ids from session messages, and three of them repeat. The seed also draws a
  service that reads late, a handler that does not take its entry, a stream that
  takes nothing for up to 4 s, a service that stops and starts from its snapshot or
  from position 0, and members' wall clocks each up to 600 ms from the cluster's.
- What the search checks at every entry: one program state at one position beside
  every member, for a first and for a second reading; and every timer event handed
  over or passed over as a replay of the committed log says, by a map of ids to
  deadlines that knows nothing of `ServiceTimers`. At the end of a seed, after
  100 s with nothing cut, lost or late: no service holds a timer whose deadline the
  leader's clock passed in the first 50 s of that time.
- 300 seeds of 60 s each, seeds 1 to 300, in a release build on an Apple M5 Pro
  laptop, 944 s: no check failed. 105,007 timer events were appended, 1,807 of them
  by a leader that was cut off or was not the cluster's; 92,920 were handed over,
  each once; services passed events over 17,247 times and left 133,534 entries not
  taken; services started 10,988 times and members 5,503 times.
- What the search changed in the rule before this ADR. A service that started beside
  a running member and read its history told its member each old schedule on the
  way, and a leader appended an event for each: a service now tells its member
  nothing while it reads history, and sends what is pending at the end of it. A
  rule that answered a passed-over event with what its timer is was taken out: 60
  seeds without it handed every timer over, since the schedule timer that gave a
  timer its later deadline always follows. A rule that took a member's mark off
  when its log was cut was taken out for the same reason: the member leads again
  only in a term whose first entry its service reads.
- The search fails when the rule is broken, each mutation run from seed 1 and
  taken out again: an event handed over whatever its time, and a handler that does
  not take its event and leaves what it did with the timer, each within the four
  seeds that `just test` runs; no timer sent again at a new leadership term event,
  at seed 41, where a timer is pending 100 s after it was due; nothing sent at the
  end of history, at seed 2, the same; a member that does not mark the timer it
  appended an event for, at seed 2, where the log never comes to rest.
- One seed of the 300, seed 107, ends with one member whose election has not
  closed, which the search leaves out of its last check and counts. That member
  asks its leader for a catch-up every ten seconds and is sent none. It is no
  timer's doing and is work item 420.
- No measurement of latency: this change adds no code that a message passes
  through. The member's check for a due timer is one compare in a duty cycle while
  nothing has changed, and comes with the member's side.

## Consequences

- A program is handed each timer once, never early and never after its cancel, at
  one position beside every member. The log may hold timer events that every
  service passes over: `cluster.service_timer_events_passed_over` counts them, and
  each costs 32 bytes of the log.
- Timers wait while no member leads, while the leader is of version 0 or 1, and
  while no service runs beside the leader, though a service runs beside every
  other member. Feature 4 (work item 364) decides when a leader steps down; a
  leader with no working service beside it is one of its cases, and until then
  `cavalry doctor` on the leader's host names it.
- The timestamps of the log's entries do not rise from entry to entry: a wall clock
  steps, and the next leader's may be behind. A timer is due by the clock of the
  member that leads, and an event's timestamp is never below the deadline its
  leader held.
- A service of version 1 that is given a snapshot holding a timer, as beside a
  member stepped back, passes the snapshot over and reads the log from position 0.
- The C ABI moves to version 7 with the two calls and the entry kind, in an ADR of
  its own, and the five clients follow.
- Revisit: when a service runs in a process that its member starts, for the member
  to hold the timers; when a program needs a timer before its first entry; when a
  major is needed, for whether a passed-over event is still appended.

## References

- Work item 419; work item 366, feature 6; work item 420, a member that the search
  left out of its last check.
- [ADR 0136](0136-run-the-versions-of-one-major-as-one-cluster-and-upgrade-from-the-last-tree-of-the-version-before.md)
  and [ADR 0137](0137-hold-a-members-version-of-the-cluster-protocol-in-a-counter-and-no-peers.md),
  the versions of a major;
  [ADR 0147](0147-ask-a-member-for-a-snapshot-with-a-control-toggle-that-the-asking-command-owns.md),
  which added no message;
  [ADR 0131](0131-have-a-service-write-its-own-snapshot-and-its-member-list-it.md)
  and
  [ADR 0132](0132-start-a-service-from-the-snapshot-its-member-names-when-its-program-asks.md),
  a service's snapshot and its start;
  [ADR 0124](0124-give-each-client-a-session-object-and-a-service-object-whose-handlers-failure-aborts-its-entry.md),
  a handler that does not take its entry.
- `spec/cluster.md`, "The service", "Timers"; "Versions and the upgrade".
- Aeron's `ClusteredServiceAgent.scheduleTimer`, `ConsensusModuleAgent.onScheduleTimer`
  and `onReplayTimerEvent`, and the codecs `ScheduleTimer`, `CancelTimer`,
  `TimerEvent` and `Timer`.
