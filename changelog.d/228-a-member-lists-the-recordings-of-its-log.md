- A cluster member lists the recordings that hold its log in a file in its driver
  directory, `log-recordings-<member id>`, written before its
  `cluster.log_recording_id` counter names a recording and removed when the member
  closes, after the member has stored −1 in that counter. `ConsensusModule::do_work` returns the new `RaftError::LogRecordings`,
  with what to do, when the member cannot write the file: to make the driver
  directory writable, or, when the list the member made is one the file's format
  refuses, that the fault is the member's own and is to be reported. In each duty cycle
  that fails so the member still replicates, and it names no recording to a service
  until the file is written; `cavalry run` tries such a cycle again every 100 ms and
  stops the member after 10 s of them, as for any error of a duty cycle, and
  `cluster.log_recordings_unwritten` counts each. A member
  truncates no recording of its log while the counter names one. No service reads
  the file yet: the change that follows does
  ([ADR 0103](docs/decisions/0103-list-the-recordings-of-a-members-log-in-its-driver-directory-for-a-service-to-read-in-order.md)).
