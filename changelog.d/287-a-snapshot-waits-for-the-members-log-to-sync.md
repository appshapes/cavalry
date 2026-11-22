- A cluster member lists a snapshot of its consensus module only once its own log has
  synced to the snapshot's position, up to 2 s after the snapshot's recording stops,
  so that no crash leaves it a snapshot that its log does not reach and its start
  refuses; a log that does not sync in that time gives the snapshot up, counted in
  `cluster.snapshots_failed`
  ([ADR 0143](docs/decisions/0143-list-a-members-snapshot-once-its-log-has-synced-to-the-snapshots-position.md)).
