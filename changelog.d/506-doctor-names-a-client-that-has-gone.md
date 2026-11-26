- `cavalry doctor` names a client that closed or was timed out and whose process still
  holds its lock, with the file in the driver's directory that says so, once it has
  held it for 2 s while the driver runs: the driver gives that client's counters'
  records to nobody until then. And for a counters file of a newer build than its own, `doctor` says to run
  that build's `cavalry doctor`, where it said to restart the driver
  ([ADR 0179](docs/decisions/0179-give-a-record-a-client-writes-to-another-owner-only-when-its-writer-has-let-it-go.md)).
