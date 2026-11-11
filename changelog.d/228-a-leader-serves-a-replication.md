- A cluster leader made with `cavalry_raft::Consensus::with_election` answers a
  member that asks for a term it missed: a catch-up position below the leader's term
  base log position starts a replay of the leader's own recordings, from the position
  asked to the end of the term that holds it, one replay a member at a time. The
  leader stops the replay of a member that canvasses or asks again, and every replay
  when it holds an election. One counter follows, `cluster.log_replications`, and
  `cavalry_raft::Io` has four more methods: `term_base_above`, `start_replay`,
  `stop_replay` and `replay_stopped`. No member asks yet, and a member started with
  `cavalry run` is unchanged: its leader is appointed, and the counter stays 0
  ([ADR 0085](docs/decisions/0085-serve-a-replication-from-the-leaders-core-to-the-endpoint-the-members-string-names.md)).
