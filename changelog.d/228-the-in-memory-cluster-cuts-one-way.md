- `cavalry_raft::memory::Cluster` has two more faults to set. `cut_one_way(member,
  other)` stops what one member sends from reaching another and nothing else, and
  `heal_between` ends it. `stall(member)` stops a member's consensus module while
  its driver runs: what is sent to it waits, and it reads it in order after
  `unstall(member)`. With either, a test brings a leader the request vote of a later
  term before its own quorum check: it steps down, counted in
  `cluster.leader_step_downs`, and a follower that still heard it counts
  `cluster.higher_terms_seen`
  ([ADR 0082](docs/decisions/0082-give-up-the-lead-on-a-higher-term-or-a-lost-quorum.md)).
