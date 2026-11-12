- Added: two gauges a cluster member stores in its driver's counters file.
  `cluster.leader_member_id` is the id of the member it leads as or follows, and -1
  while it has none: in init, canvass, nominate and the two ballots, so from the
  start of an election until it leads or takes a leader's new leadership term, and
  in the canvass between two replications of terms it missed.
  `cluster.election_cause` says why its last election
  began: 0 it started, 1 its leader said nothing for the leader heartbeat timeout, 2
  its image of the leader's log was lost, 3 it led and lost its quorum, 4 a message
  carried a term above its own, 5 its election before failed. `cavalry-raft` gains
  `Consensus::leader_gauge`, `Consensus::election_cause` and `ElectionCause`
  ([ADR 0102](docs/decisions/0102-publish-who-a-member-follows-and-why-its-last-election-began-as-two-gauges-on-both-paths.md)).
