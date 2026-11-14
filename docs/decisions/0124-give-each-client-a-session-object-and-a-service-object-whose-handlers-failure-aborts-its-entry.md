# 0124. Give each client a session object and a service object, whose handler's failure aborts its entry

- **Status:** Accepted
- **Date:** 2026-11-14
- **Invariants affected:** 1, 2, 6
- **Supersedes:** none. It decides what ADR 0122 and ADR 0123 left to "each client's
  session object" and "each client's service object": the shape of the two objects in
  the five generated clients, and what the Python client, the first of them, does
  where its runtime forces a choice. It changes no declaration of `spec/c-abi.md`.

## Context

Versions 4 and 5 of the C ABI give a program a session with a cluster and a cluster's
service (ADR 0122, ADR 0123). The five clients bind the functions and wrap none of
them yet. Three things about the wrappers are not settled by the ABI.

First, a service's handler answers for its entry. In C the handler returns nothing
and calls `cavalry_cluster_service_action`; in a language with exceptions the
handler can also fail, and the wrapper decides what becomes of the entry then. For a
subscription and a committed log the ABI's poll consumes every fragment whatever the
handler does, so each client hands every fragment over and raises after the poll
(ADR 0046; ADR 0070). A service's poll is the first whose handler can stop it.

Second, a service's poll is a call on its client: it sends its commands with the
client and holds it for the run of the handler (`spec/c-abi.md`, "Threads"). The
Python `Client` takes a lock around every call on its handle, "never on a message's
path", and lets threads share it.

Third, the Python binding keeps the interpreter's lock, the GIL, held across the
per-message functions and releases it across every function that waits on the
driver. A service's poll is both: it is on every entry's path, and it waits for the
driver when it opens the member's log, at a session's opening, where a term's leader
changes and at a recording's end. ADR 0123 left that to this change.

## Decision

Each client has a session object, made by the client's `connect_cluster`, and a
service object, made by its `cluster_service`, with the ABI's functions as methods
under the names of the Rust `ClusterSession` and `ClusterService`. A session's
handler is given a reply's payload and the reply's values, its leadership term id
and the cluster's time. A service's handler is given a session message's payload,
the entry's values and the service, and **what it returns is its action**: nothing
or continue, break, or abort. **A handler that fails has its entry aborted**: when
it raises or throws, or returns a value that is no action, the wrapper calls
`cavalry_cluster_service_action` with `CAVALRY_ACTION_ABORT` from inside the
trampoline, the ABI ends the poll with the entry not taken, the poll raises once the
ABI has returned, and the next poll hands the entry over again. A session's handler
is held as a subscription's is: every reply reaches it and the poll raises after.
The values are read once per call into one struct the object keeps. In Python the
service's poll **keeps the GIL held and takes no lock**: the client a service was
made on is used by the thread that polls the service and by no other, from
`cluster_service()` until the service's close.

## Alternatives considered

- **Take the entry whose handler failed and end the poll, as Aeron does.** Aeron's
  `Image.boundedControlledPoll` moves past a fragment before it calls the handler,
  passes a handler's exception to the error handler and stores the position past the
  fragment; `BoundedLogAdapter` hands a service's entries through it, so an Aeron
  service that throws in `onSessionMessage` has the exception counted and the entry
  skipped. It lost because of what the position means here: `service.position()` is
  "how far the service has applied the log" (`spec/c-abi.md`), and an entry whose
  handler failed is not applied. A service that goes on past it holds a state no
  other member's service holds, and says nothing (invariant 1). Aborting keeps the
  service at the entry, loudly: every poll raises until the handler takes it.
- **Hand every entry over and raise after, as the committed log's poll does.** The
  entries after the failed one would be applied on a state that lacks it. The
  committed log's README has a three-step recipe for a service to avoid exactly that;
  the service's poll can end at the entry, so the recipe is not needed.
- **An action method on the service instead of the return value.** It is the ABI's
  form, forced there by a handler that returns nothing. A handler that forgets the
  call continues, which is right; but a returned value is what the Rust handler
  gives, costs no call for continue, and cannot be made from outside the handler.
- **A reply method on the entry, for the entry's own session.** The ABI replies to
  any session open in the log at the entry, not only the entry's. The service is
  passed to the handler so that a handler needs no closure to reply.
- **A wrapper class with a property for each value, read lazily, as `Header` is.**
  A handler reads two to four values of an entry, each an ABI call; one call before
  the handler fills the struct for all of them, as the committed log's trampoline
  reads the position. The struct is the object's own memory, not a pointer into the
  log, so a kept one reads the last entry's values and never freed memory.
- **Release the GIL across the service's poll**, so that the process's other threads
  run while the poll waits for the driver. Each call-back then takes the GIL back.
  Measured below: beside one Python thread that computes, a poll of 10 messages took
  5.8 to 6.3 ms at its p99, against 4.4 to 4.6 µs held. That is every entry's tail
  (invariant 2), paid to shorten four waits of about 10 ms in a round of 17,900
  polls.
