# 0131. Have a service write its own snapshot, and its member list it

- **Status:** Accepted
- **Date:** 2026-11-15
- **Invariants affected:** 1, 3, 4, 5, 6
- **Supersedes:** none. Decides the fourth change of work item 230. Takes up what
  ADR 0103 left to the snapshot's change, the channel by which a service tells its
  member of a snapshot, and ADR 0129's "a service takes none". Changes
  `spec/cluster.md`: two paragraphs of "Snapshots", "A service's snapshot" and "The
  member's list of a service's snapshots"; two templates, client session (102) and
  service ack (33); a row of "Members and streams", the consensus module stream;
  the marker's rows; five rows of "Counters", which `counters.toml` gains as ids 167
  to 171; three rows of "Values"; three of "What a reader rejects"; a sentence of
  "The service"; and "Out of this version". A sentence of `spec/durable-log.md`,
  "The driver". The page's version stays 0, as ADR 0129 left it: Cavalry has made
  no release, and no member of another version reads the page. No file's format
  changes and no function of the C ABI.

## Context

A member's consensus module takes a snapshot of its sessions at the leader's
cluster action, lists it, and starts from it (ADR 0128, ADR 0129, ADR 0130). The
service beside it holds the rest of the cluster's state, a program's own, and has
no snapshot: a service reads its member's log from position 0 at every start, and
while one does, no recording of that log can be purged (ADR 0130, Consequences).

A service is a client of its member's driver, in a process of its own or in the
member's, and its member does not know it: a member does not know how many
services read its log, or whether one runs (ADR 0066). A service reads the log
through `ClusterService::poll`, whose handler is the program (ADR 0118). The
program's state is the program's own, and only the program can turn it into bytes.

Three questions follow. How a program is asked for its state at one position, the
same on every member. Where the bytes go. And how a member that knows no service
comes to list a service's snapshot.

## Decision

The action is a new kind of entry for the program, `Entry::Snapshot`, and the
service's poll ends after it, so the program's state is then its state at the
snapshot's position, the position after the action's entry. The program may give
that state to `ClusterService::snapshot(client, state)` before its next poll. A
program that gives none reads on, and no snapshot is taken for that action.

