- A cluster member made with `cavalry_raft::Consensus::with_election` truncates its
  log to join a leader: told a next term whose base is below its append position, it
  cuts what it holds above that base, the tail of an older term that no quorum
  recorded, before it records or reports anything. It never cuts below the commit
  position it knows: told to, it keeps its log and fails its election. Two counters
  follow, `cluster.log_truncations` and `cluster.truncations_refused`, and
  `cavalry_raft::Io` has one more method, `truncate_log`. A member started with
  `cavalry run` is unchanged: its leader is appointed, and both counters stay 0
  ([ADR 0083](docs/decisions/0083-truncate-to-the-next-terms-base-and-never-below-the-commit-position.md)).
