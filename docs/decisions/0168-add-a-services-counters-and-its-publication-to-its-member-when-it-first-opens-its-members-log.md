# 0168. Add a service's counters and its publication to its member when it first opens its member's log

- **Status:** Accepted
- **Date:** 2026-11-25
- **Invariants affected:** 2, 3, 4, 6
- **Supersedes:** ADR 0159 in one part, and with it one sentence each of ADR 0131
  and ADR 0151: the moment at which a service adds its five counters and its
  publication to its member. ADR 0159's rule for a refusal stands: it is the error
  of the read, and it is counted in no counter of the service's. Nothing on the
  wire changes, no declaration of the C header changes, and no version moves.

## Context

A service added its five counters and its publication on the consensus module
stream at the first of three moments: its first snapshot, the first thing it had
to tell its member of a timer, and the first timer event it passed over (ADR
0131, ADR 0151, ADR 0159). For a program with timers that is the poll in which it
first schedules one. That poll has handed the program a session's message, and
the entries behind that message wait for it to return.

Work item 462 came from what the clients' cluster rounds print. A timer that a
program scheduled as due at once, at the first message at which it scheduled any,
was stamped by the leader 49 ms after the message in the Python and the Node
round, and 38 to 68 ms after it across the five clients' rounds, on one laptop.
The poll that scheduled it took 23 to 37 ms. The work item took that for the
waits of six commands.

It is one command. Measured on that laptop, an Apple M5 Pro, in release builds:

- `add counter` waited about 90 microseconds when the driver's conductor was
  busy, and 1 to 3 ms when it had idled, in 48 calls. The five counters of a
  service's first telling took 1.5 to 2.6 ms together, in four runs.
- The publication took 27, 28, 168 and 363 ms in those four runs. On a driver
  that did nothing else, four additions of one took 137 to 191 ms, and five
  counters with one publication, back to back, 89 to 270 ms in five runs.

A `cavalry:ipc` channel takes no parameters, so every IPC publication's log is
three terms of 64 MiB, 192 MiB, and the driver makes every page of a new log
resident as it creates it (ADR 0027). The service's publication to its member
carries service acks and timer messages of 36 bytes at the most.

## Decision

A service adds its five counters, and then its publication on the consensus
module stream, in the read that first opens its member's log, after it has read
its member's counters there and before that read hands its program an entry,
whatever the program does. A read after it adds what is missing, before its
entries, for as long as something is: a refusal by the driver is the error of
that read, returned once the read's entries are taken, as ADR 0159 has it. Until
the service holds the publication it tells its member nothing and keeps what it
has to tell. Telling the member of a timer adds nothing and sends the driver no
command, so a program's first timer costs what a later one costs.

## Alternatives considered

- **Leave the moment where it was and say on the page why.** It lost to
  invariant 2. The wait is on a path that hands over messages, once in each run of
  every program that has timers, at a moment the program chooses without knowing
  it: its first schedule.
- **Send the six commands without waiting for each answer.** The time is not the
  answers' round trips, about 2 ms of the whole, but the driver's making of the
  log. Taking the answers in a later read would return the poll sooner, and the
  member would be told as late as it is now; the client has no command that it
  does not wait for, and this would be its first.
- **Give this stream a small log.** It is the right size for the stream, and it
  would make the publication cheap wherever it is added. `cavalry:ipc` takes no
  term length today: that is a change to the channel grammar with its own
  decision, work item 478. It does not take the commands out of the poll, so it
  is no alternative to this decision but the next step after it.
- **Add them when the service is made, or at its start from a snapshot.** The
  counters are keyed by the member's id, which a service reads at its first open
  of the log, and a service may start before its member (ADR 0066). The first
  open is the first moment at which it can.
- **Add the publication early and the counters where they were.** The counters
  come before the publication so that a refusal at a snapshot is counted (ADR
  0131), and five counters are 2 ms. One moment for the six is one rule.

## Evidence

All from one laptop, an Apple M5 Pro with macOS; nothing was run on the bench
hosts, and no run's number is a bound.

@@EVIDENCE@@

- `a_service_adds_its_counters_and_its_publication_when_it_first_opens_its_members_log`
  in `crates/cavalry-client/tests/cluster_service.rs` holds the rule from inside
  the handler: at every entry a program is handed, the first included, the five
  counters are in the driver's counters file, and the member's subscription has
  the publication's image; the driver's counters hold as many records after the
  member is told of the program's first timer as at the first entry, so neither
  a counter nor a publication, whose position and limit are records, was added on
  the way; and a service that reads history tells its member nothing meanwhile.

## Consequences

- A program's first timer costs what a later one costs.
- The read that first opens the log is longer by the six commands, and it is the
  longest poll of a service's run where the poll of the first schedule was.
- Every service holds the five counters and the publication from its first open
  of the log, a program that schedules no timer and takes no snapshot included,
  which held none before. That is five more of the 256 counters a driver gives
  its clients, and one more default `cavalry:ipc` log in the driver's directory,
  3 × 64 MiB + 4 KiB, 192 MiB, every page of it resident. Each service's replay
  of its member's log is a log of that size already, and so is the member's own
  replay of it. Work item 478 is to let the stream name a small one.
- On a host whose driver directory has no room for that log, `/dev/shm` on
  Linux, the driver refuses the publication with error code 12 and counts it in
  `driver.log_create_failed`, which `cavalry doctor` prints with what the log
  needs and what to free. The service's read that first opened the log returns
  that refusal as its error, once its entries are taken, and each read after it
  asks again and returns it again while it lasts (ADR 0159): the program reads
  its log, is told of the error at every read, and its member is told of no
  timer until the publication is there. Before this decision that was the case
  from a program's first schedule or snapshot; now it is from a service's start,
  for every program.
- A driver that refuses a counter, its 256 in use, is the same error of the same
  reads, error code 10, counted in `driver.registry_full`.
- A service's gauge `cluster.service_timers` is in the counters file while the
  service reads its history, which it was not unless a snapshot had added it. So
  `cavalry doctor`'s finding for a leader that holds no timer beside a service
  that has some can show for as long as the leader's service reads a long
  history. Its text asks for a second run and says why.
- The publication a service's snapshots are recordings of is still added at its
  first snapshot, and is a log of the same size. A snapshot is no message's path.
- What is left of the time between a message and the event of a timer it
  scheduled as due at once is the same for every entry: the entry's commit, its
  replay to the service, the telling and the leader's duty cycle. Work item
  @@COMMIT@@ asks where that goes.
- ADR 0167's list of the counters a service adds stands, and so does what doctor
  reads from it: a service adds the first of them at the same moment as before.
- Revisit when work item 478 is decided: with a small log for this stream the
  cost of the first open falls, and nothing of this decision changes.

## References

- Work item 462, which this decision was made in; work item 419, whose rounds
  showed it; work item 478, the term length of a `cavalry:ipc` channel.
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
