# 0132. Start a service from the snapshot its member names, when its program asks

- **Status:** Accepted
- **Date:** 2026-11-15
- **Invariants affected:** 1, 3, 4, 5, 6
- **Supersedes:** none. Decides the fifth change of work item 230. Answers ADR
  0130's Revisit on the lowest position a member's log must reach back to, and
  ADR 0118's on a service that starts from a snapshot and the sessions a snapshot
  carries. Changes
  `spec/cluster.md`: a paragraph of "Snapshots", "A service's start"; a paragraph
  of "A member's start", on the position a member's log is needed from; a sentence
  each of "Snapshots", "The service" and "Out of this version"; a row of "Members
  and streams", the service snapshot replay stream, 109; a row of "Values" and the
  stream ids' row; and four rows of "Counters", which `counters.toml` gains as ids
  172 to 175. The page's version stays 0. No message's layout changes, no file's
  format and no function of the C ABI.

## Context

A service writes a snapshot of its program's state at the leader's cluster
action, and its member lists it (ADR 0131). Nothing reads it back: a service
reads its member's log from position 0 at every start, so a start takes as long
as the log is long, and no recording of a member's log can be purged while a
service beside it may start again (ADR 0130, Consequences).

A service is made with no argument and opens nothing until its first poll (ADR
0118). It knows its member by the member's counters and by a file the member
writes in the driver directory (ADR 0066, ADR 0103). It does not read the
member's log directory, where the snapshots file is. And a service written
through the C ABI has no call that takes a snapshot or a state (work item 335).

The questions: how a service learns which snapshot to start from; who decides
that it starts from one; what it does with one it cannot read; whom it answers
after; and from where a member's log is then needed.

## Decision

A program asks its service to start from a snapshot: `ClusterService::restore`,
before the first poll. A program that does not ask reads the log from position 0,
as before.

The member names one recording: the gauge `cluster.service_snapshot_recording_id`
holds the recording of the newest snapshot of a service that it lists, −1 for
none. The service loads it, of a member whose live recording it has found, and
plays that recording back whole on `cavalry:ipc` and a stream of its own, the
service snapshot replay stream, 109, which nothing subscribes to but a service
reading its snapshot. It
takes the snapshot when the messages are a service's snapshot, a begin marker of
type id 2 at a position above 0, the client sessions by rising id, the section
marker, the program's bytes, and the end marker as the last message; when no more
than 5 s passes before the first message and between two; and when its table
takes the sessions. It then holds those sessions, reads the log from the
snapshot's position, and gives the program its bytes: `Restored::State`.

A snapshot that fails is passed over, counted in
`cluster.service_snapshots_passed_over`, and the service reads the log from
position 0: `Restored::FromStart`, which is also the answer when the member lists
none. With no member that has a log yet the answer is `Restored::NoMember`, and
the program asks again.

The driver's refusal of the replay fails the snapshot only when it is a refusal
for the recording, which a later start meets again: error code 21, a recording the
driver does not hold as a valid one, or code 23, no bytes of it where the replay
starts. Every other refusal of the start's commands is the call's error, with the
driver's code: a registry, the counters file or a queue at its bound, code 10; a
retention command at work on the recording, code 24; code 12, a file that cannot
be read or created; and a refusal of the add of the start's two counters or of its
subscription. The service has not started and has counted nothing, and the
program asks again.

A service that started from a snapshot takes the snapshot's term as the term it
is in, without that term's leader. For an entry of that term it answers when its
member's counters say that the member leads the term.

A member's log is needed from the lower of two positions, those of the newest
snapshot of its consensus module and of the newest snapshot of a service that it
lists, and from 0 while it lists none of either. The gauge
`cluster.log_purge_position` holds it.

## Alternatives considered

- **A service starts from its snapshot by itself.** Lost. A service through the C
  ABI cannot be given a state (work item 335), and a Rust program written before
  this change does not expect one. Either would begin at the snapshot's position
  with an empty state and no error. A program that takes snapshots is the one
  that knows it must be given its state back, and it says so with one call.
- **The state handed to the handler as a first entry.** Lost. It is the same fault
  with a later symptom: a handler that does not match the new entry skips its
  state. It also puts megabytes through a signature made for one frame.
- **A file in the driver directory that lists every snapshot of a service, as the
  member lists its log's recordings (ADR 0103).** Lost. It is a file format, with
  its checks and vectors, for one use: to pass over the newest snapshot for an
  older one. A log purged below the newest snapshot does not reach back to an
  older one, which is what ADR 0130 found for the member's own. One counter names
  one recording, and the recording's own markers say its position and term.
