# 0146. Take the C ABI to version 6 with a service's snapshot and its start from one

- **Status:** Accepted
- **Date:** 2026-11-22
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. Amends `spec/c-abi.md` to version 6 with three functions of
  the "cluster service" group, four defines, four error codes and a section of its
  own, "A service's snapshot and its start"; every declaration of version 5 is
  unchanged (ADR 0051, ADR 0123). It is the ABI's part of work item 335, which ADR
  0131 left to its own change, and takes up ADR 0132's consequence that a service
  written through the ABI reads its member's log from position 0 at every start.

## Context

A Rust service takes a snapshot of its program's state and starts from one (ADR
0131, ADR 0132). Its poll hands the handler `Entry::Snapshot` at the leader's
request and ends after it; `ClusterService::snapshot_due` then gives the
snapshot's position, and `ClusterService::snapshot(client, state)` writes the state
before the next poll. Before its first poll, `ClusterService::restore(client)`
answers `Restored::NoMember`, `Restored::FromStart` or `Restored::State(bytes)`. The
calls fail with four kinds of `ClusterError`: `NoSnapshotDue`, `SnapshotTooLong`,
`SnapshotFailed { cause }` and `RestoreTooLate`.

The ABI at version 5 has none of this. A service written through it is handed the
request as `CAVALRY_CLUSTER_ENTRY_OTHER`, has no call that takes a state or gives
one back, and maps the four errors to the generic code, 11, which no call raises.
So a service in Python, Node, Go, the JVM or .NET reads its member's log from
position 0 at every start, and beside it `cluster.log_purge_position` stays 0: no
recording of the member's log can be purged (ADR 0132, Consequences).

Three things have no form in C yet. A Rust `Option<i64>` is the answer of a read.
A Rust `Vec<u8>` of up to 1 GiB is the answer of a call, and must outlive the call
while the caller copies it. And a Rust `bool` says whether a snapshot was written.

## Decision

Version 6 adds three functions to the "cluster service" group, as the Rust calls
are, each named for its Rust call:

- `bool cavalry_cluster_service_snapshot_due(const cavalry_cluster_service_t *service,
  int64_t *position)`: `true` with the position written, `false` with nothing written,
  as `cavalry_replay_ended` and `cavalry_committed_log_failed_at` answer.
- `int cavalry_cluster_service_snapshot(cavalry_cluster_service_t *service, cavalry_t
  *client, const uint8_t *state, size_t length)`: 1 when the ack is on its way, 0 when
  the member lists one at or past the position already, −1 with the error. A state
  over 1 GiB is refused before a byte of it is read.
- `int cavalry_cluster_service_restore(cavalry_cluster_service_t *service, cavalry_t
  *client, const uint8_t **state, size_t *length)`: a restored code,
  `CAVALRY_CLUSTER_RESTORED_NO_MEMBER` 1, `_FROM_START` 2 or `_STATE` 3, the Rust
  `Restored`'s in order, with the state's bytes held by the handle until the
  service's next poll or its close, which frees them, poisoned first in a debug build.

The request has a kind of its own, `CAVALRY_CLUSTER_ENTRY_SNAPSHOT`, 6, after
`_OTHER`, so that every define of version 5 keeps its value. The four errors have
codes of their own, 147 to 150, `CAVALRY_ERR_CLUSTER_NO_SNAPSHOT_DUE`,
`_SNAPSHOT_TOO_LONG`, `_SNAPSHOT_FAILED` and `_RESTORE_TOO_LATE`, in the Rust order.
Three carry the ABI's own message, which names the C calls; a snapshot that failed
carries the Rust message, which names its step in the parentheses. The snapshot and
the start count as calls on the client, as the poll does, and from inside a
service's handler each of the three is refused with `CAVALRY_ERR_POLLING`.

## Alternatives considered

