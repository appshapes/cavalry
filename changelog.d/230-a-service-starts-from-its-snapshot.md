- A cluster's service starts from its snapshot. `ClusterService::restore(client)`,
  called before the first poll, gives the program its state back as the newest
  snapshot of the service held it, and the service then reads its member's log
  from that snapshot's position, with the sessions that were open there. A program
  that does not call it reads the log from position 0, as before. A snapshot that
  the service cannot take is passed over, counted in
  `cluster.service_snapshots_passed_over` and reported by `cavalry doctor`, and the
  service reads from position 0. A driver that refuses the start for anything but
  the snapshot's recording, a registry at its bound among them, makes `restore`
  return the driver's error, and the program calls it again. The member's counter `cluster.log_purge_position`
  says from which position its log is needed, the lower of its own snapshot's and
  its service's: a recording of its log that stopped at or below it may be purged
  while the member is stopped
  ([ADR 0132](docs/decisions/0132-start-a-service-from-the-snapshot-its-member-names-when-its-program-asks.md);
  [the cluster protocol](docs/reference/cluster.md)).
- `cavalry-client`: `Restored`, what `restore` found, and
  `ClusterError::RestoreTooLate`. `cavalry-raft`:
  `ConsensusModule::log_purge_position()`.
