# 0109. Measure every interval on a nano clock, and keep the epoch clock for what another process reads

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 2, 3, 4, 6
- **Supersedes:** none. Amends `spec/client-protocol.md`, "Liveness and timeouts",
  with no new version: which clock measures each timeout, when the heartbeat is
  stored, and what a step of the host's clock does. Amends the sentence of
  `spec/udp.md` that names the same clock, the wording of the consumer heartbeat in
  `spec/command-ring.md`, and the reclaim deadline's row in `spec/counters-file.md`,
  a field the driver alone writes and reads. Adds to `spec/durable-log.md`, "Time
  index", what the recorder already does when the clock is set back. No byte of a
  file or a frame moves, and no value another process reads changes its meaning.

## Context

Every thread of the driver read one clock, once per duty cycle: `now_nanos()` in
`crates/cavalry-driver/src/conductor.rs`, the host's wall clock in nanoseconds since
the Unix epoch. The conductor, the sender, the receiver, the recorder and the
replayer each passed that reading down, and every timeout was the difference of two
such readings. `spec/client-protocol.md` said so: "Each timeout is measured on the
clock the driver stores in the consumer heartbeat, nanoseconds since the Unix
epoch." A client measured its keepalive interval the same way (`keepalive` in
`crates/cavalry-client/src/conductor.rs`), and `cavalry-log`'s recording writer its
sync interval (`sync` in `crates/cavalry-log/src/recorder.rs`). No decision recorded
the choice, and no page said what a step of that clock does (card 266).

A wall clock steps. A time service sets it, a person sets it, and a host or a
virtual machine that was suspended finds it ahead by the time it slept. Read from
the code, a step forward of F ended at once every interval with less than F to run:
with F of 10 s or more, the conductor timed out every client whose next keepalive
it had not read yet (`time_out_clients`), the receiver withdrew every image over
UDP it had not heard from since the step, and every publication's linger ended;
with F of 1 s, a freed counter's record was allocated again while a reader might
still hold its id (`CountersFile::allocate` compared the reclaim deadline with a
wall clock reading of its own). A step back of B lengthened every interval by B,
and three of them failed in ways the card did not list. The recorder made no
periodic sync for B, so a crash could lose B more of a recording than the
recording sync interval promises. A client that was idle sent no keepalive for B.
A freed counter's record stayed reclaimed for B longer, and a counters file that
filled in that time refused allocations.

Only two kinds of reading are compared with a reading another process made, or a
later run. The consumer heartbeat: a client, `cavalry doctor` and a starting driver
each compare it with their own clock. The timestamps on disk: a recording's start
and stop and its time index, which `cavalry replay` looks up by instant, and the
three files' start timestamps. Every other use is an interval one process measures
by itself. A client's liveness is one of them: a command carries no time, and the
driver stamps a client with its own reading when it reads the client's command.

Aeron reads two clocks for this reason, and the cluster protocol already measures
its timeouts on a clock that does not step (ADR 0058).

## Decision

A process that speaks the client protocol reads two clocks, named as Aeron names
them. Its **epoch clock** is the host's wall clock, nanoseconds since the Unix
epoch. Its **nano clock** counts nanoseconds from the process's first reading of
it, never goes back, and is not moved when the host's clock is set; a reading of
it means nothing to another process. Every interval in the tables of
`spec/client-protocol.md` and `spec/udp.md` is measured between two readings of
the measuring process's nano clock: the heartbeat interval, the keepalive
interval, the client liveness timeout, the response timeout, which a client
measures so already, both unblock timeouts, the publication linger, the counter
reuse timeout, the recording sync interval, the replay connect timeout, the
closing endpoint wait, and every interval of the sender and the receiver. The
epoch clock is kept for the values another process or a later run reads: the
consumer heartbeat, the files' start timestamps, and every timestamp of
`spec/durable-log.md`, the time index's cadence with them. A freed counter's
reclaim deadline becomes a reading of the driver's nano clock; the driver writes
it and the driver reads it, in a file it makes anew each time it starts.

Two comparisons have another process's clock in them, and keep the epoch clock for
that reason.

- **The driver timeout** is between two processes: a client, `cavalry doctor` and
  a starting driver that finds no `pid` each measure it as their own epoch clock
  less the consumer heartbeat. So the driver keeps the heartbeat right. It stores
  its epoch clock there once per heartbeat interval of its nano clock, and at once
  when it finds its epoch clock a heartbeat interval or more from the value it last
  stored, forward or back. It reads its epoch clock for this on every duty cycle.
- **The client liveness timeout** is measured on both of the driver's clocks. The
  driver notes both when it reads a client's command, and times the client out
  when its last command is older than the timeout on the nano clock and on the
  epoch clock. The nano clock is in it so that a clock set forward times out
  nobody. The epoch clock is in it for a client of an earlier release, which
  measured its keepalive interval on its epoch clock and so sends nothing for the
  length of a step back.

