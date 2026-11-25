# Schedule a timer in a cluster's service

A how-to page: one task, have a cluster's service act at a time: expire a session,
close an auction, send what is due ([glossary](../glossary.md), "service", "timer"
and "timer event"). A service never reads a clock of its own, since every member's
service must reach the same state from the same log. It schedules a timer in the
cluster's time, and when the time has come the cluster's leader appends a timer
event, which every member's service is handed at one position. The page uses the
Python client; each of the five clients has these calls, in one shape, and its
README's "A service's timers" has them (`clients/python`, `node`, `go`, `jvm` and
`dotnet`), and in Rust they are `Replies::schedule_timer` and `cancel_timer`
(`crates/cavalry-client/README.md`). What a service and its member do is
[`spec/cluster.md`](../reference/cluster.md), "Timers"
([ADR 0151](../decisions/0151-give-a-clusters-service-timers-that-every-service-decides-from-the-log.md)).
In Aeron, these are a service's `scheduleTimer` and `cancelTimer`, and its
`onTimerEvent`.

## Prerequisites

- A cluster that runs, started as [Run a cluster of three members](run-a-cluster.md)
  starts one: three members on one host, with the directories `/tmp/cavalry-member-0`,
  `-1` and `-2`.
- Every member of it runs version 2 of the cluster protocol, with the service
  beside it. `cavalry doctor` says so in each member's `note cluster-member` line:
  `it runs version 2 of the cluster protocol`. In a cluster that is being upgraded
  from version 1, finish [the upgrade](upgrade-a-cluster.md) before a program
  schedules its first timer, and do not step a member back after it: a service of
  version 1 hands its program no timer, so it would compute another state from the
  same log.
- The shared library, which the Python package loads: `cargo build --release -p
  cavalry-ffi --lib`.
- Python 3.9 or later. The package is `clients/python`, used from the checkout.

## Steps

1. Write the service. Save this as `tick_service.py`. It keeps two deadlines that
   repeat, a tick each two seconds and a tock each three, and prints a line when
   one has come:

   ```python
   import heapq
   import sys
   import time

   import cavalry

   TIMER = 1                              # the one timer this program schedules
   EVERY = {"tick": 2000, "tock": 3000}   # what repeats, and after how many milliseconds
   due = []                               # the program's own deadlines, the earliest first


   def arm(service):
       # One timer, for the earliest deadline, whatever the count of deadlines.
       if due:
           service.schedule_timer(TIMER, due[0][0])
       else:
           service.cancel_timer(TIMER)


   def on_entry(body, entry, service):
       if entry.type == cavalry.CAVALRY_CLUSTER_ENTRY_NEW_LEADERSHIP_TERM and entry.position == 0:
           # The first entry of every log: the program's first deadlines, counted from it.
           for what, after in EVERY.items():
               heapq.heappush(due, (entry.timestamp + after, what))
       elif entry.type == cavalry.CAVALRY_CLUSTER_ENTRY_TIMER:
           # The cluster's time is entry.timestamp: every deadline that has come.
           while due and due[0][0] <= entry.timestamp:
               deadline, what = heapq.heappop(due)
               print(f"{what} at position {entry.position}, {entry.timestamp - deadline} ms after its deadline",
                     flush=True)
               heapq.heappush(due, (entry.timestamp + EVERY[what], what))
       else:
           return
       arm(service)


   with cavalry.Client(sys.argv[1]) as client:
       service = client.cluster_service()
       try:
           while True:
               if service.poll(on_entry, 10) < 0:
                   # The poll stopped. Code 102, CAVALRY_ERR_CLOSED, is the close of this
                   # service's client, as when the member's driver stops.
                   code, _, _ = service.fault()
                   print(f"ended at position {service.position()}: code {code}", flush=True)
                   break
               time.sleep(0.001)
       finally:
           service.close()   # a service is closed before its client
   ```

   Three things in it are the rules of a timer:

   - **It schedules and cancels from inside the handler, and nowhere else.** Its
     deadlines are counted from `entry.timestamp`, the cluster's time, and from no
     clock of the program's. So the timers are a function of the log, as the
     program's state is, and the same beside every member.
   - **It schedules its first timer at the first entry it is handed.** Every log
     begins with a new leadership term at position 0, which a service that reads
     the log from its start is handed first.
   - **It keeps its deadlines in its own state, and one timer for the earliest.** A
     service holds at most 1,024 pending timers, and a schedule beyond them answers
     `CAVALRY_CLUSTER_TIMER_REFUSED`. A program with one deadline for each session,
     order or lease has more than that: it keeps them as this one keeps `due`, and
     when it is handed its timer it acts on every deadline that has come and
     schedules the timer again for the earliest that is left.

