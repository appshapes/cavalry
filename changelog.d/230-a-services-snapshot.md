- A cluster's service takes a snapshot of its program's state. The leader's request
  for a snapshot is a new entry for a service's handler, `Entry::Snapshot`, and the
  poll that hands it over ends after it. `ClusterService::snapshot(client, state)`
  then writes the program's state, 1 GiB at most, with the open sessions, to a
  recording of the service's own, and tells the member, which lists the snapshot in
  its snapshots file beside its own. `ClusterService::snapshot_due()` says when one
  is asked for. The member's counter `cluster.service_snapshot_position` says where
  the newest one is, and `cavalry doctor` reports a snapshot that a service gave up,
  `cluster.service_snapshots_failed`, and one that a member could not list,
  `cluster.service_acks_rejected`. A program that takes no snapshots changes
  nothing. A service does not start from its snapshot yet, and a service written
  in Python, Node, Go, the JVM or .NET takes none yet
  ([ADR 0131](docs/decisions/0131-have-a-service-write-its-own-snapshot-and-its-member-list-it.md);
  [the cluster protocol](docs/reference/cluster.md)).
- `cavalry-client`: `ClusterError` gains `NoSnapshotDue`, `SnapshotTooLong` and
  `SnapshotFailed`, whose `SnapshotCause` names the step. `cavalry-raft`:
  `ConsensusModule::service_snapshot_position()` and `service_snapshot_counts()`.
  `cavalry-proto` reads and writes the client session of a service's snapshot and
  the service ack.