`snapshot` is one call that waits for the driver. The service keeps one exclusive
publication on `cavalry:ipc` and the service snapshot stream, 106, from its first
snapshot. For each snapshot it starts a recording of that publication, and writes
a marker of type id 2, a client session (Aeron's template 102) for each open
session by rising id, a section marker, the program's bytes, 1,024 to a message,
and an end marker. It stops the recording when the recorder has written to the
end, and checks that it stopped there. Then it sends its member a service ack
(Aeron's template 33) on `cavalry:ipc` and the consensus module stream, 105, with
the position, the action's time and the recording's id. Each step is bounded by
the snapshot timeout, 1 s without progress, and a state is at most 1 GiB.

A member subscribes to the consensus module stream from its start and reads at
most 10 messages of it in a duty cycle. It lists a service's snapshot for an ack
when the service id is 0, its catalog holds the recording as a stopped recording
of stream 106 with bytes, the position is above 0 and at or below its commit
position, and its snapshots file takes the entry. It answers no ack and waits for
none. One service beside a member takes snapshots, with service id 0.

The C ABI hands a client the action as the kind `other`, as before. Its own call
for a snapshot, and the five clients, are work item 335.

## Alternatives considered

- **A service made "with snapshots", whose polls read nothing after the action
  until the program has given its state.** Lost. A program that forgets the call
  stalls for ever with no error, and every program written before this change
  would need the flag. With no flag and no stall, a program that knows nothing of
  snapshots is handed an entry it does not match, as it was handed `Other`, and
  reads on.
- **The state given from inside the handler.** Lost. The handler's signature is
  the five clients' callback (ADR 0124), and a handler that writes a recording
  holds the poll, and the service's sessions, for as long as the driver takes.
- **A snapshot stepped by the program's polls, as the consensus module's is by its
  duty cycle (ADR 0129).** Lost. The module's cycle must not wait: it reads
  consensus messages on the same thread. A service's thread is its program's, the
  state must not change while it is written, and every command of the client
  waits for the driver already. One call is one thing to explain, and it measures
  31 ms.
- **A writer that takes the state in pieces.** Lost for now. One slice is what a
  program with a state of some megabytes has. The Revisit names a state that does
  not fit memory twice.
- **The program's bytes with no marker before them, or each piece in a message of
  Cavalry's own.** Lost. With no marker a piece can read as a client session. A
  wrapper is a template Aeron does not have. Aeron's own section mark, which no
  snapshot used, says where the page's messages end: after it every message but
  the last is the program's, by its place.
- **The leader of the snapshot's term in the snapshot.** Lost. A service answers an
  entry when the term's event named its member and the member's counters say that
  it leads the term still (ADR 0118). The second implies the first, since one
  member leads a term. So a service that starts at a snapshot's term can take its
  member's counters alone, and the snapshot needs no message that Aeron's lacks.
  The start is the next change.
- **The member waits for its services before its own snapshot counts, as Aeron's
  consensus module does.** Lost. It needs the count of services, which a member
  does not have and no key gives it (ADR 0066, invariant 5), and it makes a
  member's snapshot fail for a service that is stopped.
- **The member answers the ack.** Lost. An answer is a second stream, a wait in
  the service and a timeout, for a case the next action repairs: an ack that the
  member did not read is a snapshot that is in no list.
- **The member reads the recording before it lists it.** Lost. The bytes after
  the section marker are the program's, which a member cannot check, and the
  markers say only what the ack says. The service checks them when it starts
  from the snapshot, as a member checks its own (ADR 0130).
- **A service id for each service.** Lost for now. One program is a cluster's
  service in this version: two services beside one member would both reply to a
  session (ADR 0118). The field is there, in the ack and in the file.
- **The C ABI's call and the clients in this change.** Lost. It is an ABI version
  and five clients, each of which was a change of its own in work item 229. The
  Rust API comes first, as it did there. It is work item 335.

## Evidence

- `crates/cavalry-raft/tests/snapshot.rs`, three members over three drivers on the
  loopback with a service beside each, 10 of 10 runs of the file at a load average
  of 5 to 14. With two clients' sessions open and a state of 1,048 bytes, every
  service gives its state once, at the position its member's own snapshot is of.
  Every member lists both, with one term, base and time. The three services'
  recordings hold the same seven messages: a marker of type id 2, the two client
  sessions by their ids with each client's channel, the section marker, the state
  in two messages, 1,024 bytes and 24, and the end marker. A service that starts
  again reads the action again, finds that its member lists a snapshot of that
  position, and writes none. A call where no snapshot is due is refused.
- The same file, the member's four checks. Seven acks sent by hand, each differing
  in one field from an ack the member lists, are each counted in
  `cluster.service_acks_rejected` with nothing listed: another service id, a
  recording the catalog does not hold, a recording of the snapshot stream, a
  position above the commit position, position 0, the position already listed,
  and a position that is no entry's end. A session keepalive on the stream is
  counted in `cluster.messages_rejected` and is no ack. The ack that differs in
  nothing is listed.
- The same file, a service whose member has stopped while its driver runs. A
  state one byte over 1 GiB is refused before anything is written. The service
  then writes its recording, and gives the snapshot up at the ack after the
  snapshot timeout, counted in `cluster.service_snapshots_failed`, with an error
  that says to check that the member runs.
- The 258 vectors of `spec/vectors/cluster.json`, 15 of them new, agree with the
  Rust reader, and every valid one encodes back to its bytes.
- How long `snapshot` takes, timed in a release build on an Apple M5 Pro laptop,
  macOS 26.6.1, by the ignored test `a_services_snapshot_timed`, 10 snapshots of
  each state at a load average of 7. No bytes: p50 31.1 ms, max 74.2 ms, the first
  of them with the service's two publications and two counters added. 1 KiB: p50
  31.1 ms, max 35.8 ms. 1 MiB: p50 33.0 ms, max 34.5 ms. 64 MiB: p50 51.4 ms, max
  60.1 ms. No Linux host has timed it, and a sync there is not `F_FULLFSYNC`.
- A fault met while this was built. With the members' duty cycles and three
  services' calls on one thread, a member gave its own snapshot up at the
  snapshot timeout, having waited behind the services. The test now has each
  service give its state once the members have listed theirs. Outside a test a
  service's call is on its program's thread and holds no member.
- `cavalry-control`'s tests hold `cavalry doctor` to a warning for
  `cluster.service_acks_rejected` and for `cluster.service_snapshots_failed`
  above 0, and to none at 0.

## Consequences

- A program that takes snapshots makes one more match in its handler and one call
  after its poll. The crate's example shows both.
- A service's snapshot holds no leader's id, and the service that starts from one
  decides whom it answers from its member's counters alone.
- A member lists what a service tells it. A program that gives another state than
  the one its entries made lists a snapshot that is wrong, and no member can tell.
  That is the program's bug, as a handler that reads a clock is.
- A second service beside a member that takes snapshots has its recording or its
  ack refused, and is counted. It is never listed for the first.
- A service still reads its member's log from position 0, and the log below a
  snapshot is still purged only where no service reads it from its start (ADR
  0130). The start from a service's snapshot is the next change of work item 230,
  with the lowest position a member's log must reach back to.
- A service written in Python, Node, Go, the JVM or .NET takes no snapshot until
  work item 335.
- Revisit: when a program's state does not fit one slice, for a writer in pieces;
  when two programs are services of one cluster, for the service id; when a
  service's snapshot must reach another host, for a standby or a member that lost
  its directory.

## References

- Work item 230 on the Brigade Board, "Catch a member up from a peer's recording,
  snapshot the service and follow as a standby"; work item 335, "Take a service's
  snapshot through the C ABI and the five clients".
- `spec/cluster.md`: "Snapshots", "The service", "Members and streams",
  "Counters", "Values", "What a reader rejects", "Out of this version";
  `spec/durable-log.md`, "The driver"; `spec/vectors/cluster.py`.
- `crates/cavalry-client/src/service.rs` (`ClusterService::snapshot`,
  `snapshot_due`, `Entry::Snapshot`), `src/cluster.rs` (`ClusterError`,
  `SnapshotCause`); `crates/cavalry-raft/src/snapshot.rs` (`read_acks`,
  `list_service`), `src/module.rs` (`work_service_acks`), `tests/snapshot.rs`;
  `crates/cavalry-proto/src/cluster.rs`; `counters.toml`;
  `docs/how-to/run-a-cluster.md`.
- [ADR 0128](0128-list-a-members-snapshots-in-a-file-of-their-own-in-its-log-directory.md),
  [ADR 0129](0129-snapshot-a-members-sessions-when-its-read-of-its-log-reaches-the-leaders-action.md)
  and
  [ADR 0130](0130-start-a-member-from-its-newest-snapshot-and-hold-its-log-to-that-position-alone.md),
  the snapshots file, the member's own snapshot and its start;
  [ADR 0066](0066-feed-the-committed-log-to-a-service-through-a-replay-of-the-members-recording.md),
  a service as a client that its member does not know;
  [ADR 0103](0103-list-the-recordings-of-a-members-log-in-its-driver-directory-for-a-service-to-read-in-order.md),
  what a member tells its service by a file;
  [ADR 0118](0118-reply-to-a-session-from-the-service-beside-the-entrys-leader-and-wait-for-no-client-past-the-keepalive-interval.md),
  the service's handler and whom it answers;
  [ADR 0124](0124-give-each-client-a-session-object-and-a-service-object-whose-handlers-failure-aborts-its-entry.md),
  the five clients' service.