So a step of the host's clock moves no interval. What it still does is in
`spec/client-protocol.md`. Until the driver's next duty cycle, which stores the
heartbeat again, a step forward of F makes the heartbeat read F older than it is: a
client that loads it then, with F above the driver timeout less the heartbeat's
age, reports `driver-timeout` and closes. A step back of B makes it read B newer
until the driver stores the stepped clock, and a client that stopped sending is
timed out B later, as it always was. The store at once has a cost of its own after
a step back of more than the driver timeout: a reader that took its epoch clock
before the step and loads the heartbeat after that store reports `driver-timeout`.
The window is the time between the reader's two reads, and it is left open here:
closing it means confirming a timeout against a second reading of the clock in
each reader, which is card 273's. On disk, the time index already writes a
clock that went back as the last entry's timestamp (`spec/durable-log.md`, "Time
index"), and a recording's stop timestamp is its last entry's.

The driver's and a client's nano clock is read through `cavalry-shm`
(`clock::nano_time`), one for all the threads of a process, so that the
conductor's reading and the counters file's are of one clock. Each data-plane
thread still reads one clock per duty cycle, now the nano clock. A thread that
stamps adds the difference between its two clocks to the duty cycle's reading. The
recorder reads that difference once per millisecond. The conductor, which no
message passes through, reads it on every duty cycle: a host that wakes from a
suspension has clients reading the stepped clock at once, and a difference a
millisecond old would leave them the stale heartbeat for that long.

## Alternatives considered

