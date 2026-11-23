- A cluster's service written in Python takes a snapshot of its program's state and
  starts from one: `service.snapshot_due()`, `service.snapshot(state)` and
  `service.restore()`. `just conformance` holds them beside three members, with
  `cavalry snapshot` asking for the snapshot
  ([the Python client's README](clients/python/README.md);
  [ADR 0149](docs/decisions/0149-give-each-clients-service-a-snapshot-and-a-start-from-one-with-the-state-copied-to-the-caller.md)).
