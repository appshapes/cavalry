- A step of the host's clock no longer moves the driver's timeouts. The driver, a
  client and the recorder measure every interval on a clock that never goes back and
  that a time service, a clock set by hand or a suspended host does not move: a step
  forward of 10 s or more used to time out every client, withdraw every image over
  UDP and end every linger, and a step back held a recording's sync and an idle
  client's keepalives for its length. The consumer heartbeat and a recording's
  timestamps stay on the wall clock, and the driver stores its heartbeat again on
  its next duty cycle after a step. Two things still follow the wall clock. A client
  can report `driver-timeout` when the clock steps forward by more than 10 s, or the
  host is suspended for longer, and it loads the heartbeat before the driver has
  stored it again. And a client that stopped sending is timed out the length of a
  step back later, as before, so that a client of an earlier release, which is
  silent for that long after a step back, is not timed out by a newer driver
  ([ADR 0109](docs/decisions/0109-measure-every-interval-on-a-nano-clock-and-keep-the-epoch-clock-for-what-another-process-reads.md)).
