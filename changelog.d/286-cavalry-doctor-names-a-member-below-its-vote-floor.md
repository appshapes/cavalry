- `cavalry doctor` names a cluster member that came back on an empty log directory.
  `WARN cluster.vote_floor` says how far its log is below its vote floor while it
  withholds its vote, and goes once it has caught up. `WARN cluster.votes_withheld`
  counts the requests it answered with no vote, and `WARN cluster.leaders_withheld`
  the leaders it passed over while it still has none, which names a member that is
  stopped or cannot reach it. Each line says what to do
  ([Run a cluster](docs/how-to/run-a-cluster.md);
  [ADR 0145](docs/decisions/0145-withhold-the-vote-of-a-member-that-lost-its-log-until-it-holds-what-the-cluster-held.md)).
