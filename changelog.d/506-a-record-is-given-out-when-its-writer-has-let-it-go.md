- A subscriber no longer loses messages when its client's conductor is not run for
  more than a second while one publication of its stream ends and the next begins,
  and a client that was stopped past the driver's timeout no longer stores into a
  counter another client owns. In both, a thread stored a position into a record of
  the counters file that the driver had given to another owner after a second. In the
  first case a publisher was let 256 MiB past its window and its subscriber lost
  98,304 messages with no fault reported. The driver now gives a record that a client
  writes to another owner only when the thread that writes it has stored the record's
  release word, or when the client is gone, which a lock the client holds on
  a file of its own in `clients/` says. `counters.held` counts the records held meanwhile
  and `cavalry doctor` names a client that has gone and still holds its lock
  ([ADR 0179](docs/decisions/0179-give-a-record-a-client-writes-to-another-owner-only-when-its-writer-has-let-it-go.md)).
- The counters file is at version 1 and the client protocol at version 11, and a
  client and a driver of the two sides of this change do not attach to each other:
  each refuses with `counters file rejected (version)` and says which side is the
  older. On one host, stop the driver and every client of it together and start
  them from the newer build, the driver first. Between hosts nothing changes
  ([Upgrade a cluster one member at a time](docs/how-to/upgrade-a-cluster.md)).
