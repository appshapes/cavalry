- The Go client gives a cluster service's program its timers:
  `service.ScheduleTimer(correlationID, deadline)` and
  `service.CancelTimer(correlationID)`, for a call from inside the handler, which
  is handed a timer that is due as an entry of the type `ClusterEntryTimer` with
  the timer's id in `entry.CorrelationID`. The comment of `ClusterService.Poll`
  now says what a poll with a client its driver closed answers, −1 with
  `ErrClosed` at every poll, and the Go conformance runner holds it, with the
  timers, beside real drivers
  ([clients/go](clients/go/README.md);
  [ADR 0157](docs/decisions/0157-take-the-c-abi-to-version-7-with-a-services-timers.md)).
