- `cavalry doctor` now reports three more of a cluster member's counts, each with
  what to do: `WARN cluster.log_recordings_unwritten`, a member that could not
  write the file of its log's recordings for its service in its driver directory;
  `WARN cluster.log_recordings_unsynced`, one that could not write or sync the list
  of them in its log directory; and
  `WARN cluster.log_recordings_forgotten`, one whose log is in more than 1,024
  recordings and that will not start again on its log directory. On a cluster
  member's driver the `checks run:` count of the report is 3 higher
  ([Run a cluster of three members](docs/how-to/run-a-cluster.md#when-it-does-not-work);
  [ADR 0108](docs/decisions/0108-list-the-recordings-of-a-members-log-in-its-log-directory-and-start-a-member-on-the-log-it-kept.md)).
