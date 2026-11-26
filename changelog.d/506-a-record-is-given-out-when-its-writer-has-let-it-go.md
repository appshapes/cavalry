- A subscriber no longer loses messages when its client's conductor is not run for
  more than a second while one publication of its stream ends and the next begins,
  and a client that was stopped past the driver's timeout no longer stores into a
  counter another client owns. In both, a thread stored a position into a record of
  the counters file that the driver had given to another owner after a second. In the
  first case a publisher was let 256 MiB past its window and its subscriber lost
  98,304 messages with no fault reported. The driver now gives a record that a client
  writes to another owner only when the thread that writes it has stored the record's
  release word, or when the client is gone, which a lock the client holds on
  `clients/<client id>.lock` says. `counters.held` counts the records held meanwhile
  ([ADR 0179](docs/decisions/0179-give-a-record-a-client-writes-to-another-owner-only-when-its-writer-has-let-it-go.md)).
