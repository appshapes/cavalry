- A cluster's service written for Node takes a snapshot of its program's state and
  starts from one: `service.snapshotDue()`, `service.snapshot(state)` and
  `service.restore()`. `just conformance` holds them beside three members
  ([the Node client's README](clients/node/README.md);
  [ADR 0149](docs/decisions/0149-give-each-clients-service-a-snapshot-and-a-start-from-one-with-the-state-copied-to-the-caller.md)).
