- `cavalry_raft::memory::Cluster` keeps a log for each member: `log(member)` returns
  the bytes that reached that member, so a test of a service or of the cluster's
  safety can find two members that hold different bytes at one position, which a
  single shared log could not show. `cut_between(a, b)` and `heal_between(a, b)` part
  two members from each other and from nobody else
  ([the crate's README](crates/cavalry-raft/README.md)).