- **Two counters, the recording and its position.** Lost. Two loads are not one
  read, and the marker holds the position.
- **The snapshot played back on the service log stream, where a service reads its
  member's log.** Lost, after it was built. A replay moves no faster than its
  slowest subscriber allows, and every service's subscription on that stream is a
  subscriber of every replay there. A service that is not polling, in its
  program's own work or in a snapshot, holds them all at its position. A start
  from a snapshot of 64 MiB beside such a service read what the publication's
  window let through, then nothing for 5 s, and passed its snapshot over; states
  up to 1 MiB fitted the window and were read (Evidence). On a stream that is
  subscribed to only while a snapshot is read, nothing stands still.
- **The service reads the member's snapshots file.** Lost. The file is in the log
  directory, which a client of the driver is not given (ADR 0066).
- **Pass the snapshot over at every refusal of its replay.** Lost, after it was
  built; the review of pull request 303 found it. The driver also refuses a replay
  for a registry at its bound, code 10, for a retention command at work on the
  recording, code 24, and for a replay's log it cannot create under shm pressure,
  code 12, and each of those passes. A start that passed its snapshot over for one
  of them could not take it again: it read the whole log for a condition of a few
  milliseconds, and beside a log purged at `cluster.log_purge_position` its poll
  was refused its replay and the service did not run. That is back-pressure
  absorbed, against invariant 3. Code 12 is also the code of a segment file that
  cannot be read, which does not pass, and one code names both: a program refused
  it at every start reads its driver's message, which names the file, and may poll
  without the call, which reads from position 0.
- **Refuse the service's start when its snapshot cannot be read.** Lost, for ADR
  0130's reason: the log may be whole, and then position 0 gives the same state
  after a longer read. Where the log is not whole, the replay's refusal says so,
  as it did before this change.
- **The leader of the snapshot's term in the snapshot.** Lost in ADR 0131, and this
  is the change that relies on it. A service answers an entry when the term's
  event named its member and the member's counters say that it leads the term
  still. One member leads a term, so the second holds only on the member the
  event names. A test holds a service that started from a snapshot to it: the one
  beside the leader answers the term's next entry, and the one beside a follower
  does not.
- **A purge position of the member's own snapshot alone.** Lost. It is what ADR
  0130 left an operator to work out, with "only where no service reads from the
  start". The lower of the two is the rule, and a counter can hold it.
- **`cavalry doctor` says which recordings to purge.** Lost for now. No command of
  `cavalry` purges a member's log, and a finding with no command to run is a page.
  The Revisit names it.

## Evidence

- `crates/cavalry-raft/tests/snapshot.rs`, three members over three drivers on the
  loopback with a service beside each. A service that stops and starts again is
  given the state its program gave at the snapshot, holds the two sessions and
  the snapshot's position before it has read an entry, reads the two entries
  that follow and none below, and holds what the others hold. A second call is
  refused. The leader's service started from its snapshot answers none of the
  entries its member held when it opened the log, and answers the next entry its
  member appends; the one beside a follower answers neither. At the next snapshot
  the two services that started from one and the one that never stopped write
  the same bytes. Every member's `cluster.log_purge_position` is the snapshots'
  position, and its `cluster.service_snapshot_recording_id` the recording it
  lists.
- The same file, after a purge: a follower of two terms and its service stop, the
  first recording of the member's log, which stopped below both snapshots, is
  purged, and both start again, the member from its snapshot and the service from
  its own. Every service then holds the six entries, and the next snapshot's
  bytes are the same beside the three members. This is the work item's "a member
  restarted after a purge rejoins from a snapshot and the replay after it, with
  nothing committed lost", with the service's state among what is not lost.
- The same file, a snapshot passed over: a service whose member lists none starts
  from position 0 and adds no counter; with its snapshot's recording purged it is
  told of that snapshot, cannot play it back, counts one passed over, reads the
  log from position 0 and holds what the others hold.
- The same file, a driver with no room for the replay: another client holds the
  64 replays of a follower's driver, each following a live recording, and a
  service's start beside that member is refused twice with code 10. It has not
  started, its position is 0, it counted nothing, and the driver holds no counter
  of its start once it has taken the removes. Once one of the other client's
  replays stops, the next call is given the state at the snapshot and the service
  reads on to what the others hold. With every refusal passing the snapshot over,
  as first built, the same test found `Restored::FromStart` at the first call.
