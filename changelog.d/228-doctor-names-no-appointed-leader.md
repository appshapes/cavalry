- Removed: `cavalry doctor`'s checks `cluster-leader-behind` and
  `cluster-joined-late`, and every text of `doctor` that named an appointed leader
  or told an operator to start the cluster again. A cluster elects its leader, and
  `doctor` now says of every member what it said of a member that elects:
  `cluster-no-leader` and `cluster-joining`, each ending with the elections the
  member has failed and the counter of each rule. On a cluster member's driver the
  `checks run:` count of the report is 2 lower
  ([Run a cluster of three members](docs/how-to/run-a-cluster.md#when-it-does-not-work);
  [ADR 0107](docs/decisions/0107-take-the-appointed-leader-out-of-cavalry-doctor-and-retire-its-counter.md)).
- Removed: the counter `cluster.appointed_leader_behind`, id 113, which no member
  added since a cluster elects its leader. The id is never used again, and a driver
  refuses `add counter` for it. The `cluster.member.id` key's doc no longer calls
  the member with the lowest id the appointed leader.
