- `cavalry_raft::memory::Cluster` keeps a log for each member: `log(member)` returns
  the bytes that reached that member, so a test of a service or of the cluster's
  safety can find two members that hold different bytes at one position, which a
  single shared log could not show. `cut_between(a, b)` and `heal_between(a, b)` part
  two members from each other and from nobody else. `cut(member)` of the leader now
  stops its log reaching every follower too, where before only a follower's own cut
  stopped the log reaching it: what a cut leader appends extends its own log alone
  until the cut ends ([the crate's README](crates/cavalry-raft/README.md)).
