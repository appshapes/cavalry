- A cluster's service written in Go takes a snapshot of its program's state and starts
  from one: `service.SnapshotDue()`, `service.Snapshot(state)` and
  `service.Restore()`. `just conformance` holds them beside three members
  ([the Go client's README](clients/go/README.md);
  [ADR 0149](docs/decisions/0149-give-each-clients-service-a-snapshot-and-a-start-from-one-with-the-state-copied-to-the-caller.md)).
