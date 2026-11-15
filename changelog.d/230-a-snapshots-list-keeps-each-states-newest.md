- A cluster member's list of snapshots keeps the newest snapshot of each state. The
  list names 64 snapshots at most, and one more used to push out the oldest,
  whichever state it was of: a service that took no snapshot while its member took
  64 lost the one it had, and with a log purged below that snapshot it could not
  start. A full list now leaves out the oldest snapshot of a state that has a newer
  one. The file's format does not change
  ([ADR 0133](docs/decisions/0133-keep-the-newest-snapshot-of-each-state-in-a-members-list.md);
  [the durable log](docs/reference/durable-log.md)).
