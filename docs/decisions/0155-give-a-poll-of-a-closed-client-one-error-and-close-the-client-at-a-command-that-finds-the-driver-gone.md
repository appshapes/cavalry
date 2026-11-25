# 0155. Give a poll of a closed client one error, and close the client at a command that finds the driver gone

- **Status:** Accepted
- **Date:** 2026-11-24
- **Invariants affected:** 5, 6
- **Supersedes:** none. No page of `spec/` changes: `spec/c-abi.md`, "Error codes",
  lists the codes and fixes none of them to a call. The functions, their arguments
  and the codes of the C ABI stay as they are at version 6; which of two listed
  codes two calls return for a driver that is gone changes, as "Decision" says.

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
  command ring's consumer heartbeat and the client's conductor reading it there,
  which is up to 1 ms, the conductor's longest park. The command ring's writer
  reads the same heartbeat for itself before it claims.
- `Ok(0)`, for as long as it is called, where the service has no log open and its
  member names no recording, and after any of the errors above once the handle is
  gone and no recording can be found.

Through the C ABI those are −1 with `CAVALRY_ERR_CLOSED` (102), −1 with
`CAVALRY_ERR_FAULT_CLOSED` (122), −1 with `CAVALRY_ERR_COMMAND_RING` (107), and 0.
The third is not the service's alone: every command of a client, an add, a remove,
a start or a stop, returned the ring's refusal in that millisecond and
`ClientError::Closed` after it.

A program cannot be asked to know four answers. The example knew two and was wrong.
A loop that polls and takes no look at `closed()`, as the service of
`docs/how-to/take-a-services-snapshot.md` does, ended on an error in some stops and
polled a dead driver for ever in others.

## Decision

A driver that is gone is one condition and has one error. `ClusterService::poll` of
a closed client returns `ClusterError::Client(ClientError::Closed(reason))`, with
the reason `Cavalry::closed()` gives, and nothing else: it looks at the client
before it does anything, and an error that its work meets is returned as that one
when the client is closed by the time the work returns. A command that finds the
driver gone on the command ring, `CommandRingError::DriverClosed` or
`CommandRingError::DriverTimeout`, closes the client for that reason, as the
conductor does on the same reading within its next millisecond, and returns
`ClientError::Closed`. `cavalry_cluster_service_poll` is −1 with
`CAVALRY_ERR_CLOSED` for a closed client, and the tutorial's service ends on that
one error with its one line.

## Alternatives considered

**Change the example alone, to take the poll's `ClientError::Closed` as the close
that `closed()` gives.** It was the first form of this change, and an independent
read of it found the other two errors and the 0: the example would still have ended
with the library's line in the millisecond of the third answer and on the second,
and the sentence written for `poll`, that a close is an error only where the poll
has a command, was not true. Fifty runs of the tutorial's script passed on both
runners with that form, against one failure in fifty without it, so the runs did
not show what the read did. Every program in six languages would have had to carry
the same list.

**Return 0 from a poll of a closed client and leave the close to `closed()`.** It
makes the loop that forgets `closed()` poll a dead driver for ever. A loop that
forgets must end, and an error ends it. `Subscription::poll` of a closed client is
an error too, the fault `Fault::Closed`.

**Give the service's poll the one error and leave the commands as they were.** The
poll would have had to report a close that `Cavalry::closed()` denied for up to a
millisecond, or wait for the conductor. The command ring's writer has read the
heartbeat that the conductor closes on: closing on that reading is the same
decision made by the first thread that can make it.

**Map every error of a closed client's poll, or only the three known.** Every one:
an error met at a client that is closed says nothing a program can act on beside
the close, and a list of three is a list that the next command added to the poll
makes wrong. What it costs is under "Consequences".

## Evidence

- `crates/cavalry-client/tests/client.rs`,
  `a_command_that_finds_the_driver_closed_is_the_clients_close`: twenty drivers,
  each closed and a command sent at once. Without the change to the command's send
  it fails in its first round with `Some(CommandRing(DriverClosed))`.
- `crates/cavalry-client/tests/cluster_service.rs`,
  `a_poll_of_a_closed_client_is_the_clients_close_for_a_service_with_no_log` and
  `..._for_a_service_that_reads_its_log`: each polls after its driver's close until
  a poll is an error and holds that error and the next three polls to the one
  error. Without the poll's change the first fails with no error in 10 s while the
  client says `Some(DriverClosed)`, and the second with `Ok(0)` from the poll after
  its first error.
- The tutorial's script with a pause of 300 ms at the top of the example's loop, on
  a laptop: `master`'s example fails step 7 with the library's line, as on the
  runner; the example of this change passes its ten steps.
- Fifty runs of `scripts/run-a-cluster.sh` on each runner, `ci` started by hand on
  throwaway branches: `master`'s example, run 45530433822, 50 of 50 on Linux and 49
  of 50 on macOS, the one failure being this one; the example-only form of the
  change, run 45530437996, 50 of 50 on both. The pull request's body has the runs of
  the form that landed.

## Consequences

- A program's loop for a service is `poll` and one arm for the close. The
  documentation of `ClusterService::poll` and of `cavalry_cluster_service_poll` says
  so, and the client crate's README.
- A program that matched on `ClientError::CommandRing` holding `DriverClosed` or
  `DriverTimeout` for a command, or on `CAVALRY_ERR_COMMAND_RING` with that
  meaning, no longer sees it from a command: it sees `ClientError::Closed`, which
  it already had to handle a millisecond later. `CommandRingError` keeps the two
  variants: the client's conductor reads them for its own keepalive.
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
- The five clients get the rule through the C ABI with no change of their own: none
  holds a copy of the tutorial's service.

## References

- Work item 454 on the Brigade Board, which this decision was made in; work item
  446, where the failure was found.
- `crates/cavalry-client/src/service.rs`, `ClusterService::poll`;
  `crates/cavalry-client/src/client.rs`, the command's send;
  `crates/cavalry-ffi/src/cluster_service.rs`, `cavalry_cluster_service_poll`.
- `spec/client-protocol.md`, "Liveness and timeouts"; `spec/command-ring.md`, the
  consumer heartbeat; `spec/c-abi.md`, "Error codes".
- [Run a cluster](../tutorials/run-a-cluster.md), the tutorial whose step this
  holds.
