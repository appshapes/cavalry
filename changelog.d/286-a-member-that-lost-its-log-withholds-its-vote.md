- A cluster member started on a new log directory beside members that hold the
  cluster's log comes back by itself and safely. It replicates the log, and it casts
  no vote and seeks no leadership until its log reaches what its leader held when it
  joined, the vote floor `cluster.vote_floor` holds. Meanwhile it votes only when
  every voter has reported a log the candidate's is at least, and
  `cluster.votes_withheld` counts the requests it left unanswered. The rule "never
  start one member alone on a new log directory" is gone. The durable log is at
  version 6, a slot of the terms file's header
  ([ADR 0145](docs/decisions/0145-withhold-the-vote-of-a-member-that-lost-its-log-until-it-holds-what-the-cluster-held.md)).