- **Keep the wall clock for every timeout, specify what a step does, and have
  `cavalry doctor` see a step** (the card's first option). It writes the fault
  down and keeps it: a step forward of 10 s closes every client of a healthy
  driver, and a step back widens what a crash can lose of a recording, which is
  invariant 1's ground. A check can report a step only after the clients are gone.
- **Move the consumer heartbeat to a clock that does not step as well**, so that
  the driver timeout is immune too (the card's second option, as worded). The
  heartbeat is the one value every client of every version reads. A client of an
  earlier release would read the new value as an epoch time decades old and report
  `driver-timeout`, so it is a new protocol version with no path for mixed
  versions. It also needs a clock whose readings two processes can compare: the
  standard library's `Instant` is opaque, the operating system's monotonic clock is
  an `unsafe` call in every process that reads the heartbeat, and a directory on a
  disk can hold a heartbeat from before a reboot that is ahead of the clock after
  it. The step it would mend is mended more cheaply on the client's side, below.
- **Judge the driver by a heartbeat that stopped changing**, measured on the
  client's nano clock, as a client's `connect` already does while it waits for a
  driver that is creating its files (ADR 0072), and keep the comparison of ages for
  the first read alone. It makes the driver timeout immune to a step and to a
  suspended host. Not in this decision: `spec/command-ring.md` has every claim
  check the heartbeat's age, with no state, and the rule would change that
  algorithm, its vectors and the five clients' rounds. It is what "revisit" below
  names.
- **The heartbeat interval on the nano clock alone.** The driver would then wait
  out what is left of a second before it stored its heartbeat after a step, where
  it stored on its next duty cycle when the interval was measured on the wall
  clock: a host that woke from a suspension of more than 10 s would leave a stale
  heartbeat for up to a second, and nearly every client would load it. The store
  at once keeps the window where it was.
- **The client liveness timeout on the nano clock alone**, as every other interval.
  Under a driver of this decision, a client of an earlier release would be timed
  out by a step back of more than 9.5 s, the timeout less its keepalive interval,
  which the wall clock on both sides never did. Mixed versions run during an
  upgrade (invariant 3), so the driver keeps the epoch clock in this one timeout.
  The cost is the old behaviour in one case: a client that stopped is found the
  length of a step back later.
- **Aeron's own split, which measures a client's liveness on the epoch clock
  alone.** In Aeron the time of a client's last keepalive is a counter in the
  counters file, which the client also writes with its own epoch clock. In Cavalry
  only the driver stamps a client, so the epoch clock alone would keep the fault of
  a step forward for nothing.
- **A nano clock that starts at the epoch clock's reading**, so that its values
  look like wall times and code that mixes the two keeps working. That is why it
  lost: a nano reading stored where an epoch time belongs would be right until the
  first step, and wrong by the step after it, with no test to see it. With an
  origin at the process's start such a value is decades stale, and every heartbeat
  check refuses it at once.
- **Count each step the driver sees, in this change.** The driver sees a step now,
  and invariant 4 wants it counted. A static counter with an id of 128 or more does
  not fit the counters files of 128 records that 36 tests and the bench create, and
  every id below 128 is taken. The count is an instance counter with a `doctor`
  finding that reads it, a change of its own: card 273.

## Evidence

- The readings, at `master`'s 1aa851b: `now_nanos()` was called once per duty cycle
  in `conductor.rs`, `sender.rs`, `receiver.rs`, `recorder.rs` and `replayer.rs` of
  `crates/cavalry-driver/src`, and was `SystemTime::now()` since the epoch. The
  client's `now_nanos()` and `cavalry-shm`'s two were the same. The comparisons
  with another process's reading are `check_driver` in
  `crates/cavalry-shm/src/command_ring/cells.rs`, `liveness` in the client's
  conductor, `Attached::open` in `crates/cavalry-client/src/client.rs`, the
  heartbeat's age in `crates/cavalry-control/src/doctor.rs`, and the starter's
  check in the driver's conductor.
- The sizes of the effects in Context are the table's values: client liveness 10 s
  with a keepalive every 500 ms; publication linger 5 s; counter reuse 1 s;
  recording sync 1 s; heartbeat interval 1 s.
- Aeron 1.54.2 (commit 481d427). `DriverConductor.trackTime` takes the nano
  clock's reading every duty cycle and refreshes a cached epoch clock once per
  millisecond, which is the recorder's cadence here. `heartbeatAndCheckTimers` stores
  the cached epoch clock in the consumer heartbeat. Linger, unblocking and image
  liveness are differences of nano readings (`NetworkPublication.onTimeEvent`,
  `IpcPublication`, `PublicationImage`), and so is the command ring's unblock
  (`checkForBlockedToDriverCommands`). A client's liveness is
  `timeMs > heartbeatTimestamp.get() + clientLivenessTimeoutMs` in
  `AeronClient.onTimeEvent`, on the epoch clock, against a counter that
  `ClientConductor.checkLiveness` also writes. That method measures the keepalive
  interval on the nano clock and the driver timeout as
  `nowMs > lastKeepAliveMs + driverTimeoutMs` on the epoch clock, which is the
  comparison this decision keeps. The archive stamps a recording's stop with
  `epochClock.time()`.
- The standard library's `Instant` reads `CLOCK_MONOTONIC` on Linux and
  `CLOCK_UPTIME_RAW` on macOS (`std/src/sys/time/unix.rs`, Rust 1.99). Neither is
  set by a step, and neither counts the time a host is suspended.
- The tests in the `clocks` module at the end of
  `crates/cavalry-driver/src/conductor.rs` drive a conductor on a nano clock they
  advance and step the epoch clock it reads. After a step forward of an hour a
  linger still runs five seconds and a client is timed out after ten. The
  heartbeat is stored on the next duty cycle after a step either way. A client
  that is silent for a step back of 15 s is not timed out. Each test fails when its
  rule is put back: the liveness on the epoch clock alone, on the nano clock
  alone, the heartbeat by its interval alone, the linger on the epoch clock.
- No host's clock was stepped for this decision: `kip` runs card 159's soak, and
  the bench hosts were in use. The bench figures of the change are in its commit
  message.

## Consequences

- A time service that steps the clock, a clock set by hand and a paused virtual
  machine no longer time out a client, withdraw an image, end a linger early, hold
  a recording's sync or free a counter's record early. On Linux and macOS a
  publication's linger with 2 s to run has 2 s to run after a suspended host wakes.
- Two clocks are kept apart by name, not by type: both are an `i64` of
  nanoseconds. The nano clock's origin makes a reading in the wrong place fail the
  first heartbeat check, and the `clocks` tests fail for an interval measured on
  the epoch clock.
- The driver timeout still follows the epoch clock. A step forward of more than
  10 s, or a host suspended for longer, closes the clients that load the heartbeat
  before the driver's next duty cycle stores the next one, as it did before
  and as it does in Aeron. Nothing counts a step yet, so `cavalry doctor` cannot
  explain those closes: card 273. **Revisit** when a deployment reports
  `driver-timeout` after a step or a suspended host: the remedy is the third
  alternative.
- The epoch clock stays in the client liveness timeout for as long as a client
  released before this decision may attach. **Revisit** at the release that no
  longer supports one: the timeout then goes on the nano clock alone.
- `cavalry-raft` keeps a nano clock of its own for the cluster's timeouts (ADR
  0058), with its own origin. A process that hosts a consensus module and a client
  has both; no reading of one is compared with the other.
- `crates/cavalry-ffi/tests/committed_log.rs` and
  `crates/cavalry-raft/tests/cluster.rs` assert a linger of at least 5 s against
  `Instant`. They hold as written: the linger is measured on the clock `Instant`
  reads.

## References

- Card 266; card 273 for the count of steps. `spec/client-protocol.md`, "Liveness
  and timeouts"; `spec/udp.md`, "Values"; `spec/command-ring.md`, "Heartbeat and
  correlation ids"; `spec/counters-file.md`, "Records"; `spec/durable-log.md`,
  "Time index".
- [ADR 0058](0058-specify-the-cluster-protocol-from-aerons-codecs.md): the
  cluster's timeouts on a monotonic clock.
- [ADR 0072](0072-wait-in-a-clients-connect-for-a-driver-that-is-creating-its-files.md):
  a heartbeat that was not stored again, measured on a clock that never goes back.
- [ADR 0079](0079-hold-the-driver-directory-by-a-lock-for-the-drivers-life.md): a
  starting driver goes by the lock, and by the heartbeat only where no `pid` is.
- Aeron: <https://github.com/aeron-io/aeron/tree/1.54.2>, `aeron-driver`,
  `aeron-client` and `aeron-archive`, the classes named under Evidence.
