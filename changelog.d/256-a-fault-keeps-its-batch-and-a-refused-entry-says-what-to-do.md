- A cluster member whose duty cycle faults on one consensus message no longer loses
  the messages read with it: each is handed to the election as the next cycle would
  have handed it, and the cycle returns the first fault. Before, up to fifteen
  messages of the same poll were taken from the stream and dropped with no count
  ([`cavalry-raft`'s README](crates/cavalry-raft/README.md)).
- A leader whose log publication refuses an entry for a reason that waiting does not
  mend now says what to do: `member stopped (append)` names the refusal, `closed`,
  `max-position-exceeded` or `too-long`, and the next step for each, where the
  message was a driver error with none
  ([Run a cluster of three members](docs/how-to/run-a-cluster.md#when-it-does-not-work)).