- **`_SNAPSHOT` as 5, the Rust order, with `_OTHER` moved to 6.** Lost. A define of
  version 5 would change its value under its name, which the page rules out ("a newer
  library ... never with a different behaviour under the old names"). Every other
  entry keeps its number; the order of the defines differs from the Rust enum's by
  one place, and the constants table says so.
- **The request still handed over as `_OTHER`, and `snapshot_due` the one way to
  know.** Lost. A handler that counts entries by kind would count the request as a
  timer event or a membership change, and a program ported from Aeron looks for the
  request in its handler, as `onTakeSnapshot` is a callback there. The work item's
  brief asks for the kind.
- **`snapshot_due` as an `int64_t`, −1 for none.** Lost. −1 is what every `int64_t`
  function of the page returns when it cannot answer, so a caller could not tell
  "none due" from a null handle without reading the error. A `bool` with the
  position through a pointer is the shape the page already gives a read whose
  answer may be absent.
- **`snapshot` as a status, 0 or −1, with the Rust `bool` through a pointer.** Lost.
  The answer is a count of snapshots written, 1 or 0, as a poll's answer is a count
  of entries; a pointer for it is one more argument to get wrong, and a caller that
  tests `< 0`, as it does for a poll, reads it right.
- **The state given from inside the handler, at the request.** Lost, for ADR 0131's
  reason: the call waits for the driver while it writes, and a handler that held the
  poll for that long would hold the service's sessions with it. The Rust API takes
  the state after the poll, and so does this.
- **The restored state copied into a buffer the caller gives, after a call that
  says its length.** Lost. It is two calls where the Rust API has one, with a buffer
  of up to 1 GiB that the caller sizes, and the handle holds the bytes already: the
  Rust call returned them. A caller that copies them once does what the second call
  would do.
- **The restored state handed to a handler of its own.** Lost. It is a second
  handler type, and every client builds its trampoline once, for the one fragment
  handler (ADR 0070).
- **The state held for the handle's life, or freed by a function of its own.** Lost.
  The first keeps up to 1 GiB after the program has its copy. The second is one
  more function, and a program that forgets it holds the bytes for the handle's
  life. The next poll is a call every program makes after its start, so the bytes
  are freed there.
- **The Rust `SnapshotCause` as five codes, or as a value a function reads.** Lost.
  Every cause asks the same of a program: call again, or poll on. The operator's next
  step differs by cause, and the message names the cause and that step. The page
  names the value under "Out of this version".
- **The four errors on the generic code, as version 5 had them.** Lost. A program
  cannot tell its own mistake, a call where nothing is due or a call after its
  start, from a snapshot that failed on the driver, and the work item asks for codes
  of their own.
- **The Rust messages for all four.** Lost for three of them. The messages of
  `NoSnapshotDue`, `SnapshotTooLong` and `RestoreTooLate` name `ClusterService::`
  calls and `Entry::Snapshot`, which a C caller has never seen. A snapshot that
  failed names no call, so its message is the Rust one.
- **A define for the most a state holds, 1 GiB.** Lost. No call of a correct program
  needs it: the refusal's message carries both lengths, as the reply's 1,344 bytes
  have no define either.

## Evidence

- `crates/cavalry-ffi/tests/cluster_snapshot.rs`, three members over three drivers
  on the loopback, a service through the ABI beside each and a session through the
  ABI on a fourth driver, 11 of 11 runs on an Apple M5 Pro laptop, macOS 26.6.1,
  at a load average of 2 to 4. Before the first poll every service answers
  `_FROM_START`, a second restore 150 and a snapshot 147. Three messages of 600
  bytes later the leader's process asks for a snapshot: every service's poll ends at
  the request, handed over as `_SNAPSHOT` with no session, no leader and no payload,
  and `snapshot_due` gives the position the member's own snapshot is of. From inside
  the handler at the request, `snapshot_due`, `snapshot` and `restore` each answer
  `CAVALRY_ERR_POLLING`, and the refused restore writes a null state and a length of
  0 over what it was given. A state one byte over 1 GiB, at a pointer to no buffer,
  is refused with 148 and both lengths, and a null state with a length with 131; the
  snapshot stays due. The service beside a member stopped while its driver runs
  writes its recording and gives the snapshot up at the ack with 149, `(snapshot-failed,
  ack)`, still due. The two others write a state of 1,816 bytes, answer 1, are then
  refused with 147, and both members list their service's snapshot at that position.
  A new service on a follower's client starts from it: `_STATE` with the bytes as the
  program gave them, the position, one session open; a second restore is 150; it
  reads the fourth message and no entry below the snapshot, and its program's state
  is the leader's.
- The same file, a service beside a driver that hosts no member. Each of the three
  calls refuses a null handle or pointer with 131, and a refused start writes a null
  state and a length of 0 over what it was given. The start answers `_NO_MEMBER`
  twice, with a poll between that reads nothing, and sets no error; no snapshot is
  due, with nothing written; and a state of no bytes at a null pointer is refused
  with 147, for the one reason that none is due.
- `src/error.rs`, the four errors on 147 to 150; the three with the ABI's words name
  no Rust call, and a snapshot that failed keeps the Rust words for each of the five
  causes. `src/cluster_service.rs`, `Entry::Snapshot` on `_SNAPSHOT`.
- `tests/conformance.rs` holds the header to the vectors: 11 opaque types, 13
  structs, 96 functions, 103 defines. `clients/generate-test.py`: 76 tests. The five
  packages regenerated, and `just lint-clients` finds them what the header gives.
  Two generators met a parameter they had no form for, `const uint8_t **`: .NET's
  binds it as `out byte*`, and Node's addon returns the bytes and their count as one
  `Buffer` that copies them, since the library frees them at a later call.
- An agent's review of the change before it was pushed found the Node addon handing
  the state back as a pointer that JavaScript cannot read, which the copy above
  answers, and that a second restore clears the caller's pointer while the handle
  still holds the state, which the page now says.

## Consequences

- A program in any language with a binding of version 6 can take a snapshot and
  start from one. The five clients' objects are the next changes of work item 335,
  one client each, as each client's session and service were in work item 229, and
  each holds its snapshot in its runner's cluster round.
- A program of version 5 is refused at its first call, as every version's is; once
  rebuilt, its handler is handed the request as 6 where it was 5. A handler that
  matched the four kinds it uses passes over this one too.
- Beside a service written through the ABI that calls `restore`, a member's
  `cluster.log_purge_position` holds as it does beside a Rust one (ADR 0132).
- A program's copy of a restored state is its own; the handle's is gone at the
  next poll. A program that reads the pointer after its first poll reads freed
  memory, the poison of a debug build there. The first poll after a start frees
  the bytes before it reads an entry: one free in a service's life, of up to 1 GiB,
  and no work for any message.
- Revisit: when a program's state does not fit one slice, with ADR 0131's writer in
  pieces, for a call that gives the state in pieces and a start that hands them back
  in pieces; when a program needs the cause of a failed snapshot as a value.

## References

- Work item 335 on the Brigade Board, "Take a service's snapshot through the C ABI
  and the five clients"; work item 366, feature 6 of ADR 0138, whose first part it
  is; work item 230, which built the Rust side.
- `spec/c-abi.md`: "Functions", cluster service; "Result codes"; "Error codes";
  "Constants"; "When a function cannot answer"; "Calls during a claim or a poll";
  "Lifetimes"; "Threads"; "A service's snapshot and its start"; "Versions and what a
  reader rejects"; "Out of this version". `spec/vectors/c-abi.py`.
- `crates/cavalry-ffi/src/cluster_service.rs`, `src/error.rs`, `src/codes.rs`,
  `tests/cluster_snapshot.rs`; `clients/generate_dotnet.py`, `clients/generate_node.py`.
- [ADR 0131](0131-have-a-service-write-its-own-snapshot-and-its-member-list-it.md)
  and
  [ADR 0132](0132-start-a-service-from-the-snapshot-its-member-names-when-its-program-asks.md),
  the Rust service's snapshot and its start;
  [ADR 0123](0123-take-the-c-abi-to-version-5-with-the-cluster-service-answered-from-inside-the-one-fragment-handler.md),
  the cluster service in the ABI;
  [ADR 0070](0070-take-the-c-abi-to-version-2-with-the-committed-log.md), the one
  fragment handler.
