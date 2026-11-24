# 0151. Give a cluster's service timers that every service decides from the log

- **Status:** Accepted
- **Date:** 2026-11-23
- **Invariants affected:** 1, 2, 3, 4, 6
- **Supersedes:** of ADR 0136's Decision, the sentence "Within a major no version
  changes a layout, a template's id, an enumeration's values, a check of 'What a
  reader rejects', or what an entry does to the state computed from the log", which
  point 8 restates, as `spec/cluster.md`, "Versions and the upgrade", held it and
  ADR 0147 took it by its letter; the rest of ADR 0136 stands. It also does what
  ADR 0136's Revisit asks of the first version that adds an entry, "for when a
  leader may use it", and takes `spec/cluster.md` from version 1 to version 2,
  0.2.0.

## Context

Feature 6 gives a cluster's service timers in six languages (ADR 0138, condition 2;
work item 419). In Aeron a service calls `scheduleTimer` and `cancelTimer`, which
send the consensus module a message each; the module's timer service holds the
timers; the leader appends a timer event when one expires, and every service is
handed it in `onTimerEvent`. The module's snapshot holds the pending timers. A
module that replays a timer event cancels that timer, and counts the event when it
holds no such timer, so that the service's schedule, when it arrives, is dropped.
Aeron's own words for the two calls are that each "is asynchronous and will race
with the timer expiring", and that a caller loops until the send succeeds.

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

A service decides from the log alone which timer event is handed to its program,
and the member beside it is an alarm clock whose mistakes cost an entry and no
more. The service holds the pending timers, at most 1,024, as a function of the
entries its program took; it hands a timer event over only when that timer is
pending with a deadline at or before the event's timestamp, and passes every other
event over. It tells the member on its driver what its timers are with Aeron's
schedule timer and cancel timer, and the member, while it leads, appends a timer
event for a timer whose deadline its clock has reached. What the member holds is
soft state, in no snapshot, and the service sends it every pending timer again
each second, so that nothing the member lacks or holds wrongly lasts longer. The
service's snapshot holds its timers. This is version 2 of
`spec/cluster.md`, 0.2.0, which adds to version 1 and changes nothing of it. In
detail:

1. **A service holds the pending timers**, each a correlation id and a deadline in
   the cluster's time. Its program schedules and cancels from inside its handling
   of an entry and nowhere else, so the table is a function of the log, the same
   beside every member. A schedule of another id at the bound is refused to the
   program by the count of pending timers alone, so every member's service refuses
   the same call. A cancel answers nothing. Neither call sends anything, so neither
   fails for a stream or a driver, and a program loops on neither.
2. **A handling that is not taken changes no timer.** A handler that does not take
   its entry (ADR 0124) is handed it again with every timer as it was before,
   whatever it scheduled and cancelled, and the member is told nothing of it. So
   the second handling is answered as the first was, at the bound too.
3. **A timer event is handed to the program only when its timer is pending with a
   deadline at or before the event's timestamp**, and the timer is then pending no
   more unless the last thing the handler did with its id was to schedule it.
   Every other timer event is passed over and counted. So nothing that any member
   appends can hand a timer over twice, before its deadline, or after its cancel.
4. **The member on a service's driver is told** by Aeron's schedule timer and
   cancel timer, templates 31 and 32 with Aeron's layouts, on the consensus module
   stream. It holds what it was told, at most 2,048, as soft state: in no snapshot,
   empty at a member's start. While it leads with its election closed it appends a
   timer event, template 20, for a timer whose deadline its wall clock has reached,
   and marks the timer so that it appends no second event for it. A schedule timer
   for the id takes the mark off, whatever its deadline.
