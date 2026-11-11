# 0072. Wait in a client's connect for a driver that is creating its files

- **Status:** Accepted
- **Date:** 2026-11-10
- **Invariants affected:** 3, 5, 6
- **Supersedes:** none. Amends `spec/client-protocol.md`, "The driver directory",
  within version 7, with a rule for a case the page answered with a refusal: a client
  that attaches while the driver is still creating its files. Amends `spec/c-abi.md`,
  "Threads", within version 1, with the sentence that `cavalry_init` can take that
  long. Nothing on the wire or in the header changes.

## Context

A driver that starts creates three files in its directory, `counters`, then
`command-ring`, then `broadcast-buffer` (`spec/client-protocol.md`, "The driver
directory"). Each is created, sized, mapped, brought into memory (ADR 0027) and given
its header, the magic last. A reader that opens one of them on the way finds one of
three things: no file; a file of no bytes, created and not yet sized, which every page
calls `too-short`; or a file whose magic is eight zero bytes, which every page calls
`not-ready` and answers with "Wait and read again."

A connecting client did not wait. It looked for the command ring, opened it, then the
broadcast buffer, then the counters file, and refused at the first that failed a
check. `cavalry doctor` reads the command ring and the counters file and not the
broadcast buffer, so it says a driver "is running" while the broadcast buffer is
still being made. Card 243 found the result on 2026-11-10: a test of `cavalry` that
starts `cavalry run`, waits for `cavalry doctor` to pass and runs `cavalry record`
failed once in 60 runs with `client refused (broadcast): broadcast buffer rejected
(not-ready): the magic is zero, the creator has not finished; wait and read again`.
The message tells the operator to do what the program could have done. The README's
ten-minute path has the same moment between `cavalry run` and its first client.

A starting driver already waits in the same case: it gives a command ring that is
`not-ready` the driver timeout, 10 s, and then treats it as a dead driver's.

## Decision

A connecting client waits for a driver that is creating its files. When the command
ring is there and one of the three files is missing, empty or `not-ready`, the client
reads the directory again from the command ring, every 10 ms, for up to the driver
timeout, 10 s, from its first read, measured on a clock that never goes back. At the
bound the driver is a dead one, and the client refuses with `driver-gone` and the
reason `driver-timeout`, in two cases: its command ring is still unfinished, as it is
to a starting driver; or its ring is finished and its heartbeat has not been stored
again since the client first read it, which is what a driver that stopped after it
made its ring leaves, since a driver that runs stores its heartbeat every second.
Another file still unfinished at the bound beside a heartbeat the driver has stored
meanwhile is refused with its own check, as before: a driver that runs made its files
before it ran, so that directory is damaged, and the client must not call its driver
gone. The wait is for an unfinished file alone. A directory with no command ring, a
ring that holds −1 and a ring whose heartbeat is older than the driver timeout are
refused at once, as before, with `not-found` and `driver-gone`.

## Alternatives considered

- **The driver creates the command ring last, so that a ready ring means a whole
  directory** (the card's second option). Lost on three counts. A client of this
  version beside a driver of the last one still meets the half-made files, and mixed
  versions run during an upgrade (invariant 3). The ring is itself empty and then
  `not-ready` while it is made, so a client that looks for it first needs the wait
  anyway. And it lengthens the time in which a second starting driver finds a
  directory with no command ring and removes it under a driver that is alive, which
  card 250 measures and owns.
- **Publish each file whole: create it under another name and link it into place.**
  Not chosen now: it changes three create paths in `cavalry-shm` and leaves a client
  beside an older driver where it was. It is compatible with this decision and can
  follow it.
- **Have `cavalry doctor` read the broadcast buffer too, and the test wait longer.**
  Lost: `doctor` would be right and a client started in that moment still refused,
  and the test is not the only client that starts right after its driver (the card).
- **Wait for a driver that has not begun: a missing command ring, or the ring a
  stopped driver left.** Aeron does: its client waits the driver timeout for the
  driver's file to be created and for a heartbeat that is not stale. Lost: a
  directory with no command ring, or with a stopped driver's, cannot be told from no
  driver, and a connect to nothing would take 10 s to say so where it now says at once
  what to start. A program that starts a driver and a client together waits for
  `cavalry doctor` to pass; after that the client connects.
- **A bound of its own, shorter than the driver timeout.** Lost: a second number to
  explain where one already means "a driver silent this long is dead", to a starting
  driver and to a client alike, and a driver whose CPU is throttled while it brings
  its files into memory can take longer than any small number chosen from a laptop.
- **At the bound, refuse with the file's `not-ready` message** (the card's words).
  Lost for a dead creator: the message says to wait and read again, which the client
  has done for 10 s. What is true by then is that the driver is gone, and
  `driver-gone` says to start one, which removes what the dead one left.
- **Tell the dead creator from a running driver by the heartbeat's age at the bound.**
  The first form of this change did, and its review found the fault: the bound is
  measured on a clock that never goes back and a heartbeat's age on the wall clock,
  so a wall clock set back during the wait, by a few milliseconds when the client
  started right after the creator stopped, left the heartbeat "fresh" at the bound
  and a dead creator's file answered with "wait and read again". Whether the
  heartbeat was stored again needs no clock.

## Evidence

- Card 243: one failure in 60 runs of `crates/cavalry/tests/cli.rs` at 50becf1, in
  `record_is_refused_with_the_drivers_message_when_nothing_publishes`, with the
  message above.
- The code as it was: `Cavalry::connect_to` in `cavalry-client` opened the three files
  once each; `BroadcastBuffer::create`, `CommandRing::create` and
  `CountersFile::create` in `cavalry-shm` each create the file, size it, map it, bring
  it into memory and write the magic last; `prepare_dir` in `cavalry-driver` waits the
  driver timeout for a `not-ready` command ring.
- A test that connects the moment the command ring appears, while `Driver::start` runs
  in another thread (`a_client_connects_while_the_driver_creates_its_files`,
  `crates/cavalry-client/tests/client.rs`): without the wait it fails in its first
  round, `client refused (command-ring): command ring rejected (not-ready)`. With the
  wait, 200 of 200 rounds connect.
- How long the wait is, release build, Apple M5 Pro, macOS 26.6.1, 200 connects each:
  a connect to a driver that is running takes 0.44 ms at least, 0.49 ms p50, 0.88 ms
  p99 and 9.7 ms at most; a connect made the moment the command ring appears takes
  16.8 ms at least, 19.0 ms p50, 22.8 ms p99 and 24.2 ms at most, which is one or two
  sleeps of 10 ms. The driver's files are whole between 10 and 20 ms after its command
  ring appears, on that laptop.
- The bound's two answers, in `a_file_left_unfinished_is_refused_at_the_driver_timeout`:
  a finished ring whose heartbeat is 5 s ahead of the clock and never stored again,
  beside a broadcast buffer left `not-ready`, is `driver-gone` at the bound; with the
  comparison of the two heartbeats taken out, the same test reads it as the buffer's
  `not-ready`. A running driver whose broadcast buffer reads `not-ready` is refused
  at the bound with the buffer's check.
- Aeron, `aeron-client`, `CommonContext.awaitCncFileCreation` and
  `Aeron.Context.connectToDriver`, read at commit 54a40fae47: the client waits while
  the driver's file is missing, shorter than its metadata, or has a version of zero
  ("CnC file is created but not initialised"), and then for a heartbeat, all up to
  `driverTimeoutMs`, 10 s by default, sleeping 1 ms or 16 ms between reads, and throws
  `DriverTimeoutException` at the deadline. Cavalry keeps Aeron's bound and its three
  states, and reads again every 10 ms, the interval a starting driver uses, because a
  read maps the files it finds whole anew.

## Consequences

- A client started once the driver's command ring is there connects, and needs no
  retry loop of its own, in Rust and in every client over the C ABI, since
  `cavalry_init` connects through the same call. `cavalry_init` can therefore take up
  to 10 s to return, which `spec/c-abi.md`, "Threads", now says.
- `cavalry doctor` can still pass a few milliseconds before the broadcast buffer is
  whole. A client that connects in that moment waits for it, so nothing is refused.
- A connect to a directory whose creator died takes up to 10 s to refuse, where it
  took less than a millisecond. The refusal then says the driver is gone and what to
  start. At most a thousand reads are made in that time, each of which maps the files
  it finds whole.
- A client started before the driver has made its command ring is still `not-found`,
  and one started while a stopped driver's directory is still there is still
  `driver-gone`: both say to start a driver, which may be starting. `cavalry doctor`
  passing is the sign that a client connects.
- Invariant 4: the wait counts nothing, and cannot. The client waits for the counters
  file among the three and holds none of the driver's counters until it is attached;
  a wait that ends attached lasted milliseconds, and one that ends at the bound ends
  with no driver to count it. What explains a refusal at the bound is its own message
  and `cavalry doctor`, which reports a command ring or a counters file that is not
  ready, and from 10 s after the creator stopped, a driver whose heartbeat is stale.
  `doctor` does not read the broadcast buffer, so a damaged one beside a running
  driver has no finding of its own; the client's refusal names it.
- Revisit when a driver's files are published whole, which makes the wait a rule for
  older drivers alone; and if a driver is ever measured to take a large part of the
  bound to create its files.

## References

- `spec/client-protocol.md`, "The driver directory" and "Liveness and timeouts", the
  driver timeout and the heartbeat interval; `spec/c-abi.md`, "Threads";
  `spec/command-ring.md`, `spec/broadcast-buffer.md` and `spec/counters-file.md`,
  "What a reader rejects", `too-short` and `not-ready`.
- ADR 0021, the client API; ADR 0027, bringing a mapping into memory when it is
  mapped; ADR 0049, the other wait that took the place of a refusal.
- Card 243; card 250, two drivers that start at once.
