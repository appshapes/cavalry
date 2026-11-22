- `cavalry snapshot` asks a cluster for a snapshot now: run on the host of the member
  that leads, it has every member take one and prints the snapshot's log position once
  that member lists its own. Beside a member that does not lead it asks nothing and
  names the leader. `cavalry doctor` reports a request that waits as
  `cluster.control_toggle`
  ([Ask a cluster for a snapshot](docs/how-to/ask-for-a-snapshot.md);
  [ADR 0147](docs/decisions/0147-ask-a-member-for-a-snapshot-with-a-control-toggle-that-the-asking-command-owns.md)).
