- `cavalry-proto` reads and writes the file in which a cluster member lists the
  recordings that hold its log, so that a service can read a log that is in several
  recordings: `LogRecordings::read`, `LogRecordings::recording_at` and
  `write_log_recordings`, with the file's layout in `spec/cluster.md`, "The log's
  recordings". No member writes the file yet and no service reads it: the changes
  that follow build both
  ([ADR 0103](docs/decisions/0103-list-the-recordings-of-a-members-log-in-its-driver-directory-for-a-service-to-read-in-order.md)).
