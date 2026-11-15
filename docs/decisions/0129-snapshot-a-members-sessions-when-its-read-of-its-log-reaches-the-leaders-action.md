# 0129. Snapshot a member's sessions when its read of its log reaches the leader's action

- **Status:** Accepted
- **Date:** 2026-11-15
- **Invariants affected:** 1, 2, 3, 4, 5
- **Supersedes:** none. Decides the second change of work item 230. Answers ADR
  0052's question of the snapshot cadence, ADR 0115's Revisit on the sessions a
  snapshot must hold, and, for the consensus module's own state, ADR 0058's Revisit
  on a snapshot's contents between its markers. Changes `spec/cluster.md`: the
  section "The snapshot marker" becomes "Snapshots", with what asks for a snapshot,
  what a member writes and what a failure does, and two templates, cluster session
  (103) and consensus module (105); the cluster action's row of "The log"; a
  sentence of "A member's start"; six rows of "Counters", which `counters.toml`
  gains as ids 159 to 164 with the state machine `cluster_snapshot`; four rows of
  "Values"; three rows of "What a reader rejects"; "Out of this version". Two
  sentences of `spec/durable-log.md` name the new section. The page stays at
  version 0, and no function of the C ABI changes.

## Context

ADR 0128 gave a member a file that lists its snapshots. Nothing writes one. A
snapshot is the member's state at one position of its log, and for the consensus
module that state is its table of sessions: each open session's id, the
correlation id of its connect and its response channel, and the id the next
session is given. Every member builds that table from the committed session open
and close events of its own log, which its module reads through a replay on the
log replay stream (ADR 0115, ADR 0116). Everything else a session holds is a
leader's own, and begins again when a leader begins to serve.

Four facts constrain how a member can take one.

- The module's thread is the thread that appends and commits. A command to the
  driver is synchronous on it: the client waits for the driver's answer.
- The module reads its log with a poll that hands over up to 64 entries. A
  snapshot must be of the table as one entry left it.
- Every member must take the snapshot at the same position, with no member
  telling another: the members' logs are the same bytes, so the position must come
  from the log.
- A member does not know which services read its log (ADR 0066), so it cannot
  wait for them.

Aeron takes a snapshot when an operator asks: a control toggle of the consensus
module, `ClusterControl.ToggleState.SNAPSHOT`. The leader then appends a cluster
action to the log, and each member's consensus module writes its state, on stream
107 and the channel `aeron:ipc?alias=snapshot`, to a recording: a marker, a
`ClusterSession` for each session, its timers, a `ConsensusModule` and a marker.
It writes a session's `timeOfLastActivity` as its null value and an open session's
`closeReason` as the enumeration's null. A service's snapshot is on stream 106,
with marker type id 2 where the module's is 1 (read on 2026-11-15 at `master`,
1.55.0-SNAPSHOT, schema version 18).

## Decision

The leader asks for a snapshot by appending a cluster action whose action is
snapshot. It appends one by itself when its log has grown by the snapshot
interval, 256 MiB, since the last such action, and when its own process asks,
`ConsensusModule::snapshot()`. Every member takes the snapshot when its consensus
module's read of its own committed log reaches the action. The read is a
controlled poll and ends at the action's entry, so the table is then what every
entry below the position after the action made it, on every member alike. That
position is the snapshot's log position. A member holds one exclusive publication
on the snapshot stream, 107 on `cavalry:ipc`, from its start. For a snapshot it
starts a recording of that publication, writes a marker, a cluster session for
each open session in the order of the sessions' ids, a consensus module and a
marker, stops the recording once the recorder has written them, and lists the
snapshot in its snapshots file when the driver has announced a stop at the end of
what it wrote. Each step is one duty cycle's work, and none waits. While the
messages are written the module does not read its log. A step that fails ends
the snapshot, counted in `cluster.snapshots_failed`, and the member goes on. The
two templates are Aeron's, field for field. A member still starts by reading its
log from position 0: the start from a snapshot is the next change.

## Alternatives considered

- **A publication for each snapshot, as Aeron adds one.** Lost on a measurement.
  Adding an exclusive publication took the driver 110 ms on the laptop, and a
  member's duty cycle stopped for 183 ms at the median while a snapshot was
  taken. With one publication kept for the member's life the same cycle is 20
  ms. Each snapshot is a recording of a span of that publication's stream, which
  a replay plays from the recording's own start.
