- A cluster member that replicated a term it lacked goes back to canvass with no
  member's report, and so waits for its leader's answer. With the reports it held
  before, read against the shorter log it had then, it could nominate itself the
  moment it had caught up and cost a running cluster its leader for an election:
  in memory, at eight of twenty seeds, after a leader had started again on an
  empty log directory
  ([ADR 0106](docs/decisions/0106-canvass-with-no-reports-after-a-replication.md)).
