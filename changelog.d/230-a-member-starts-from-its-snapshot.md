- A cluster member starts from its newest snapshot. Its start takes its sessions
  from the snapshot and reads its log from the snapshot's position, and no longer
  from position 0, so a start reads the log written since that snapshot: at most
  the 256 MiB between two snapshots and what was appended after. A member needs
  its log from that position alone. An operator can purge the recordings of a
  member's log that end below its newest snapshot while the member is stopped, and
  a member whose log is in more than 1,024 recordings starts again when its newest
  snapshot is at or above where the log it kept begins. A snapshot that a start
  cannot read is passed over for the one listed before it, or for none:
  `cluster.snapshots_passed_over` counts it and `cavalry doctor` reports it, and
  `cluster.snapshot_start_position` says where the member's read began. A service
  still reads its member's log from position 0, so purge only where no service
  starts again from the beginning of the log
  ([ADR 0130](docs/decisions/0130-start-a-member-from-its-newest-snapshot-and-hold-its-log-to-that-position-alone.md);
  [Run a cluster of three members](docs/how-to/run-a-cluster.md#when-it-does-not-work)).
- `cavalry-raft`: `RaftError::LogForgotten` gains `from_position`, the position the
  start would read the log from, beside `floor_position`, and
  `snapshots_passed_over`, how many newer snapshots the start could not read. Its
  message names the two positions and, above 0, the count, with what to do first.
