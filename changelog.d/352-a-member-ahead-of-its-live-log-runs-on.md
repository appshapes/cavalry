- A cluster member whose catch-up ended ahead of its recording of the live log runs
  on. A follower that joins late catches up through a replay, and that replay can
  pass what the follower's live recording has synced while a slower member paces
  the leader's log. The member's read of its own log then asked its driver for a
  position that the live recording had not reached, the driver refused with error
  23, and each duty cycle failed on that refusal for as long as the live recording
  was behind: `cavalry run` gave the member up when that lasted 10 s. There is now
  no log at that position yet. The member waits and reads on when its live
  recording has reached it, and `Cavalry::committed_log` and
  `cavalry_committed_log` answer a service beside it with no log, `None` and a
  null handle, where they returned the refusal. A position past the end of a
  member's log is answered so too, where it was refused with error 23:
  `cluster-service --from` with such a position now waits, and the example's last
  line when its member has no log at its position reads `the member has no log
  from position`, where it read `the member's log ended`. Error 23 is still the
  answer for a position that is not a multiple of 32, for one below every recording
  the member keeps, and for one that a purge of a stopped member's log took
  ([Read the committed log](docs/how-to/read-the-committed-log.md#when-it-does-not-work);
  [ADR 0135](docs/decisions/0135-answer-no-log-yet-where-a-members-live-recording-has-not-reached-the-position.md);
  work item 352).
