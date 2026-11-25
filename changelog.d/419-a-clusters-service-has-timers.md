- A cluster's service has timers in Rust: its program calls
  `Replies::schedule_timer` and `Replies::cancel_timer` while it handles an entry
  and is handed `Entry::Timer` when the timer is due, once, at one position beside
  every member. `Entry` gains that variant, so a `match` that names every entry
  needs an arm for it. A program schedules its first timer only once every member
  of its cluster runs version 2 of the cluster protocol. Three counters say what a
  service holds and did, `cluster.service_timers`,
  `cluster.service_timer_events_passed_over` and
  `cluster.service_timer_cancels_dropped`, and `cavalry doctor` names a leader
  that was not told of the timers its service has pending and a service that
  dropped a cancel. A service written through the C ABI has no timer yet
  ([cavalry-client](crates/cavalry-client/README.md);
  [Run a cluster](docs/how-to/run-a-cluster.md#when-it-does-not-work);
  [ADR 0151](docs/decisions/0151-give-a-clusters-service-timers-that-every-service-decides-from-the-log.md);
  [ADR 0159](docs/decisions/0159-report-to-the-program-a-publication-the-driver-refuses-while-a-service-tells-its-member-of-a-timer.md)).
