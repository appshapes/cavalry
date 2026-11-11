- `cavalry_raft::Consensus::with_election(member_ids, member_id, seed)` makes a cluster
  member that elects its leader: it canvasses, nominates itself after a delay drawn
  from its seed, and leads the term a quorum votes for, at once when every member it
  has heard has voted for it. Four counters follow the ballot: `cluster.candidacies`,
  `cluster.ballots_lost`, `cluster.votes_for` and `cluster.votes_against`. A member
  started with `cavalry run` is unchanged: its leader is appointed until the election
  is whole, and the crate's README lists what the electing path does not do yet
  ([ADR 0081](docs/decisions/0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md)).
