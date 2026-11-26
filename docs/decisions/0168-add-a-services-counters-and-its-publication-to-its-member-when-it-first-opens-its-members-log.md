# 0168. Add a service's counters and its publication to its member when it first opens its member's log

- **Status:** Accepted
- **Date:** 2026-11-25
- **Invariants affected:** 2, 3, 4, 6
- **Supersedes:** ADR 0159 in two parts: the moment at which a service adds its
  five counters and its publication to its member, which ADR 0131 and ADR 0151
  had given `spec/cluster.md` in a sentence each, and the read at which it asks
  again after a refusal, which was its next read with something to tell. ADR
  0159's rule for the refusal itself stands: it is the error of the read, and it
  is counted in no counter of the service's. Nothing on the wire changes, no
  declaration of the C header changes, and no version moves.

## Context

A service added its five counters and its publication on the consensus module
stream at the first of three moments: its first snapshot, the first thing it had
to tell its member of a timer, and the first timer event it passed over. For a
program with timers that is the poll in which it first schedules one. That poll
has handed the program a session's message, and the entries behind that message
wait for it to return. Work item 462 came from what the clients' cluster rounds
print: a timer scheduled as due at once, at the first message at which a program
scheduled any, was stamped by the leader 38 to 68 ms after the message across the
five clients' rounds on one laptop, and the poll that scheduled it took 23 to 37
ms. The work item took that for the waits of six commands.

It is one command, the publication, and the size of its log. A `cavalry:ipc`
channel takes no parameters, so the log is three terms of 64 MiB and a page of 4
KiB, 192 MiB. Every page of a new log is touched three times before its first
message is read (ADR 0027): by the driver as it creates it, by the client that
asked as it maps it to write, and by the subscriber's client as it maps it to
read. The first two are inside the poll. The stream carries service acks and
timer messages of 36 bytes at the most. The Evidence has the times.

## Decision

A service adds its five counters, and then its publication on the consensus
module stream, in the read that first opens its member's log, after it has read
its member's counters there and before that read hands its program an entry,
whatever the program does. Telling the member of a timer adds nothing and sends
the driver no command, so a program's first timer costs what a later one costs.
A refusal by the driver is the error of the read that asked, returned once that
read's entries are taken, as ADR 0159 has it. The service asks again for what is
missing before the entries of its first read a timer refresh interval, 1 s, after
the refusal, and at no read between, each of which reports nothing of it. Until
the service holds the publication it tells its member nothing and keeps what it
has to tell.

## Alternatives considered

- **Leave the moment where it was and say on the page why.** It lost to
  invariant 2. The wait is on a path that hands over messages, once in each run of
  every program that has timers, at a moment the program chooses without knowing
  it: its first schedule.
- **Send the six commands without waiting for each answer.** The time is not the
  answers' round trips, about 2 ms of the whole, but the making of the log, by
  the driver and then by the service's own client when it maps it. Taking the
  answers in a later read would move the driver's part out of the poll and leave
  the client's part in one; and the client has no command that it does not wait
  for, so this would be its first.
- **Give this stream a small log.** It is the right size for the stream, and it
  would make the publication cheap wherever it is added. `cavalry:ipc` takes no
  term length today: that is a change to the channel grammar with its own
  decision, work item 478, placed directly after this one. It does not take the
  commands out of the poll, so it is no alternative to this decision but the
  next step after it.
- **Add them when the service is made, or at its start from a snapshot.** A
  service that is made has found no member yet, and may start before its member
  (ADR 0066). A start from a snapshot does read the member's key, and adds its own
  two counters there, but not every program makes one. The first open of the log
  is the first moment that every service has.
- **Add the publication early and the counters where they were.** The counters
  come before the publication so that a refusal at a snapshot is counted (ADR
  0131), and five counters are 2 ms. One moment for the six is one rule.
- **Ask again at every read while something is missing**, as a service did for a
  timer it could not tell. Every service asks from its start now, a program
  that polls a thousand times a second among them, and a refused command is not
  free for the driver: an error event to every one of its clients, two counters,
  and for a refused publication the making of its log on the conductor's thread,
  which serves every client of the host. Once a timer refresh interval is as
  often as a member is told of a timer again anyway.

## Evidence