5. **Two rules tell the member**, and nothing else does. After an entry is taken,
   what the entry changed: a cancel for each timer that is pending no more, held at
   most 1,024 unsent, and a schedule for each that is new or has another deadline.
   And every pending timer again, when the service has read its history, during
   which it tells its member nothing, and then every timer refresh interval, 1 s.
   The first rule tells a member soon. The second is the one its table rests on:
   whatever a member lacks, or holds with another deadline or with a mark that no
   event answers, is put right within a second of its service's reading, and no
   way for a table to go wrong has a rule of its own.
6. **A member whose table is full removes a timer it has appended an event for,
   and with none removes them all**, and the refresh brings back what is pending.
   A full table with no such timer holds more than its service has pending, so
   some of it is pending nowhere, and the member cannot tell which.
7. **A service's snapshot holds its timers**, as Aeron's timer record, template 104,
   between its client sessions and its section marker. A snapshot with no timer
   pending is byte for byte version 1's. The member's snapshot does not change, and
   no file of `spec/durable-log.md` does.
8. **A leader of version 2 appends a timer event whatever the versions of the other
   members**, and a program schedules its first timer only once every member runs
   version 2. Every version of major 0 names template 20 and accepts it, and a
   leader of version 0 or 1 appends none. A service of version 0 or 1 hands its
   program every timer event as an entry of a kind it does not know, so a program
   that schedules is another version of the application than one that cannot. The
   page's rule for the versions of a major is rewritten to say what holds: no
   version changes a layout, a template's id or an enumeration's values, and none
   changes, for a member, client or service of an earlier version, which messages
   it rejects or what an entry does to the state it computes.

Where this departs from Aeron: a passed-over event is not handed to the program,
where Aeron hands over every event and its cancel races with the expiry; the
timers are in the service's snapshot and not the consensus module's; schedule and
cancel are calls on the service's table that take effect when the entry is taken,
where Aeron's are sends that a program loops on; and a program may schedule while
it handles an entry of any kind, a new leadership term event among them, where
Aeron names four callbacks. A program that needs a timer from its beginning
schedules it at the first entry it is handed, which for a service that reads from
position 0 is the log's first new leadership term event.

## Alternatives considered

- **Aeron's design as it is: the member's table is the truth, and every event is
  handed over.** Lost, for the reason of "Context": here the table is not a function
  of the log, and Aeron's count of replayed events that met no timer is a map with
  no bound that pairs a count with an id, not an event with the schedule it is for.
- **Keep the member's table exact: stamp each schedule and cancel with the position
  of the entry that made it, and have the member drop what it has taken.** Lost. It
  adds fields to Aeron's two messages, needs the timers in the member's snapshot and
  a member that reads timer events itself in step with its service, and still has
  two tables that must agree. With point 3 the member's table need not be right.
- **The timers as counters the service owns, which a leader finds by reading the
  counters file**, as the control toggle is found (ADR 0147). Lost. A leader that
  reads 16 records a millisecond finds a timer a second late, and one that reads
  the whole file each duty cycle reads 16,384 records for nothing most of the time.
- **Hold the pending timers in the member's snapshot**, as Aeron does. Lost. A member
  with no service beside it would then append events for timers it took from its
  snapshot, which is all it gains: such a leader answers no session either, and the
  cost is a new record in the consensus module's snapshot, which the log-directory
  gate holds across versions (ADR 0141).
- **Hand every timer event to the program**, and let point 3 decide only what clears
  a timer. Lost. A program would then see an event after its cancel and one before
  the deadline it last scheduled, each only when a leader was behind: the rarest
  path of a program would be the one it never tested.
- **Let what a handler scheduled and cancelled stand when it does not take its
  entry**, the first form of this ADR, with only the event's own timer put back.
  Lost, to an independent read of that form. At the bound a handler that is
  refused a schedule, cancels another timer and does not take the entry is given
  the schedule at its second handling, beside that member alone; and a handler
  that schedules a due timer and does not take its entry had its leader append an
  event for each try.
- **Drop a schedule timer that meets a full table**, the first form's rule for the
  member. Lost, to the same read. The service had marked the timer sent, and
  nothing sent it again: a member full of timers whose cancels it never got would
  have held a pending timer back for as long as it ran.
