- A cluster leader made with `cavalry_raft::Consensus::with_election` closes its
  election when a quorum of members, itself among them, has joined its log, as row 8
  of `spec/cluster.md` has it. It waited for every member it had heard, for the
  leader heartbeat timeout, 10 s, at most. A member that joins after the term's first
  entry catches up with it. A member started with `cavalry run` is unchanged: its
  leader is appointed, and waits as before
  ([ADR 0088](docs/decisions/0088-catch-up-within-a-leaders-term-and-join-the-live-log-from-the-replay.md)).