All from one laptop, an Apple M5 Pro with macOS 26.6.1, in release builds.
Nothing was run on the bench hosts, other sessions' gates ran on the laptop
during some of it, and no run's number is a bound.

- **What the publication costs.** The independent read of this change measured
  it with a driver in the measuring process: the driver's creation of a 192 MiB
  log took 9.8 to 11.5 ms, the client's writable open of it 6.5 to 7.4 ms, and a
  read-only open 6.3 to 7.1 ms. `add_exclusive_publication("cavalry:ipc")` took
  17.6 to 41.0 ms in 16 calls, where a `cavalry:udp` publication of 64 KiB terms
  took 0.5 to 10.1 ms and an `add counter` 0.2 to 6.0 ms. My own first
  measurement was inside a service's first telling of a timer, in four test
  runs: the five counters took 1.5 to 2.6 ms together, and the publication 27,
  28, 168 and 363 ms. The read did not reproduce the two larger, and nothing
  here explains them.
- **A first timer, before and after.** Before, the event of a timer that a
  program scheduled as due at once at its first schedule was stamped 38 to 68 ms
  after its message, in the five clients' rounds as they were run for work item
  419. After, in ten runs of each client's runner: 16 to 25 ms in the Python
  round, 16 to 27 ms in the Node round, 15 to 22 ms in the Go round, 15 to 24 ms
  in the JVM round and 18 to 24 ms in the .NET round. That is 50 first timers
  and a maximum of 27 ms. A service has one first timer in its life, so there
  is no p99.99 of it to give.
- **A later timer.** `conformance/python/cluster.py` now sends 200 messages at
  the end of its round, one at a time, at each of which every program schedules
  one more timer, due at once. In one run with 10,000 of them the leader stamped
  the event, after the message's own time, nearest rank: p50 20 ms, p99 28 ms,
  p99.99 38 ms, max 38 ms. The histogram, milliseconds and counts: 9: 1, 11: 3,
  12: 3, 13: 6, 14: 49, 15: 209, 16: 511, 17: 857, 18: 1,164, 19: 1,456, 20:
  1,617, 21: 1,356, 22: 1,028, 23: 731, 24: 492, 25: 238, 26: 112, 27: 55, 28:
  32, 29: 24, 30: 8, 31: 11, 32: 10, 33: 8, 34: 5, 35: 4, 36: 3, 37: 5, 38: 2.
  The service beside the leader was handed each timer, after it scheduled it,
  p50 19,352 µs, p99 26,246 µs, p99.99 31,677 µs, max 38,186 µs, by its
  process's clock. In the ten runs of the gate's 200, 2,000 timers, the stamps
  were 11 to 42 ms. Both measures hold the round's own cycle, one thread that
  polls three services and a session and sleeps 200 µs between cycles.
- **So a first timer costs what a later one costs**, and what is left is one
  commit of an entry, its replay to a service and one telling of the member:
  work item 493.
- **The longest poll of a run.** In 49 of the 50 runs it was the poll in which a
  service first opened its member's log, 27.5 to 47.5 ms. In one run of the
  Python round it was a later poll that took one entry, 134.2 ms, which the
  round's report does not explain. Before, it was the poll in which a program
  first scheduled a timer, 22.7 to 37.8 ms. Each client's README has its own
  round's numbers.
- `a_service_adds_its_counters_and_its_publication_when_it_first_opens_its_members_log`
  in `crates/cavalry-client/tests/cluster_service.rs` opens the log before
  anything is committed: the five counters and the publication's image are there
  with no entry handed over. At every entry the program is handed after that,
  and after its member is told of its first timer, the driver's counters hold as
  many records as at the open, so no counter and no publication, whose position
  and limit are records, was added on the way. A service that reads history
  tells its member nothing meanwhile.
- `a_refused_add_is_the_polls_error_once_an_interval_and_is_made_good`, in the
  same file, fills the driver's counters so that the first of the five is
  refused. The poll that opens the log returns error code 10 with its entry
  taken, and the driver counts one refused command. No poll in the next 600 ms
  asks again or reports anything. The first poll a second after the refusal asks
  once more and is refused once more. With room again, the service adds what is
  missing and its member is told of the timer it kept. No test holds a refused
  publication with the counters taken, which runs the same lines.

## Consequences