- **Send every pending timer again at each new leadership term event**, the first
  form's rule for a member that started or whose log was cut. Lost. It sends
  1,024 messages at every election to a member that lost nothing, and a member
  that starts again while its leader stays is told nothing until the next one.
- **A rule for each way a member's table goes wrong**, the second form: a gauge in
  which a member says that it began again with no timer, at which its service
  sends everything again; an answer to each passed-over event with what its timer
  is; and a member that takes every mark off when it begins to lead. Lost, on the
  evidence below. With each of the three taken out in turn, 60 seeds of the search
  found nothing, so none had a failing run to its name; and reading them against
  a second process beside one member found a case that none covers, a cancel from
  the process that is behind, for a timer the one ahead has scheduled again, after
  which that process stops. A list of repairs is as good as the list is complete.
  The refresh needs no list.
- **A refresh that leaves the mark on a timer whose deadline is the one the member
  holds**, so that a refresh never costs an event. Lost. A mark on a timer whose
  event is in no log, and one on a timer that was handed over, never cancelled to
  the member and scheduled again for the same deadline, would each then need a
  rule of its own. What the chosen rule costs is bounded: a leader whose service
  is more than a second behind its log appends, each second, one event more for
  each timer that is due and not yet read, and every service passes it over.
- **Have a leader append timer events only once every member is of version 2.**
  Lost. A member holds no peer's version (ADR 0137), and the wait would be checked
  by the one member that can append, not by the services that differ.
- **A new major.** Lost. Members of two majors form no cluster, so there would be no
  upgrade one member at a time (ADR 0136).
- **A cancel that is sent once and forgotten.** Lost. A program that sets a timeout
  for each request and cancels it when the answer comes cancels as often as it
  schedules, and a member that missed those cancels fills with timers that are
  due hours later.

## Evidence

- `spec/vectors/cluster.json`, 335 vectors from `spec/vectors/cluster.py`: schedule
  timer, cancel timer and the snapshot's timer, each valid and with any values; a
  rejection for each on a stream that does not carry it, for a block length and for
  trailing bytes; and 23 vectors of a new kind, `timers`, each the steps a service
  takes, the timers that are pending after each and what the service then tells
  its member. Among them: a timer handed once; an event before the deadline; an
  event of no timer; a timer scheduled again; one that repeats, with one deadline
  too; a handler that does not take its event after scheduling and cancelling the
  event's timer, one below it and one above; an entry that is not taken; three at
  the bound of 1,024, one of them the refused schedule, the cancel and the entry
  not taken that the first form got wrong; a stream that takes nothing; the
  cancels held at their bound; a service that reads history, with a cancel held
  and a refresh inside it; and a refresh.
  `crates/cavalry-proto/tests/cluster_conformance.rs` runs every one against
  `ServiceTimers`, which holds the table in itself and allocates nothing.
- `crates/cavalry-proto/tests/cluster_timers.rs` runs `ServiceTimers` against a
  second reading of the rule, a model over a map, under seeded calls: 8 seeds of
  4,000 handlings over nine ids and 8 of 600 with the table one short of its
  bound, with refreshes between handlings and inside them, the two compared after
  every handling and after the words the stream then takes, which is now and then
  all of them. Its first run found
  that the table told its member of earlier entries while a handler ran, until
  the handler's first change.
- A seeded search in memory, run before this ADR's first review, in the pattern of
  `crates/cavalry-raft/tests/lost_log_search.rs`. It is
  `crates/cavalry-raft/tests/timer_search.rs`, which lands in a change after this
  one with the member's table it needs, and it was run from the branch that holds
  both. Three members of `cavalry_raft::memory::Cluster` under loss and delay, with
  members halted, cut from each other and started again, and a leader cut off from
  both others; beside each a service as `spec/cluster.md` gives it, over
  `ServiceTimers`, and the member's timers; and beside one member, which the seed
  draws, a second process of the program that reads at its own pace, tells the
  same member, and may stop for good. A program schedules and
  cancels twelve ids from session messages, and three of them repeat. The seed
  also draws a service that reads late, a handler that does not take its entry, a
  stream that takes nothing for up to 4 s, in half of the seeds a service that
  stops and starts from its snapshot or from position 0, and members' wall clocks
  each up to 600 ms from the cluster's.
