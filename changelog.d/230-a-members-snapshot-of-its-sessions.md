- A cluster member takes snapshots of its sessions. The leader asks for one each
  time its log has grown by 256 MiB, with a cluster action in the log, and every
  member writes its snapshot when it reads that entry: the open sessions and the
  next session id, as a recording of its own, listed in the member's snapshots
  file. The three members' snapshots are of one position and hold the same bytes.
  The member's counter `cluster.snapshot_position` says where its newest snapshot
  is, and `cavalry doctor` reports `cluster.snapshots_failed` above 0, a snapshot
  that a member gave up with its log whole. A service takes no snapshot yet: a
  change that follows in work item 230 gives it one ([ADR 0129](docs/decisions/0129-snapshot-a-members-sessions-when-its-read-of-its-log-reaches-the-leaders-action.md);
  [the cluster protocol](docs/reference/cluster.md)).
- `cavalry-raft`: `ConsensusModule::snapshot()` asks for a snapshot for the leader's
  own process, and `snapshot_position()` and `snapshot_counts()` say what a member
  took. `cavalry-proto` reads and writes a snapshot's two messages, cluster session
  and consensus module.
