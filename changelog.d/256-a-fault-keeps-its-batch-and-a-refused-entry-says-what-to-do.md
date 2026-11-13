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
- A cluster member no longer holds its duty cycle's thread while it waits for a
  recording to stop, which it did for up to 1 s each time it left the log, gave up
  the lead or ended a catch-up. The cycle that asks the stop returns at once, the
  member stays where it is, and a later cycle reads where the recording stopped. A
  member in init now leaves the consensus messages that arrive on the stream and
  acts on them once it has left init, where before it read and dropped them with no
  count. `Io::leave_log`, `Io::remove_log` and `Io::remove_catchup` return whether
  they are done ([`cavalry-raft`'s README](crates/cavalry-raft/README.md)).
