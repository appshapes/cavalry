- `cavalry_raft::memory::Cluster::with_election(member_ids, seed)` is a cluster in
  memory whose members elect their leader, with the same elections at every run of
  one seed. `leader()` names the member that leads, `offer` goes to it, and
  `log_to(member, position)` returns a member's log up to a position. A leader that
  stops with its followers level is replaced within a second of the first heartbeat
  timeout; a member that is behind or ahead of the new leader does not join yet
  ([the crate's README](crates/cavalry-raft/README.md), "What the election does not
  do yet").