- What the search checks at every entry: one program state at one position beside
  every member, for a first and for a second reading; and every timer event handed
  over or passed over as a replay of the committed log says, by a map of ids to
  deadlines that knows nothing of `ServiceTimers`. At the end of a seed, after
  100 s with nothing cut, lost or late: no service holds a timer whose deadline the
  leader's clock passed in the first 50 s of that time, and the leader's member
  holds no such timer that it has appended no event for.
- 300 seeds of 60 s each, seeds 1 to 300, in a release build on an Apple M5 Pro
  laptop, four processes of 75 seeds, 352 s: no check failed. 202,365 timer events
  were appended, 10,532 of them by a leader that was cut off or was not the
  cluster's; 105,480 were handed over, each once, so about half of the events of
  these seeds were passed over by every service; services left 135,346 entries
  not taken, started 3,781 times and sent every pending timer again 141,446
  times, and members started 5,359 times. That half is what the search draws:
  services that read nothing for up to 4 s, a second process behind the first,
  and leaders cut off. It is no measure of a cluster at rest.
- One seed of the 300, seed 23, ends with two members whose elections have not
  closed after 100 s with nothing cut, lost or late, and a leader that commits
  nothing. The search makes none of its last checks for such a seed, counts it,
  and fails when more than one seed in twenty ends so. That it is the cluster's
  and no timer's is an argument and no run: a timer event passes the checks of
  any entry a leader appends for itself, and what the search adds to the cluster
  is a cut-off leader that goes on appending. It is noted on work item 420, with
  the seeds of earlier forms that left one member out, and 420 is where it is
  found out.
- What the search changed in the rule. A service that started beside a running
  member and read its history told its member each old schedule on the way, and a
  leader appended an event for each: a service now tells its member nothing while
  it reads history, and sends what is pending at the end of it. A service whose
  member's log was cut below where it ended when the service opened it never read
  to that position, and its leader was told nothing: the history ends at the
  first entry that is none, too. A member at the end of a seed held timers that
  were pending in no service, each marked: a service that stops before it has
  sent the cancel of a timer it handed over leaves one, which is why the member
  replaces a marked timer first when its table is full.
- The search fails when the rule is broken, each mutation run on seeds 1 to 60 and
  taken out again. No resend at each refresh interval: seed 13, where a timer is
  pending 100 s after it was due. An event handed over whatever its time: the
  first event that comes before its timer's deadline, which the second reading of
  the rule passes over. A member that does not mark the timer it appended an
  event for: a log that passes 100,000 timer events. Two parts of the rule have
  no failing run and are not there for one. With no resend at the end of history, the next
  refresh does it within a second. And a service that tells its member while it
  reads history costs entries that every service passes over, which the search
  does not count against a seed.
- The mutations are what took the second form out. With no answer to a
  passed-over event, with no resend when a member said it began again, and with
  no unmarking at an election's close, each in turn, seeds 1 to 60 of that form's
  search passed. That search started every stopped service again before its last
  check, and a service that starts sends everything: the search was changed to
  stop no service in half of its seeds and to leave a second process stopped.
- The vectors fail for what the search cannot see. A handling that is not taken
  and keeps what its handler did: `timer-event-not-taken`. A timer that its own
  handler scheduled again for the deadline it had, told to the member only when
  the deadline changed: `timer-repeats-with-one-deadline`.
