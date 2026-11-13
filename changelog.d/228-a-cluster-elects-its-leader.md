- A cluster started with `cavalry run` elects its leader. The members canvass, one
  nominates itself and wins the others' votes, and when the leader is lost the two
  that remain elect another after the leader heartbeat timeout, 10 s, in an election
  of some tenths of a second. A leader whose member stops running while its driver
  runs on is replaced too, after one election or several, each 10 s after the last:
  10 s to 52 s in the runs measured. Which member leads is no
  longer the one with the lowest id; `cavalry doctor` names the leader on every
  member and says why its last election began. A member that starts late, or that
  is behind its leader, catches up and joins where it was left out before
  ([Run a cluster of three members](docs/how-to/run-a-cluster.md);
  [ADR 0104](docs/decisions/0104-have-the-consensus-module-elect-its-leader-and-take-the-appointed-leader-out.md)).
- Changed: do not start one member alone on a new log directory beside the others. It
  now joins and votes, with no record of its earlier votes. A member still does not
  start again on the log directory it kept; the cluster goes on with the two that
  remain, and `cavalry run` says so when it refuses. It also refuses, as
  `member refused (terms-lost)`, a log directory that holds the cluster's log and no
  `terms` file, which a new one would have been made for before. Keep each member's
  log directory on a volume, and run every member on this version: a member of the
  version before casts no vote
  ([Run a cluster of three members](docs/how-to/run-a-cluster.md#prerequisites)).
- Removed: `Consensus::new` and `memory::Cluster::new` in `cavalry-raft`, the
  appointed leader's path; `Consensus::with_election` and
  `memory::Cluster::with_election` are the constructors. `appointed_leader`,
  `appointed_canvass` and `leader_ready` in `cavalry_proto::cluster`, with their 25
  vectors. `cluster.appointed_leader_behind` is added by no member.
