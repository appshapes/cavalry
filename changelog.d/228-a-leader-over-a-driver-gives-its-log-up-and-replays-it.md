- The consensus module's I/O can give a leader's log up and replay it, for the
  election a member over a driver does not hold yet: it keeps the last 1,024
  recordings of the log it has stopped, stops a leader's recording and closes its
  publication so that the member can lead a later term in the same process, and
  replays the recording that holds a position to a member's `catchup` endpoint, to
  an end or following its live recording. `ConsensusModule::do_work` now stores the
  member's counters before it returns a fault. A member started with `cavalry run`
  is unchanged: its leader is appointed and asks for none of it
  ([ADR 0091](docs/decisions/0091-give-a-leaders-log-up-and-replay-it-over-a-driver-from-the-recordings-the-module-stopped.md)).