- No measurement of latency: nothing calls the table at this change, so it adds no
  code that a message passes through yet. What the table costs a service's reading
  when the service's library calls it, in a change after this one: nothing for
  an entry that is no timer event and whose handler touches no timer, and nothing
  for an event that is passed over; for an event that is handed over to a handler
  that touches no timer, the move of the timers above its own, 17 bytes for each;
  for a handler that schedules or cancels, a copy of the pending timers at its
  first change, 17 bytes for each and 17,408 at the bound, a move of as much for
  a timer put in or taken out, and one pass over both copies when the handling
  ends; for a refresh one pass over the table, once a second; and for a read with
  nothing to tell, none of the table. The member's check for a due timer is one
  compare in a duty cycle while nothing has changed. The changes that add the
  callers measure both.

## Consequences

- A program is handed each timer once, never early and never after its cancel, at
  one position beside every member. The log may hold timer events that every
  service passes over: `cluster.service_timer_events_passed_over` counts them, and
  each costs 64 bytes of the log, a 32-byte frame header and the event.
- A program that schedules a timer for a deadline already past from each handling
  of that timer's event is handed the next event as soon as its leader is told
  and the event is committed and read, and the log grows by 64 bytes for each.
  Nothing bounds that but the program.
- Timers wait while no member leads, while the leader is of version 0 or 1, and
  while no service runs beside the leader, though a service runs beside every
  other member. Feature 4 (work item 364) decides when a leader steps down; a
  leader with no working service beside it is one of its cases, and until then
  `cavalry doctor` on the leader's host names it.
- An application's program schedules its first timer only once every member runs
  version 2, and no member is stepped back after. No member checks either. A
  service of version 1 that is given a snapshot holding a timer, as beside a member
  stepped back against that rule, passes the snapshot over and reads the log from
  position 0, and hands its program timer events as entries it does not know.
- The timestamps of the log's entries do not rise from entry to entry: a wall clock
  steps, and the next leader's may be behind. A timer is due by the clock of the
  member that leads, and an event's timestamp is never below the deadline its
  leader held.
- A timer may be late by a timer refresh interval, 1 s, where a member's table
  lost it: after a member starts again beside a service that ran on and then
  leads, after a member removed every timer, and after an event that a later
  leader's log does not hold, on the member that appended it.
- A service with timers pending sends its member one message a second for each,
  1,024 at most, on `cavalry:ipc`. The member's side sizes its reading of the
  consensus module stream for that, and says what it measured.
- A leader whose service is more than a second behind its log appends a second
  event for a due timer the service has not read, each second, and every service
  passes it over: `cluster.service_timer_events_passed_over` on that host rises
  with `cluster.timer_events`.
- A member holds timers that are pending in no service, marked, until their places
  are needed: `cluster.timers` may be above `cluster.service_timers`.
- This change is the page, the vectors, the codecs and the service's table. A
  member of this build writes version 2 and holds no timer yet, and no service has
  the calls: the member's table, the leader's events, the counters of the page's
  "Counters", the `cavalry doctor` finding for a leader with no service beside it
  and the service's library land in the changes after this one, with the search.
  In a cluster of this build alone no program can schedule a timer. Beside
  members of a later build, a member of this build that leads appends no timer
  event, so timers wait while it leads, as they do under a leader of version 1,
  though it writes version 2.
- The C ABI moves to version 7 with the two calls and the entry kind, in an ADR of
  its own, and the five clients follow.
- Revisit: when a service runs in a process that its member starts, for the member
  to hold the timers; when a program needs a timer before its first entry; when a
  major is needed, for whether a passed-over event is still appended; when a
  second lateness after a member's start is too long, for a sign from the member
  that it began again.

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
- Aeron's `Cluster.scheduleTimer` and `cancelTimer`, whose documentation is quoted
  in "Context", `ClusteredServiceAgent.scheduleTimer`,
  `ConsensusModuleAgent.onScheduleTimer` and `onReplayTimerEvent`, and the codecs
  `ScheduleTimer`, `CancelTimer`, `TimerEvent` and `Timer`.