- **Take the client's lock in the service's poll**, so that threads may share the
  service's client. It is a lock on a message's path, which the client's own rule and
  `clients/lint.py` refuse. It would also need a re-entrant lock: a handler that
  calls its client is refused by the ABI with `CAVALRY_ERR_POLLING` today, and would
  wait on its own thread's lock for ever. `spec/c-abi.md` already says what a program
  does: its other threads get a client of their own.
- **A service object that makes a client of its own**, so that no thread can share
  it. The ABI and the Rust API take the client; a wrapper that hides it is a second
  shape for the tutorial and the four other clients to explain.

## Evidence

- `conformance/python/cluster.py`, the Python runner's cluster round, beside three
  members started with `cavalry run`: the C runner's checks through the client, then
  a handler that replies and then raises, handed its entry twice at one position
  with one reply read by the session; a handler that returns 3, refused with
  `CAVALRY_ERR_CLUSTER_ACTION` and handed its entry again; `BREAK` with two entries
  waiting, which takes one; nine calls refused from inside a service's handler,
  three of them on the client its poll holds, and eight from inside a session's.
  25 of 25 runs on an Apple M5 Pro laptop, macOS 26.6.1, Python 3.14.6, each on a
  new driver and a new cluster.
- Two changes to the generated code, each made and undone, fail the round. Without
  the abort for a handler that raised: "after its handler raised on the entry at 320
  the service is at 416". With `BREAK` not passed to the ABI: "the leader's polls
  took [2], not one each".
- The GIL, on that laptop, three runs: one subscription's poll bound through
  `ctypes.PyDLL`, which holds the GIL, and through `ctypes.CDLL`, which releases it
  around the call and takes it back in each call-back; 10 messages of 32 bytes over
  `cavalry:ipc` offered, then polled, timed per poll. Alone, 3,000 polls: p50 3.3 to
  4.1 µs held and 3.4 µs released, p99 4.4 to 5.3 µs held and 4.6 to 4.9 µs released;
  the pair is inside the spread between runs. Beside one thread in a Python loop,
  300 polls: held, p50 3.2 to 3.7 µs, p99 4.4 to 4.6 µs, max 12.5 to 15.7 µs;
  released, p50 4.4 to 6.0 µs, p99 5.8 to 6.3 ms, max 6.1 to 12.4 ms. The
  interpreter's switch interval is 5 ms.
- The waits a held GIL costs, from the round's report in the same 25 runs: of 17,790
  to 18,030 polls of three services, 4 took over 1 ms in every run, the three that
  opened a member's log and the leader's at the session's opening; the longest took
  7.4 to 12.2 ms.
- Aeron, read on 2026-11-14 at `master`, version 1.55.0-SNAPSHOT: `Image.java`,
  `controlledPoll` and `boundedControlledPoll`; `BoundedLogAdapter.java`, `onFragment`
  and `poll`.

## Consequences

Easier: a Python program holds a session with a cluster and runs a cluster's
service, and the four other clients have the shape to copy: the handler's
arguments, the action as the returned value, the abort for a handler that failed,
and the checks of the Python round. A service's handler that fails cannot make its
member's service diverge without the program hearing of it at once.

Harder: a handler that always fails on one entry stops its service at that entry,
which is the point, and the program that catches the error and polls again raises
again. A handler must be ready to be handed an entry twice: it replies first and
changes its state last. In Python every other thread stops while a service's poll
waits for the driver, about 10 ms at a log's opening and at a session's on a
laptop, and up to the driver timeout when the driver does not answer; and the
service's client is one thread's. A client's `HandlerError` has two more forms.

Revisit: when a runtime without a GIL is supported, since the Python client's
"one thread at a time" then rests on nothing for a service's client; when a service
needs its client from inside its handler (ADR 0123, Revisit); if a client's users
ask to take a failed entry and go on, which is a choice a program can make itself
by catching in its own handler.

## References

- Work item 229 of the Brigade Board
- `clients/python/README.md`, "The cluster session", "The cluster service", "The
  call", "Threads" and "The exception"; `clients/README.md`; `conformance/README.md`
- `spec/c-abi.md`, "Functions", "Calls during a claim or a poll" and "Threads";
  `spec/cluster.md`, "Sessions", "The service" and "Replies"
- ADR 0046, ADR 0070, ADR 0117, ADR 0118, ADR 0121, ADR 0122, ADR 0123
- Aeron, read 2026-11-14 at `master` (1.55.0-SNAPSHOT):
  `aeron-client/src/main/java/io/aeron/Image.java`
  (<https://github.com/aeron-io/aeron/blob/master/aeron-client/src/main/java/io/aeron/Image.java>)
  and `aeron-cluster/src/main/java/io/aeron/cluster/service/BoundedLogAdapter.java`
  (<https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/service/BoundedLogAdapter.java>)
