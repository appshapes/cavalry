- Changed: `cavalry doctor`'s `cluster-member` note ends with who the member follows
  and why its last election began, read from `cluster.leader_member_id` and
  `cluster.election_cause`, and `cluster-joining` names the leader the member has
  taken from that gauge. A member that elects its leader, which no member started
  with `cavalry run` is yet, gets findings of its own: `cluster-no-leader` says why
  the election began and that a quorum elects a leader, `cluster-joining` covers a
  replication and a catch-up, and neither says to start the cluster again
  ([Run a cluster](docs/how-to/run-a-cluster.md#when-it-does-not-work)).