- A program's first timer costs what a later one costs.
- The read that first opens the log is longer by the six commands, and it is the
  longest poll of a service's run where the poll of the first schedule was. A
  program whose other work cannot stop for tens of milliseconds at its start is
  told so in its client's README.
- Every service holds the five counters and the publication from its first open
  of the log, a program that schedules no timer and takes no snapshot included,
  which held none before. That is five more of the 256 counters a driver gives
  its clients, and one more log in the driver's directory, a default
  `cavalry:ipc` log of 3 × 64 MiB + 4 KiB, 192 MiB, every page of it resident.
  Beside a member, a service's replay of the member's log is a log of 48 MiB,
  three terms of the 16 MiB a member's log has. So a service with no timer and
  no snapshot goes from about 48 MiB of logs to about 240 MiB. Work item 478,
  placed directly after this decision, is to let the stream name a small one.
- On a host whose driver directory has no room for that log, `/dev/shm` on
  Linux, the driver refuses the publication with error code 12 and counts it in
  `driver.log_create_failed`, which `cavalry doctor` prints with what the log
  needs and what to free. The service's read that first opened the log returns
  that refusal as its error, once its entries are taken, and the service asks
  again once a second and returns it again while it lasts: the program reads its
  log, is told of the error once a second, and its member is told of no timer
  until the publication is there. A program that ends on a poll's error, as the
  how-tos' programs do, ends at its start. Before this decision that was the
  case from a program's first schedule or snapshot; now it is from a service's
  start, for every program.
- A log that the driver could not create may stay in its directory as a file,
  and each ask may leave another: work item 492, found by reading the driver
  for this decision and not run. The paced ask is one a second where it was one
  a poll.
- A driver that refuses a counter, its 256 in use, is the same error of the same
  reads, error code 10, counted in `driver.registry_full`.
- A service's gauge `cluster.service_timers` is in the counters file from its
  first open, for a program with no timer too, and while the service reads its
  history, where it was there only once a snapshot, a timer or a passed-over
  event had added it. So `cavalry doctor`'s finding for a leader that holds no
  timer beside a service that has some can show for as long as the leader's
  service reads a long history; its text asks for a second run and says why. And
  ADR 0167's remark that the gauge alone does not say that a service runs holds
  for a service of an earlier build only. Its list of the counters a service
  adds, and what doctor reads from it, stand.
- The publication a service's snapshots are recordings of is still added at its
  first snapshot, and is a log of 192 MiB. A snapshot is no message's path.
- A session's egress publication is still added in a poll that hands entries
  over, at the session's opening: a `cavalry:udp` log of 48 MiB, touched the same
  way. This decision is of the timers' publication alone.
- What is left of the time between a message and the event of a timer it
  scheduled as due at once is the same for a first timer and a later one: the
  entry's commit, its replay to the service, the telling and the leader's duty
  cycle, about 20 ms at the median on a laptop. Work item 493 asks where that
  goes.
- Revisit when work item 478 is decided: with a small log for this stream the
  cost of the first open falls, and nothing of this decision changes.

## References

- Work item 462, which this decision was made in; work item 419, whose rounds
  showed it; work item 478, the term length of a `cavalry:ipc` channel; work item
  492, a log left as a file where the driver could not create it; work item 493,
  the time an entry takes to reach its service.
- `spec/cluster.md`, "Members and streams", "Snapshots", "A service's snapshot",
  and "The service", "Timers", "What a service tells its member"; `spec/c-abi.md`,
  "Handles".
- [ADR 0027](0027-pre-touch-shared-mappings-at-map-time.md),
  [ADR 0066](0066-feed-the-committed-log-to-a-service-through-a-replay-of-the-members-recording.md),
  [ADR 0131](0131-have-a-service-write-its-own-snapshot-and-its-member-list-it.md),
  [ADR 0151](0151-give-a-clusters-service-timers-that-every-service-decides-from-the-log.md),
  [ADR 0159](0159-report-to-the-program-a-publication-the-driver-refuses-while-a-service-tells-its-member-of-a-timer.md),
  [ADR 0167](0167-have-doctor-name-a-leader-beside-which-no-service-runs.md).
- `conformance/README.md`, on the cluster round, and each client's README, "The
  call", for what the rounds print.
