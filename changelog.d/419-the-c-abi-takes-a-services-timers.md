- The C ABI is at version 7 and gives a service's program its timers:
  `cavalry_cluster_service_schedule_timer` and
  `cavalry_cluster_service_cancel_timer`, for a call from inside the handler, and an
  entry of the kind `CAVALRY_CLUSTER_ENTRY_TIMER` with the timer's id in the new
  field `correlation_id` of `cavalry_cluster_entry_values_t`. A program built
  against version 6 is refused at its first call until it is rebuilt against this
  header, and a client's package of version 6 when it loads. A program of version
  6 was handed every timer event of the log as `_OTHER`; a program of this version
  is handed the event of a timer that is pending and due, as `_TIMER`, and no other
  ([cavalry-ffi](crates/cavalry-ffi/README.md);
  [ADR 0157](docs/decisions/0157-take-the-c-abi-to-version-7-with-a-services-timers.md)).
