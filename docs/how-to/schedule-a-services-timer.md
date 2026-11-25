# Schedule a timer in a cluster's service

A how-to page: one task, have a cluster's service do something at a set time: expire
a session, close an auction, send what is due ([glossary](../glossary.md), "service",
"cluster time", "timer" and "timer event"). A service never reads a clock of its
own, since every member's service must reach the same state from the same log. It
schedules a timer in the cluster's time. When that time has come, the cluster's
leader appends a timer event to the log, and every member's service is handed it at
one position.

The page uses the Python client. Each of the five clients has these calls, in one
shape: see "A service's timers" in the README of `clients/python`, `node`, `go`,
`jvm` or `dotnet`. In Rust they are `Replies::schedule_timer` and `cancel_timer`
(`crates/cavalry-client/README.md`). What a service and its member do is
[`spec/cluster.md`](../reference/cluster.md), "Timers"
([ADR 0151](../decisions/0151-give-a-clusters-service-timers-that-every-service-decides-from-the-log.md)).
In Aeron, these are a service's `scheduleTimer` and `cancelTimer`, and its
`onTimerEvent`.

## Prerequisites

- A cluster that runs, started as [Run a cluster of three members](run-a-cluster.md)
  starts one: three members on one host, with the directories `/tmp/cavalry-member-0`,
  `-1` and `-2`.
- Every member of it runs version 2 of the cluster protocol or a later one, with the
  service beside it. `cavalry doctor` says so in each member's `note cluster-member`
  line: `it runs version 2 of the cluster protocol`, or version 3. In a cluster that is being upgraded
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
       # One timer, for the earliest deadline, whatever the count of deadlines.
       service.schedule_timer(TIMER, due[0][0])


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

   Four things in it are the rules of a timer:

   - **It schedules from inside the handler, and nowhere else.** Its deadlines are
     counted from `entry.timestamp`, the cluster's time, and from no clock of the
     program's. So the timers are a function of the log, as the program's state
     is, and the same beside every member. `service.cancel_timer(TIMER)` cancels
     a timer, from inside the handler too; this program always has a deadline, so
     it never cancels.
   - **It schedules its first timer at the first entry it is handed.** Every log
     begins with a new leadership term at position 0, which a service that reads
     the log from its start is handed first.
   - **It keeps its deadlines in its own state, and one timer for the earliest.** A
     service holds at most 1,024 pending timers, and a schedule beyond them answers
     `CAVALRY_CLUSTER_TIMER_REFUSED`. A program with one deadline for each session,
     order or lease has more than that. It keeps them as this one keeps `due`.
     When it is handed its timer, it acts on every deadline that has come and
     schedules the timer again for the earliest that is left.
   - **Its handler takes every entry.** A handler that returns `cavalry.ABORT`, or
     raises, is handed its entry again, and the service puts its timers back as
     they were before that handling. The program's own state, `due` here, is the
     program's to put back. A handler that may abort, as one does when a reply
     answers `CAVALRY_CLUSTER_REPLY_RETRY`, decides so before it changes its state.

2. Run it beside each member, in three terminals:

   ```sh
   PYTHONPATH=clients/python python3 tick_service.py /tmp/cavalry-member-0
   PYTHONPATH=clients/python python3 tick_service.py /tmp/cavalry-member-1
   PYTHONPATH=clients/python python3 tick_service.py /tmp/cavalry-member-2
   ```

   You will see two lines at once, as soon as a program runs beside the member
   that leads, such as `tick at position 192, 141873 ms after its deadline` and
   `tock at position 192, 140873 ms after its deadline`. The program counted its
   first deadlines from the log's first entry, two and three seconds after the
   cluster elected its first leader. A cluster that has run for longer than that
   is past both, and a deadline in the past is due at once: the two are handed
   over with one timer event, late by as long as the cluster ran, less the two
   seconds and the three. The position is
   where the log ended then: 96 on a cluster that has elected one leader and
   committed nothing else, and 96 more for each leader it has elected since, so
   192 here.

   From then on a line follows each second or two: `tick at position 256, 1 ms
   after its deadline` two seconds after the first two, `tock at position 320, 0
   ms after its deadline` a second later, and so on. Each line's position is 64
   above the last, the size of a timer event in the log, with two exceptions.
   Each six seconds a tick and a tock fall due within a millisecond or two of each
   other: they are handed over with one event and print at one position, or the
   later of the two follows with an event of its own, a few milliseconds late. And
   now and then a position is 128 above the last: a leader that is told of a timer
   again, after it appended that timer's event and before its own service was
   handed it, appends a second event, and every service passes that one over.

   Every program prints the same lines, and one that starts later prints the
   earlier ones at once: it reads them from the log. The leader appends a timer's
   event only when the service beside it has told it of the timer, so nothing is
   printed until a program runs beside the member that leads. A service beside
   another member is handed the same events. It tells its own member of the same
   timers, so that the member has them if it becomes the leader.

3. Stop the program beside one member with Ctrl-C, which prints a
   `KeyboardInterrupt` traceback, and run it again as in step 2. It prints the
   lines it printed before, from the first, with the same positions and the same
   milliseconds: it read the log from its start, and its timers and its deadlines
   are what the log makes them. Then it goes on with the two others.

   If the program you stopped ran beside the member that leads, the two others
   print the lines of one more event at the most and then wait: no service tells
   the leader of the timer after that one. They go on when the program runs again.
   A deadline that passed while the program was stopped is handed over late by as
   long as it waited, and one that had not passed is on time.

4. Stop the three programs with Ctrl-C when you have seen enough. The log keeps a
   timer event for each position they printed, and one for each event they passed
   over.

## How to verify it worked

- The three terminals show the same lines: each deadline was handed to every
  member's service at one position of the log.
- The first tick is late by 1,000 ms more than the first tock, to the millisecond:
  both were counted from one entry and handed over with one event.
- The program of step 3 printed its earlier lines again, unchanged.
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
- **The lines stop in every terminal.** The program beside the member that leads
  has stopped, or the cluster is electing a leader. Timers wait while no member
  leads and while no service runs beside the leader: step 3. Run `cavalry doctor`
  for each member. Where one of them says `the leader in term` and `election
  closed`, the cluster has its leader, and the program beside that member is the
  one that stopped: doctor has no line for a leader with no service beside it. A
  member that prints `WARN cluster-no-leader` is in an election.
- **A program prints `ended at position 256: code 102` and ends**, with its own
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
- **A traceback that ends in `cavalry.HandlerError`, with `OverflowError` or
  `TypeError` above it as its cause.** The id or the deadline is no int64: a
  deadline counted in a `float`, such as `entry.timestamp + 0.5 * 1000`, or an id
  above 2^63 − 1. Pass an `int` (`clients/python/README.md`, "A service's timers").
- **The lines print again at every start.** That is step 3: a service that takes no
  snapshot reads its member's log from position 0 at every start, its timers'
  events among the entries. [Take a snapshot of a service's
  state](take-a-services-snapshot.md): the service writes its pending timers into
  its snapshot by itself, and the program's own deadlines, `due` here, are part of
  the state the program gives.
- **A position is 128 above the last, and not 64.** The log holds a timer event
  that every service passed over. The leader appended it when it was told of a
  timer again, after it appended that timer's first event and before its own
  service was handed it. Nothing is wrong, and no program was handed anything for
  it. `cluster.service_timer_events_passed_over` counts them beside each member.
  `doctor` does not print that counter: read it from the `counters` file in the
  driver's directory ([the counters file](../reference/counters-file.md);
  [counters](../reference/counters.md) says what each counter means).
