- A cluster member lists the recordings that hold its log in a file in its driver
  directory, `log-recordings-<member id>`, written before its
  `cluster.log_recording_id` counter names a recording and removed when the member
  closes. `ConsensusModule::do_work` returns the new `RaftError::LogRecordings`,
  with what to do, when the member cannot write the file; the member replicates
  meanwhile and names no recording to a service until the file is written. A member
  truncates no recording of its log while the counter names one. No service reads
  the file yet: the change that follows does
  ([ADR 0103](docs/decisions/0103-list-the-recordings-of-a-members-log-in-its-driver-directory-for-a-service-to-read-in-order.md)).