- **Take the snapshot on a thread of its own.** Lost for now. It needs a second
  client of the driver, a hand-off of the table between two threads, and a second
  writer or reader of the snapshots file. The pause it would remove is 20 ms once
  in 256 MiB of log. The Revisit names when it returns.
- **Read the log with the plain poll, copy the table at the action and write
  after the poll.** Lost. The client has a controlled poll that ends at an entry
  (ADR 0116 uses it for the ingress stream), so the table itself is the state
  when the poll returns, and nothing is copied.
- **Only when an operator asks, as Aeron does.** Lost. A member's start must be
  bounded with nothing configured and nobody asking (invariant 5). The leader's
  own process may ask as well, which is what a test and an embedding program use.
  A command for an operator is not built: the Revisit names it.
- **An interval of time.** Lost. An interval of the log's bytes bounds what a
  start reads, which is the reason for a snapshot, and it is a function of the
  log alone, so a new leader carries it on from what it read.
- **An interval of 1 GiB, or of 64 MiB.** A member that started again read its
  log back at 185 MiB/s and 191 MiB/s. At 256 MiB an interval is about 1.4 s of
  that read. At 1 GiB it is over 5 s, during which a member that won an election
  does not serve (ADR 0116). At 64 MiB the 20 ms pause comes four times as often
  for a third of a second saved.
- **The marker with the term base log position, which Aeron's schema version 18
  adds.** Lost for now. It is a new block length of a template that readers of
  version 0 hold to 40, and the snapshots file carries the term's base. Revisit
  when a snapshot must be listed from its recording alone.
- **The sessions in the order of the table's slots.** Lost. A leader gives a slot
  at a connect and a follower at a commit, so two members hold one session in
  different slots, and their snapshots would differ. In the order of the ids the
  three members' recordings hold the same bytes, which a test holds them to.
- **The cluster's time as a session's time of last activity.** Lost. A member
  counts a session's timeout on its own monotonic clock from when its leader
  begins to serve. Aeron writes its null there, and so does this.
- **A snapshot's position as the position of the action's entry.** Lost. The
  position after the entry is where a start reads on from, and is what Aeron's
  log adapter hands as the action's position.
- **A failed snapshot as a fault of the member.** Lost. The member's log is whole
  and nothing committed depends on a snapshot. It is counted, `cavalry doctor`
  reports it, and the next action asks again. A list that cannot be written is
  the member's duty cycle's error, as the list of its log's recordings is.
- **Wait for the recorder, or for the stop, inside the cycle.** Lost. The module
  waits for no announcement of the driver's inside a duty cycle: it asks a
  recording's stop in one cycle and reads it in a later one, as it does for its
  recordings of the log (`crates/cavalry-raft/src/module.rs`, `stop_recording`).
  The snapshot's stop is asked in the cycle that finds the recorder at the end,
  and read in a later one, within the snapshot timeout of 1 s.

## Evidence

- `crates/cavalry-raft/tests/snapshot.rs`, three members over three drivers on
  the loopback, 10 runs of 10. Every member lists one snapshot at the position
  after the action, with the action's term and time; the three recordings replay
  to the same bytes, a marker, a consensus module and a marker; a second action
  lists a second snapshot; a follower's ask is refused; each member's six
  counters hold what it did. With two clients' sessions open, the recordings hold
  the two cluster sessions in the order of their ids, each with its client's
  channel and the position of its session open event, and the next session id 2,
  the same bytes on the three members. A member that stops and starts again
  reads the action in its log and takes no second snapshot of it.
- `crates/cavalry-raft/src/module/tests.rs`: a leader appends no action while its
  log is 32 bytes short of the interval since the last, and one at the interval,
  once; every member notes where that action ended.
- `crates/cavalry-raft/tests/sessions.rs`: the table gives its open sessions in
  the order of their ids, whatever slot each has, with where each opened.
- `spec/vectors/cluster.json`, 243 vectors, 11 of them new: the two templates,
  the longest response channel, a session's ignored fields with other values, and
  the rejections, for the template of a service's snapshot, the stream, the block
  length, the opened log position, a byte that is not ASCII before that position,
  and bytes after the last field. The Rust reader agrees with each.
