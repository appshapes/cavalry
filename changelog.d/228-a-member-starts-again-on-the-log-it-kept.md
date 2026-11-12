- A cluster member that stopped starts again on the log directory it had. It reads
  its log back, joins the leader the others elected and replicates what it lacks,
  where before `cavalry run` refused the directory and a cluster that had lost a
  member could only be started again as a new one. The member keeps a list of the
  recordings that hold its log in a new file in its log directory, `log-recordings`.
  A log directory written before this change has no such file, and its member is
  refused as before, with what to do. Two members are still not started again: one
  whose host lost power or whose operating system crashed while it ran, which
  nothing refuses and the how-to tells you to leave stopped, and one whose log is
  in more than 1,024 recordings, which `cavalry run` refuses with `log-forgotten`.
  Four counters say what a member's list did: `cluster.log_recordings_listed`,
  `cluster.log_recordings_unsynced`, `cluster.log_recordings_forgotten` and
  `cluster.truncations_finished`
  ([Run a cluster of three members](docs/how-to/run-a-cluster.md), step 7;
  [ADR 0108](docs/decisions/0108-list-the-recordings-of-a-members-log-in-its-log-directory-and-start-a-member-on-the-log-it-kept.md)).
