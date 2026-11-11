- A cluster member made with `cavalry_raft::Consensus::with_election` gives up the
  lead: a leader holds an election when fewer than a quorum of members report to it
  for the leader heartbeat timeout, 10 s, and when a canvass position, a request vote
  or a commit position carries a term above its own, and it is a follower from then
  on. A follower holds an election on a request vote or a commit position of a higher
  term too. Two counters follow: `cluster.leader_step_downs` and
  `cluster.higher_terms_seen`; `cluster.leader_quorum_losses` now counts a leader
  that held an election. `cavalry_raft::Io` has one more method, `remove_log`, by
  which a leader gives its log publication up. A member started with `cavalry run`
  is unchanged: its leader is appointed, and those two counters stay 0
  ([ADR 0082](docs/decisions/0082-give-up-the-lead-on-a-higher-term-or-a-lost-quorum.md)).
