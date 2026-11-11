- The C ABI is at version 2 and feeds a cluster's committed log to a service in C.
  `cavalry_committed_log(&log, client, position)` opens the log of the cluster member
  on the client's driver, with a null handle and no error while the driver hosts no
  member; `cavalry_committed_log_poll` hands the fragment handler each entry a quorum
  has committed, none past the commit position, and
  `cavalry_committed_log_entry_position(header)` gives the handler the entry's position
  in the log; `cavalry_committed_log_position` is where a service that has applied
  every entry opens the log again; and once `cavalry_committed_log_is_ended` is true,
  `cavalry_committed_log_failed_at` and `cavalry_committed_log_withdrawn_at` say
  whether the handle ended short of the entries it was owed, and where. The Python,
  Node, Go, JVM and .NET packages are regenerated from the version 2 header and
  refuse a version 1 library; each gets its own committed log object in a later
  change
  ([ADR 0070](docs/decisions/0070-take-the-c-abi-to-version-2-with-the-committed-log.md);
  card 227).
