- A cluster can hold a standby: a member that follows the log and has no vote. Mark
  it in the members string with a seventh field, `,standby`, after its five
  endpoints. A standby records the log, takes snapshots and redirects clients, and
  it never leads and counts toward no quorum, so it delays no election and no
  commit. A string names at least one voter (`members-voters`). To promote a
  standby, or to replace a lost voter with one, change one member's mark in the
  string and start each member again with it, the leader last:
  [Promote a standby to a voter](docs/how-to/promote-a-standby.md). Change one mark
  at a time, and make the next change only once every member that runs follows one
  leader and a quorum of the new voters is among them:
  two marks changed at once can lose a committed entry, and no member refuses it
  ([ADR 0134](docs/decisions/0134-mark-a-standby-in-the-members-string-and-change-one-mark-at-a-time.md);
  [the cluster protocol](docs/reference/cluster.md)).
- `cavalry doctor` names a standby as one, counts a quorum over the voters, and
  reports a member that runs with other voters than the members string on its host
  (`cluster-voters`) and a standby that was asked for a vote
  (`cluster.standby_vote_requests`). Every member's `note cluster-member` line now
  ends with the members that its own string gives a vote to, `its members string
  gives a vote to members 0, 1 and 2`: read the lines of all members together to
  find one that was started with another string. Two gauges say which string a
  member runs: `cluster.voters` and `cluster.standby`.
- `cavalry-proto`: `Member::standby`, `STANDBY_MARK` and `MembersError::NoVoter`.
  `cavalry-raft`: `Consensus::with_standbys`, `Consensus::is_standby`,
  `Consensus::voters`, `MembersRefused::NoVoter`, and in its in-memory cluster
  `Cluster::with_standbys` and `Cluster::restart_with`.