- `crates/cavalry-client/src/service.rs`, three tests of the reader with no
  driver: a snapshot as a service writes it is read back, a state of 2,500 bytes
  in three messages; a state whose bytes are themselves a client session, or the
  end marker itself, is the program's by its place; and a recording is no
  service's snapshot when it is cut short at any message, has a message in
  fragments, the consensus module's type id, position 0, an end marker of another
  position, no section marker or one with index 0, a session twice or out of
  order, eleven sessions, or a consensus module's message among them.
- The file's twelve tests on the laptop, beside another project's tests: 10 of 10
  runs before the snapshot's replay had a stream of its own, and 12 of 14 at this
  tree. One of the two failed because a member was refused a port that another
  process had bound, `Address already in use`, which the tests' rule for ports
  names as a case it does not cover. The other did not end, its message lost, its
  thread waiting in a driver's drop: work item 338.
- How long `restore` takes, timed in a release build on an Apple M5 Pro laptop,
  macOS 26.6.1, by the ignored test `a_services_start_from_its_snapshot_timed`,
  ten starts from a snapshot of each state at a load average of 5, beside a
  service that was not polling. No bytes: p50 60.4 ms, max 90.8 ms. 1 KiB: p50
  63.4 ms, max 89.3 ms. 1 MiB: p50 54.9 ms, max 65.3 ms. 64 MiB: p50 58.9 ms, max
  76.6 ms. With the replay on the service log stream, before: 47.9 ms, 47.6 ms
  and 50.4 ms at the median for the first three, and for 64 MiB no state: the
  start read nothing for 5 s and passed the snapshot over, after 5.08 s. No Linux
  host has timed it.
- `cavalry-control`'s test holds `cavalry doctor` to a warning for
  `cluster.service_snapshots_passed_over` above 0 and to none at 0.

## Consequences

- A program that takes snapshots makes one call before its first poll, and reads
  the log written since its last snapshot.
- A service's start costs the replay of its snapshot and no read of the log below
  it. A member's log below the lower of the two snapshots may be purged.
- Beside a member whose service takes no snapshot, or is written through the C
  ABI, `cluster.log_purge_position` stays 0 and nothing is purged. That is every
  cluster whose service is not Rust until work item 335.
- A service is told of its member's newest snapshot alone. One that cannot take
  it reads from position 0, and beside a log purged below that snapshot it is
  refused its replay and does not run until its member has the log again, which
  no command of this version gives it (work item 286).
- A program that calls `restore` after a poll is refused, and a program that
  never calls it loses nothing: it reads the whole log.
- Revisit: when `cavalry` has a command that purges a member's log, for a `doctor`
  finding that names the recordings; when a service must pass over its newest
  snapshot for an older one; when a service starts from a snapshot of another
  host's, for a standby or a member that lost its directory.

## References

- Work item 230 on the Brigade Board, "Catch a member up from a peer's recording,
  snapshot the service and follow as a standby"; work item 335, the C ABI and the
  five clients; work item 286, a member that lost its log directory.
- `spec/cluster.md`: "Snapshots", "A member's start", "The service", "Counters",
  "Values", "Out of this version".
- `crates/cavalry-client/src/service.rs` (`ClusterService::restore`, `Restored`,
  `SnapshotReader`, `read_snapshot`), `src/committed_log.rs`
  (`member_with_a_log`), `src/cluster.rs`; `crates/cavalry-raft/src/snapshot.rs`
  (`purge_position`), `src/module.rs` (`log_purge_position`), `tests/snapshot.rs`;
  `counters.toml`; `docs/how-to/run-a-cluster.md`.
- [ADR 0130](0130-start-a-member-from-its-newest-snapshot-and-hold-its-log-to-that-position-alone.md)
  and
  [ADR 0131](0131-have-a-service-write-its-own-snapshot-and-its-member-list-it.md),
  a member's start from its snapshot and a service's snapshot;
  [ADR 0066](0066-feed-the-committed-log-to-a-service-through-a-replay-of-the-members-recording.md)
  and
  [ADR 0103](0103-list-the-recordings-of-a-members-log-in-its-driver-directory-for-a-service-to-read-in-order.md),
  what a service knows of its member;
  [ADR 0118](0118-reply-to-a-session-from-the-service-beside-the-entrys-leader-and-wait-for-no-client-past-the-keepalive-interval.md),
  whom a service answers.
