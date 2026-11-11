- A cluster member made with `cavalry_raft::Consensus::with_election` catches up
  within its leader's term: one that has joined the term and is behind the leader's
  log asks for the log from its append position, records the leader's replay, adds
  the live log when it is within 1,048,576 bytes of the leader's position, and joins
  when the leader sends stop catch-up. So a leader that gave up the lead, and a
  member that was cut off or stopped, come back onto the new leader's log, where
  they failed an election every 100 ms before. `ElectionState` has three more
  states, `FollowerCatchupInit`, `FollowerCatchupAwait` and `FollowerCatchup`, and
  `cluster.catchups` now rises on the member too. A member started with `cavalry
  run` is unchanged: its leader is appointed
  ([ADR 0088](docs/decisions/0088-catch-up-within-a-leaders-term-and-join-the-live-log-from-the-replay.md)).
