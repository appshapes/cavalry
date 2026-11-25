- A cluster member holds the timers that the service beside it tells it of, and
  its leader appends a timer event for each when it is due, the lower correlation
  id first of two with one deadline. Four counters say what it holds and did,
  `cluster.timers`, `cluster.timer_events`, `cluster.timers_replaced` and
  `cluster.timers_cleared`, and `cavalry doctor` reports a member that removed
  every timer it held. No service has the calls yet
  ([Run a cluster](docs/how-to/run-a-cluster.md#when-it-does-not-work);
  [ADR 0151](docs/decisions/0151-give-a-clusters-service-timers-that-every-service-decides-from-the-log.md);
  [ADR 0158](docs/decisions/0158-append-the-timer-event-of-the-lower-correlation-id-first-of-two-timers-with-one-deadline.md)).
