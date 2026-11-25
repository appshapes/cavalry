# 0155. Give a poll of a closed client one error, and close the client at a command that finds the driver gone

- **Status:** Accepted
- **Date:** 2026-11-24
- **Invariants affected:** 5, 6
- **Supersedes:** none. Amends `spec/c-abi.md`, the "cluster service" row of
  "Functions" and "Versions and what a reader rejects", within version 6.

## Context

`gates (macos)` of `ci` run 45523952345 failed in `just quickstart` on code that
was `master`'s (work item 454). `scripts/run-a-cluster.sh` stops the cluster's
leader and waits for the service beside it, `crates/cavalry/examples/adder-service.rs`,
to print the line the tutorial shows: that the driver closed its client, with the
total and the position it read to. The service had ended with the client's own
error instead. Its loop called `ClusterService::poll(...)?` and then asked
`Cavalry::closed()`, and the poll had met the close first.

Reading the poll for what a stopped driver gives it found four answers for the one
condition, by what the service held and when it polled:

- `ClusterError::Client(ClientError::Closed(reason))`, where the poll sent a
  command at a client the conductor had already closed: the stop of its log's
  replay, or the open of the log.
- `ClusterError::Log(Fault::Closed)`, where the read of the log found the client
  closed and the log's replay had already ended, so that closing the handle sent no
  command.
- `ClusterError::Client(ClientError::CommandRing(CommandRingError::DriverClosed))`,
  where the poll sent a command in the time between the driver storing −1 in the
  command ring's consumer heartbeat and the client's conductor reading it there.
  The conductor parks for 1 ms at the most between two looks, and the host may
  hold it longer. The command ring's writer reads the same heartbeat for itself
  before it claims.
- `Ok(0)`, for as long as it is called, where the service has no log open and its
  member names no recording, and after any of the errors above once the handle is
  gone and no recording can be found.

Through the C ABI those are −1 with `CAVALRY_ERR_CLOSED` (102), −1 with
`CAVALRY_ERR_FAULT_CLOSED` (122), −1 with `CAVALRY_ERR_COMMAND_RING` (107), and 0.
`spec/c-abi.md` promised the second and the last of them for this call: "a fault's
code, 120 to 122, when the read of the log stopped on one", and 0 "while the driver
hosts no member that has a log yet". The third is not the service's alone: every
command of a client that tells its caller how it went, an add, a start, a stop,
returned the ring's refusal in that time and `ClientError::Closed` after it, and
`spec/command-ring.md`, "Heartbeat and correlation ids", already says what the
client does there: "A client that finds `driver-closed` or `driver-timeout` reports
it to its caller and closes." The client reported it and did not close.

A program cannot be asked to know four answers. The example knew two and was wrong.
A Rust loop of `poll(...)?` with no look at `closed()` ended on an error in some
stops and polled a dead driver for ever in others. The Python service of
`docs/how-to/take-a-services-snapshot.md`, whose `poll` returns −1 and raises
nothing, did not look at what the poll returned and polled a dead driver for ever
in all of them.

## Decision

A driver that is gone is one condition and has one error. `ClusterService::poll` of
a closed client returns `ClusterError::Client(ClientError::Closed(reason))`, with
the reason `Cavalry::closed()` gives, and nothing else: it looks at the client
before it does anything, and an error that its work meets is returned as that one
when the client is closed by the time the work returns. A command that finds the
driver gone on the command ring, `CommandRingError::DriverClosed` or
`CommandRingError::DriverTimeout`, closes the client for that reason, as the
conductor does on the same reading at its next look, and returns
`ClientError::Closed`: that is each command that tells its caller how it went, and
the remove with which a client gives back a counter it could not map, whose answer
it drops. The removes a handle sends as it closes, and the client's own close,
write to the ring themselves and take no answer, as before.
`cavalry_cluster_service_poll` is −1 with `CAVALRY_ERR_CLOSED` for a closed client,
`spec/c-abi.md` says so in the row of the call, and the tutorial's service ends on
that one error with its one line. And a service's close at a client that is closed
is no error, as a publication's is none: a driver that is gone cannot be told, so
`ClusterService::close` drops what it held and returns `Ok`, and
`cavalry_cluster_service_close` 0. A program that ends on the poll's one error
closes its service as it always does.

The C ABI stays at version 6. Its number counts the header: no function, argument,
struct or define changes, so a package generated from version 6 loads this library
and calls it rightly, and a program that handled the three codes handles the one.
Nothing is released (ADR 0003), so no built program held the page to the two
answers it had promised. The page's versions section says the same.

## Alternatives considered

**Change the example alone, to take the poll's `ClientError::Closed` as the close
that `closed()` gives.** It was the first form of this change, and an independent
read of it found the other two errors and the 0: the example would still have ended
with the library's line in the moment of the third answer and on the second,
and the sentence written for `poll`, that a close is an error only where the poll
has a command, was not true. Fifty runs of the tutorial's script passed on both
runners with that form, against one failure in fifty without it, so the runs did
not show what the read did. Every program in six languages would have had to carry
the same list.

**Take the C ABI to version 7 for it.** The number tells a generated package
whether its bindings fit the library, and they do. Work item 419 takes the header
to version 7 for a service's timers; a number spent here would say that a package
of version 6 cannot call this library, which is false.