2. Run it beside each member, in three terminals:

   ```sh
   PYTHONPATH=clients/python python3 tick_service.py /tmp/cavalry-member-0
   PYTHONPATH=clients/python python3 tick_service.py /tmp/cavalry-member-1
   PYTHONPATH=clients/python python3 tick_service.py /tmp/cavalry-member-2
   ```

   You will see the same lines in each of the three terminals, once the third
   program runs. On a cluster that was started a moment ago and has committed
   nothing else, they begin `tick at position 96, 1 ms after its deadline`, then
   `tock at position 160, 1 ms after its deadline` a second later, then `tick at
   position 224, 1 ms after its deadline`, and a line follows each second or two
   for as long as the programs run. The positions are higher on a cluster that has
   committed more, and the milliseconds differ from line to line. On a cluster that
   ran for a while before the programs started, the first tick and the first tock
   come at once, at one position, and are late by as long as the cluster ran: a
   deadline in the past is due at once.

   The leader appends a timer's event when the service beside it has told it of
   the timer, so nothing is printed until a program runs beside the member that
   leads. A service beside another member is handed the same events, and tells its
   own member of the same timers for the day that member leads.

3. Stop one of the programs with Ctrl-C and run it again, as in step 2. It prints
   the lines it printed before, from the first, with the same positions and the
   same milliseconds, and then goes on beside the two others: it read the log from
   its start, and its timers and its deadlines are what the log makes them.

## How to verify it worked

- The three terminals show the same lines: each deadline was handed to every
  member's service at one position of the log.
- No line says a number below 0: a timer is never handed over before its
  deadline. Its event is stamped with the leader's clock when the leader appended
  it, a millisecond or so after the deadline on an idle laptop.
- The program of step 3 printed its earlier lines again, unchanged.
- The counters of the member that leads say the same
  ([the counters reference](../reference/counters.md)): `cluster.timer_events`
  rises by one for each event it appended, and `cluster.service_timers` reads 1
  beside every member, the one timer this program holds for its two deadlines.
- `just conformance` runs this page's program as these steps do, in
  `conformance/python/howto_timers.py`, and runs the two calls beside three members
  in `conformance/python/cluster.py`.

## When it does not work

- **The programs print nothing.** No program runs beside the member that leads, or
  the cluster has no leader. Run the program beside every member. Then run `cavalry
  doctor --dir` with a member's directory and its two variables set: a member whose
  `cluster-member` line says `election closed` has a log, and one in an election
  has none until it has joined its leader's ([Run a cluster of three
  members](run-a-cluster.md)).
- **Two terminals print and one does not.** The program in the third runs beside a
  member that has not caught up, or beside a driver that is no member: doctor
  prints no `cluster-member` line for such a driver.
- **A program prints `ended at position 224: code 102` and ends**, with its own
  position. A poll failed, and the program ends on a poll that fails. Code 102 is
  `CAVALRY_ERR_CLOSED`: the service's client is closed, as it is when the member's
  driver stops. Start the member again, then the program. Any other code is in
  [the C ABI's error codes](../reference/c-abi.md#error-codes).
- **A schedule answers `CAVALRY_CLUSTER_TIMER_REFUSED`.** The service holds 1,024
  pending timers and the id is none of them. Keep the deadlines in the program's
  state and schedule one timer for the earliest, as this program does.
- **A schedule or a cancel answers −1.** It was called from outside the handler,
  where there is no entry whose time and position the timer could follow. Call it
  while the handler handles an entry.
- **`OverflowError` or `TypeError` from a schedule.** The id or the deadline is no
  int64: a deadline counted in a `float`, such as `entry.timestamp + 0.5 * 1000`,
  or an id above 2^63 − 1. Pass an `int` (`clients/python/README.md`, "A service's
  timers").
- **The lines print again at every start.** That is step 3: a service that takes no
  snapshot reads its member's log from position 0 at every start, its timers'
  events among the entries. [Take a snapshot of a service's
  state](take-a-services-snapshot.md): the service writes its pending timers into
  its snapshot by itself, and the program's own deadlines, `due` here, are part of
  the state the program gives.
- **A first line is tens of milliseconds late.** The poll in which a program first
  schedules a timer adds the service's counters and its publication to its member,
  six commands that wait for the driver, once in the service's life
  (`clients/python/README.md`, "The call"). A deadline a few milliseconds after the
  first entry it follows is met late by that much.
