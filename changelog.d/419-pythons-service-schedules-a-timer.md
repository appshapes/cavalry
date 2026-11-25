- The Python client gives a cluster service's program its timers:
  `ClusterService.schedule_timer(correlation_id, deadline)` and
  `ClusterService.cancel_timer(correlation_id)`, for a call from inside the
  handler, which is handed a timer that is due as an entry of the type
  `CAVALRY_CLUSTER_ENTRY_TIMER` with the timer's id in `entry.correlation_id`. The
  docstring of `ClusterService.poll` now says what a poll of a closed client
  answers, −1 with `CAVALRY_ERR_CLOSED` at every poll, and the Python conformance
  runner holds it, with the timers, beside real drivers
  ([clients/python](clients/python/README.md);
  [ADR 0157](docs/decisions/0157-take-the-c-abi-to-version-7-with-a-services-timers.md)).
