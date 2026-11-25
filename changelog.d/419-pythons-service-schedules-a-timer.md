- The Python client gives a cluster service's program its timers:
  `ClusterService.schedule_timer(correlation_id, deadline)` and
  `ClusterService.cancel_timer(correlation_id)`, for a call from inside the
  handler, which is handed a timer that is due as an entry of the type
  `CAVALRY_CLUSTER_ENTRY_TIMER` with the timer's id in `entry.correlation_id`. An
  id or a deadline that is no int64 raises before the ABI is called, `TypeError`
  for a value that is no `int` and `OverflowError` for an `int` outside the range,
  which `ctypes` would have passed as another value. The docstring of
  `ClusterService.poll` now says what a poll with a client its driver closed
  answers, −1 with `CAVALRY_ERR_CLOSED` at every poll, and the Python conformance
  runner holds it, with the timers, beside real drivers
  ([clients/python](clients/python/README.md);
  [ADR 0157](docs/decisions/0157-take-the-c-abi-to-version-7-with-a-services-timers.md)).
