- The Node client gives a cluster service's program its timers:
  `service.scheduleTimer(correlationId, deadline)` and
  `service.cancelTimer(correlationId)`, for a call from inside the handler, which
  is handed a timer that is due as an entry of the type
  `CAVALRY_CLUSTER_ENTRY_TIMER` with the timer's id in `entry.correlationId`, a
  BigInt. An id and a deadline are each a BigInt or a number that is an integer
  a number carries exactly: a number with a fraction or past the safe integers,
  and a BigInt outside int64, throw a `RangeError` before the ABI is called, where
  N-API would have passed another value. The docstring of `ClusterService.poll`
  now says what a poll with a client its driver closed answers, −1 with
  `CAVALRY_ERR_CLOSED` at every poll, and the Node conformance runner holds it,
  with the timers, beside real drivers
  ([clients/node](clients/node/README.md);
  [ADR 0157](docs/decisions/0157-take-the-c-abi-to-version-7-with-a-services-timers.md)).
