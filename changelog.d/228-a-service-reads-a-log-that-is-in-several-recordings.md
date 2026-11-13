- A service reads its cluster member's committed log across the member's recordings.
  A member's log is in several recordings once the member has left the log and
  joined it again or caught up through a replay, and a service whose position was
  below the newest one was refused with error code 23. The member lists the
  recordings of its log in a file in its driver directory, `log-recordings-<member
  id>`, and `Cavalry::committed_log(position)`, `cavalry_committed_log` in C and
  each client's call now open the recording that holds the position. A handle is ended
  at its recording's stop with nothing reported, and the log opened again at the
  handle's position is the next recording, so a service that starts at 0 reads
  every entry once, in order. The example service opens the log again by itself. A
  service that opens the log while its member closes is told there is no log, and
  asks again: with no file the call loads the member's counter a second time, and
  opens the live recording only when the counter still names it. No
  call is added and none changes
  ([Read a cluster's committed log from a service](docs/how-to/read-the-committed-log.md);
  [ADR 0103](docs/decisions/0103-list-the-recordings-of-a-members-log-in-its-driver-directory-for-a-service-to-read-in-order.md)).
