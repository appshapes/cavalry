- A cluster's leader takes a control toggle: a counter, `cluster.control_toggle`, that
  a command on its host adds to ask it for a snapshot. The leader appends one cluster
  action for each toggle it takes and says what it took in `cluster.control_toggle_taken`
  and `cluster.control_toggle_snapshot_position`. The command that adds the toggle,
  `cavalry snapshot`, is the next change
  ([ADR 0147](docs/decisions/0147-ask-a-member-for-a-snapshot-with-a-control-toggle-that-the-asking-command-owns.md);
  [Cluster protocol](docs/reference/cluster.md#snapshots)).