**Return 0 from a poll of a closed client and leave the close to `closed()`.** It
makes the loop that forgets `closed()` poll a dead driver for ever. A loop that
forgets must end, and an error ends it. `Subscription::poll` of a closed client is
an error too, the fault `Fault::Closed`.

**Give the service's poll the one error and leave the commands as they were.** The
poll would have had to report a close that `Cavalry::closed()` denied until the
conductor's next look, or wait for the conductor. The command ring's writer has read the
heartbeat that the conductor closes on: closing on that reading is the same
decision made by the first thread that can make it.

**Map every error of a closed client's poll, or only the three known.** Every one:
an error met at a client that is closed says nothing a program can act on beside
the close, and a list of three is a list that the next command added to the poll
makes wrong. What it costs is under "Consequences".

## Evidence

- `crates/cavalry-client/tests/client.rs`,
  `a_command_that_finds_the_driver_closed_is_the_clients_close`: twenty drivers,
  each closed and a command sent at once. Run once without the change to the
  command's send, on a laptop, it failed in its first round with
  `Some(CommandRing(DriverClosed))`. A round whose command comes after the
  conductor's look passes without the change, so the test finds the fault often and
  not always.
- `crates/cavalry-client/tests/cluster_service.rs`,
  `a_poll_of_a_closed_client_is_the_clients_close_for_a_service_with_no_log` and
  `..._for_a_service_that_reads_its_log`: each polls after its driver's close until
  a poll is an error and holds that error and the next three polls to the one
  error, and the second holds the service's position to the end of the entry it
  took. Run without the poll's change, the first failed with no error in 10 s while
  the client said `Some(DriverClosed)`, and the second with `Ok(0)` from the poll
  after its first error.
- `crates/cavalry-client/tests/cluster_service.rs`,
  `a_service_closes_with_no_error_at_a_client_that_is_closed`: a service that reads
  its member's log, its driver closed, one poll, and then its close. Run without
  the close's change it failed with `Client(Closed(DriverClosed))` from the close.
  A close that finds its client closed only as it stops the log's replay is `Ok`
  by the same rule and has no test: the moment cannot be made on purpose.
- `conformance/c/closed.c`, the C runner's closed round: a service beside a driver
  with no member, the driver stopped, the first poll that fails and the three
  after it held to −1 with `CAVALRY_ERR_CLOSED`, and the closes after them to 0.
  The round's service holds no log, so its close would be 0 without this change
  too; the round is evidence for the poll.
- The tutorial's script with a pause of 300 ms at the top of the example's loop, on
  a laptop: `master`'s example fails step 7 with the library's line, as on the
  runner; the example of this change passes its ten steps.
- Fifty runs of `scripts/run-a-cluster.sh` on each runner, `ci` started by hand on
  throwaway branches: `master`'s example, run 45530433822, 50 of 50 on Linux and 49
  of 50 on macOS, the one failure being this one; the example-only form of the
  change, run 45530437996, 50 of 50 on both; the library's change as the second
  read found it, before that read's fixes, run 45532875913, 50 of 50 on both; with
  that read's fixes, run 45534683647, 50 of 50 on both; and as this pull request
  opened, run 45537312460, 50 of 50 on both.

## Consequences

- A program's loop for a service is `poll` and one arm for the close. The
  documentation of `ClusterService::poll` and of `cavalry_cluster_service_poll` says
  so, and `spec/c-abi.md` and the client crate's README. Through a generated client
  the arm is a look at what the poll returned: a client's poll returns the −1 and
  raises nothing, so a loop that does not look still polls for ever, as the
  how-to's did. Its loop now ends on a poll that failed, and its page says what
  the reader then sees.
- A program that matched on `ClientError::CommandRing` holding `DriverClosed` or
  `DriverTimeout` for a command, or on `CAVALRY_ERR_COMMAND_RING` with that
  meaning, no longer sees it from a command: it sees `ClientError::Closed`, which
  it already had to handle a moment later. `CommandRingError` keeps the two
  variants: the client's conductor reads them for its own keepalive, and the first
  keepalive of a client that attaches as its driver stops returns the ring's
  refusal still, there being no client yet to close.
- A service's poll that returned 0 at a closed client returns an error. A program
  that polled a closed client on purpose, and none in this repository does, now
  ends.
- An error of another kind that a poll meets in the same call in which its client
  closes, an entry that is no entry of the page, say, is reported as the close. The
  log is as it was, and the service that starts again meets the entry again.
- `Subscription::poll` of a closed client stays the fault `Fault::Closed`, which is
  one answer. The polls of a cluster's session, `ClusterSession`, were not read for
  this change: finding that one of them gives two answers for a driver that is gone
  would be the reason to bring it under this rule.
- The five clients' packages do not change: none holds a copy of the tutorial's
  service, and each passes the poll's answer through. Their conformance runners do
  not hold the closed round yet. The C runner does, on the ABI itself.

## References

- Work item 454 on the Brigade Board, which this decision was made in; work item
  446, where the failure was found.
- `crates/cavalry-client/src/service.rs`, `ClusterService::poll` and
  `ClusterService::close`;
  `crates/cavalry-client/src/client.rs`, the command's send;
  `crates/cavalry-ffi/src/cluster_service.rs`, `cavalry_cluster_service_poll`.
- `spec/client-protocol.md`, "Liveness and timeouts"; `spec/command-ring.md`, the
  consumer heartbeat; `spec/c-abi.md`, "Error codes".
- [Run a cluster](../tutorials/run-a-cluster.md), the tutorial whose step this
  holds.
