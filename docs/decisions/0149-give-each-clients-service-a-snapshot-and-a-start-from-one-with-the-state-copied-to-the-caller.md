# 0149. Give each client's service a snapshot and a start from one, with the state copied to the caller

- **Status:** Accepted
- **Date:** 2026-11-22
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. It decides what ADR 0146 left to "the five clients' objects":
  the shape of a service's snapshot and of its start from one in the five generated
  clients, as ADR 0124 decided the shape of the service object itself, and what the
  Python client, the first of them, does. It changes no declaration of
  `spec/c-abi.md`.

## Context

Version 6 of the C ABI gives a service three calls (ADR 0146):
`cavalry_cluster_service_snapshot_due`, a `bool` with the position through a
pointer; `cavalry_cluster_service_snapshot`, which returns 1, 0 or −1; and
`cavalry_cluster_service_restore`, which returns a restored code and hands back the
state as a pointer and a length into bytes the handle holds until the service's next
poll or its close. The five clients bind the three and wrap none of them yet, so a
service written in Python, Node, Go, the JVM or .NET still reads its member's log
from position 0 at every start.

Three things about the wrappers are not settled by the ABI. A pointer that the next
poll frees is a use after free in a language whose programmer never frees. The
restore has three answers, of which one carries bytes. And a state's size is the
program's, up to 1 GiB, where everything else a service's object hands a program is
one frame.

Every runner's cluster round checks a service's object against the C runner's (ADR
0124). No round takes a snapshot: nothing could ask a `cavalry run` member for one
until `cavalry snapshot` (ADR 0147).

## Decision

Each client's service object gains three members, named for the Rust calls in the
language's casing, with one shape in every client:

- **`snapshot_due`** gives the snapshot's log position, or the language's "none": the
  position the ABI wrote, when the ABI answered `true`.
- **`snapshot(state)`** takes the program's bytes and gives a boolean: true when the
  service wrote the snapshot and told its member, false when the member lists one at
  or past the position already. Each of the ABI's refusals is the client's error for
  a call that is no message, with the ABI's code and message: in Python a
  `CavalryError`.
- **`restore`** gives the restored code and the state together, the state a copy the
  caller owns, made before the call returns, and nothing for the two codes that
  carry none: in Python `(restored, state)`, `state` a `bytes` or `None`.

Each client's runner gains a snapshot round, after its cluster round and on the log
that round left, of which the Python round, `conformance/python/snapshot.py`, is the
shape. `cavalry snapshot` is run beside each member; each of three services gives
its state at the position the command prints; a new service beside each member
starts from it with `restore`, which answers the state only when the member lists
the snapshot; and after a second request the new services' first entry is the one at
the snapshot's position, and a service that reads from position 0 reaches the states
they hold. The other four clients and the C runner gain the round in changes of
their own.

## Alternatives considered

- **The state as a view into the handle's bytes, as a fragment's body is a view into
  the log.** Lost. A body's view is released when the handler returns, inside the
  call that made it. A restored state would stay valid across calls, until the next
  poll frees it: a view that a program keeps past its first poll reads freed memory,
  in a language that has no word for that. One copy at a start, of a state the
  program then owns, costs a start nothing that matters: the replay that read the
  bytes took tens of milliseconds (ADR 0132, Evidence).
- **`restore` gives the state alone, and "none" for the two other answers.** Lost.
  "No member yet, ask again" and "no snapshot, read from the start" ask different
  things of a program, and a program that polls after the first has begun its
  service with no state and can never ask again.
- **`restore` raises for "no member yet".** Lost. It is an answer and no failure, as
  a poll that reads nothing is, and the ABI gives it a code and sets no error.
- **`snapshot` gives the ABI's 1 or 0.** Lost. In every one of the five languages a
  boolean says "written or not", and a program that writes `if service.snapshot(...)`
  reads right.
- **One call that takes a function and gives it the state, so that no copy is
  made.** Lost. It is a second kind of callback beside the handler, with the
  handler's questions again, what a raise does and what the call returns, for one
  call at a start.
- **A snapshot round inside each client's cluster round.** Lost. The cluster round
  ends with its session's close and its services closed, and a round of 500 lines in
  each of six languages would grow by half. A round of its own runs on the log the
  cluster round left, which gives its services a log with every kind of entry to
  read before the request.