- What a snapshot costs the member's thread, timed in a release build on an Apple
  M5 Pro laptop, macOS 26.6.1, by the ignored test `a_snapshots_duty_cycles_timed`
  over 50 snapshots of three members. The longest duty cycle of a member while a
  snapshot was taken: p50 19.71 ms, p90 21.08 ms, max 25.44 ms. From the ask
  until the three members had listed it: p50 119.98 ms, p90 127.12 ms, max 144.15
  ms. The longest cycle with no snapshot in hand: 0.03 ms. With a publication
  added for each snapshot, before the decision above: p50 183.26 ms, p90 212.73
  ms, max 228.47 ms, and 760.85 ms at the median until the three had listed it.
  The driver's commands took, at the mean: add exclusive publication 110 ms,
  start recording 102 ms, stop recording 40 ms, and the list's write 14 ms. On
  macOS a sync is `F_FULLFSYNC`. No Linux host has timed it.
- How fast a member reads its log back, timed in a release build on the same
  laptop by the ignored test `a_members_read_of_its_log_at_a_start_timed`: three
  members committed 64 MiB of entries of 1,408 bytes, a follower stopped and
  started again on the log it kept, and its consensus module read the 64 MiB in
  0.335 s and 0.345 s once it knew the commit position, 191 MiB/s and 185 MiB/s,
  from the page cache. It knew the commit position 0.18 s and 0.44 s after its
  start.
- `just lint-counters`: 164 counters and 11 state machines, each transition of
  `cluster_snapshot` with a counter that the code moves.
  `crates/cavalry-control`'s test holds `cavalry doctor` to a finding for
  `cluster.snapshots_failed` above 0 and to none at 0.

## Consequences

- A member has snapshots of its sessions, listed where its start can find them.
  It does not start from one yet, and its log can still not be made shorter.
- Every member takes the snapshot when it reads the action, which is at about the
  same time on the three. So the cluster's commits stop for about one duty cycle
  of a snapshot, 20 ms on the laptop, once in 256 MiB of log. Revisit when that
  pause shows in a cluster's commit latency: the snapshot on a thread of its own.
- A member's driver holds one more publication, and its catalog gains a recording
  for each snapshot. The snapshots file forgets the oldest of 64, and nothing
  purges a recording (ADR 0128's Revisit).
- A service takes no snapshot yet. A service's snapshot, its stream, its marker's
  type id, the service ack and the file that tells a service where to start are
  the change after the member's start, with a decision of their own.
- The cluster actions suspend and resume stay out of this version.
- Found while testing, and not this decision's: a cluster of one member over a
  driver never closes its election, since its log publication has no member to
  connect it. The test of the interval runs three members for that reason.
- Revisit: when an operator must have a snapshot now, for a command; when a
  cluster's services are slow to read back, for an interval that counts their
  read and not the module's alone; when a Linux host has timed a snapshot; when
  a snapshot must be listed from its recording alone, for the marker's term base.

## References

- Work item 230 on the Brigade Board, "Catch a member up from a peer's recording,
  snapshot the service and follow as a standby".
- `spec/cluster.md`: "The log", "Snapshots", "A member's start", "Counters",
  "Values", "What a reader rejects", "Out of this version";
  `spec/vectors/cluster.py` and `.json`; `spec/durable-log.md`, "Snapshots".
- `crates/cavalry-raft/src/snapshot.rs`, `src/module.rs`, `src/sessions.rs`,
  `src/consensus.rs`, `tests/snapshot.rs`; `crates/cavalry-proto/src/cluster.rs`;
  `counters.toml`; `crates/cavalry-control/src/doctor.rs`.
- [ADR 0128](0128-list-a-members-snapshots-in-a-file-of-their-own-in-its-log-directory.md),
  the snapshots file; [ADR 0052](0052-build-raft-on-the-durable-log-fifth.md), the
  slice; [ADR 0058](0058-specify-the-cluster-protocol-from-aerons-codecs.md), the
  protocol from Aeron's codecs;
  [ADR 0115](0115-keep-a-clusters-sessions-on-every-member-opened-and-closed-by-the-log.md)
  and
  [ADR 0116](0116-read-a-members-sessions-from-its-committed-log-and-serve-them-from-its-terms-base.md),
  a member's sessions and its read of its log;
  [ADR 0066](0066-feed-the-committed-log-to-a-service-through-a-replay-of-the-members-recording.md),
  a service beside a member.
- Aeron, read 2026-11-15 at `master` (1.55.0-SNAPSHOT):
  `aeron-cluster/src/main/resources/cluster/aeron-cluster-codecs.xml`, and in
  `aeron-cluster/src/main/java/io/aeron/cluster/`,
  `ConsensusModuleSnapshotTaker.java`, `ConsensusModule.java`,
  `ClusterControl.java`, `ClusterSession.java` and
  `service/ClusteredServiceContainer.java`.
