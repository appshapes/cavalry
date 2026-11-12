- A cluster member made with `cavalry_raft::Consensus::with_election` replicates a
  term it missed: told a new leadership term that leaves it below the leader's term
  base, it asks the leader for the log from its append position, records what the
  leader replays to the end of that term, and canvasses again, one term at a time,
  until it is at the leader's base. It records a term it holds none of before it
  asks for it, so a replication that fails midway is carried on from where it
  stopped. Its election fails when its append position does not move for the leader
  heartbeat timeout, counted in the new `cluster.log_replications_unavailable`, or
  when a replay joins somewhere else than where its log ends, counted in
  `cluster.join_position_mismatches`, whose `doctor` text now says what to check
  for that case and not only for a late start. `cluster.election_steps` counts the
  steps into and out of follower log replication, two more for each term a member
  replicates, and its text says so. `cavalry_raft::Io` has
  four more methods, `add_catchup`, `send_catchup_position`, `catchup_image` and
  `remove_catchup`, and `ElectionState` has `FollowerLogReplication`. A member that
  is then behind its leader's log within the leader's term still cannot join: that
  is the catch-up, the next step. A member started with `cavalry run` is unchanged:
  its leader is appointed, and both replication counters stay 0
  ([ADR 0086](docs/decisions/0086-record-a-term-when-its-replication-begins-and-check-each-replays-image-against-the-position-last-sent.md)).
