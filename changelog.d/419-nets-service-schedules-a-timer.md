- The .NET client gives a cluster service's program its timers:
  `service.ScheduleTimer(correlationId, deadline)` and
  `service.CancelTimer(correlationId)`, for a call from inside the handler, which
  is handed a timer that is due as an entry of the type
  `Abi.CLUSTER_ENTRY_TIMER` with the timer's id in `entry.CorrelationId`. The
  documentation of `ClusterService.Poll` now says what a poll with a client its
  driver closed answers, −1 with `Abi.ERR_CLOSED` at every poll, and the .NET
  conformance runner holds it, with the timers, beside real drivers
  ([clients/dotnet](clients/dotnet/README.md);
  [ADR 0157](docs/decisions/0157-take-the-c-abi-to-version-7-with-a-services-timers.md)).