- **The rounds check the member's listing by its counter.** Lost. No client reads a
  counter (`spec/c-abi.md`, "Out of this version"). A new service's `restore` gives
  the state only when its member names the snapshot's recording, which the member
  does only for a snapshot it lists: the round reads the listing through the client
  it tests.

## Evidence

- `conformance/python/snapshot.py`, on the laptop, an Apple M5 Pro, macOS 26.6.1,
  beside three `cavalry run` members that had run the cluster round. Each service's
  `restore` answers `CAVALRY_CLUSTER_RESTORED_FROM_START` and no state, and a second
  is refused with code 150. Each reads an entry, and a snapshot where none is due is
  refused with 147. `cavalry snapshot` exits 0 beside one member and prints log
  position 992, and exits 1 beside the two others, naming that member. Each service's
  poll ends at the request, handed over as `CAVALRY_CLUSTER_ENTRY_SNAPSHOT`, with
  `snapshot_due()` at 992 and one state of 2,472 bytes beside the three members;
  from inside the handler there, `snapshot_due()` is `None` with the thread's error
  at `CAVALRY_ERR_POLLING`, and `snapshot` and `restore` raise with it. Each
  `snapshot(state)` is `True`, no snapshot is due after, and a second is refused with
  147. The three services are closed. A new service beside each member starts with
  `CAVALRY_CLUSTER_RESTORED_STATE` and the 2,472 bytes as given, at position 992, and
  is refused a second `restore`. `cavalry snapshot` is run again: each new service's
  one entry is the request at 992, its snapshot is due at 1,088, the three hold one
  new state, and each `snapshot(state)` is `True`; a start beside each member is then
  given that state at 1,088. A service that never calls `restore` reads from position
  0, meets the first request with the first state and the second with the second,
  and its `snapshot` is `False` at each.
- A read of the change before the push, by an agent that was not given the author's
  conclusions, found the round's first form weaker than it said: a service that
  started from its snapshot was handed no entry after, so "no entry below the
  snapshot" held of nothing; "read to the end" was 0.3 s of quiet; and a refused
  `snapshot_due` in the handler was not told from "none due". The second request, the
  wait on the request itself and the thread's error answer the three.
- `clients/lint.py` finds no lock, no raise and no allocation in the per-message
  functions as before. None of the three calls is bound as a per-message function:
  `snapshot` and `restore` wait for the driver, and `snapshot_due`, which a loop asks
  after each poll, releases the GIL in Python as `position` does. The Python README
  says what a program beside a busy second thread does about that.

## Consequences

- A service in Python takes a snapshot and starts from one. The four other clients
  follow, one change each, with this shape and this round; until then each of their
  READMEs says that its service has no such call.
- A program holds its state twice while it starts: the handle's bytes until its
  first poll, and its own copy.
- `docs/how-to/take-a-services-snapshot.md` shows the three calls in a Python
  program, which is work item 335's "the tutorial or a how-to shows a service's
  snapshot in one client language".
- Revisit: when a state does not fit memory twice, with ADR 0146's call that gives
  the state in pieces; when a client reads counters, for a round that reads the
  member's listing directly.

## References

- Work item 335 on the Brigade Board, "Take a service's snapshot through the C ABI
  and the five clients"; work item 366, feature 6.
- `spec/c-abi.md`, "A service's snapshot and its start" and "Lifetimes";
  `clients/README.md`; `conformance/README.md`, "The snapshot round".
- `clients/generate.py` (`write_python`), `clients/python/README.md`,
  `conformance/python/snapshot.py`, `conformance/python/run.sh`.
- [ADR 0146](0146-take-the-c-abi-to-version-6-with-a-services-snapshot-and-its-start-from-one.md),
  the ABI's three calls;
  [ADR 0147](0147-ask-a-member-for-a-snapshot-with-a-control-toggle-that-the-asking-command-owns.md),
  `cavalry snapshot`;
  [ADR 0124](0124-give-each-client-a-session-object-and-a-service-object-whose-handlers-failure-aborts-its-entry.md),
  the service object;
  [ADR 0131](0131-have-a-service-write-its-own-snapshot-and-its-member-list-it.md)
  and
  [ADR 0132](0132-start-a-service-from-the-snapshot-its-member-names-when-its-program-asks.md),
  the Rust service's snapshot and its start.
