- The counters file is at version 1 and the client protocol at version 11, and a
  client and a driver of the two sides of this change do not attach to each other:
  each refuses with `counters file rejected (version)` and says which side is the
  older. On one host, stop the driver and every client of it together and start
  them from the newer build, the driver first. Between hosts nothing changes
  ([Upgrade a cluster one member at a time](docs/how-to/upgrade-a-cluster.md);
  [ADR 0179](docs/decisions/0179-give-a-record-a-client-writes-to-another-owner-only-when-its-writer-has-let-it-go.md)).
