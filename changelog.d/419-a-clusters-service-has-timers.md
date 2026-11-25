- A cluster's service has timers in Rust: its program calls
  `Replies::schedule_timer` and `Replies::cancel_timer` while it handles an entry
  and is handed `Entry::Timer` when the timer is due, once, at one position beside
  every member. `Entry` gains that variant, so a `match` that names every entry
  needs an arm for it. A service written through the C ABI has no timer yet
  ([cavalry-client](crates/cavalry-client/README.md);
  [ADR 0151](docs/decisions/0151-give-a-clusters-service-timers-that-every-service-decides-from-the-log.md)).
