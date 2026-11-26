- A cluster service now adds its five counters and its publication to its member when
  it first opens its member's log, and no longer in the poll in which its program
  first schedules a timer. That poll waited tens of milliseconds, with the entries
  behind it waiting too: the publication's log is 192 MiB, and the driver and then
  the service's client touch every page of it. A program's first timer now costs what
  a later one costs: its event was stamped 15 to 27 ms after its message in ten runs
  of each client's round on a laptop, where it was 38 to 68 ms. Every service holds
  those counters and that log from its first open, a program that schedules no timer
  and takes no snapshot included: about 240 MiB of logs in the driver's directory for
  such a service, where it was about 48 MiB. A driver that refuses one of the six,
  as where its directory has no room for the log, is the error of the poll that first
  opens the log, and of one poll each second after it while the refusal lasts, where
  it was the error of the polls after a program's first schedule
  ([ADR 0168](docs/decisions/0168-add-a-services-counters-and-its-publication-to-its-member-when-it-first-opens-its-members-log.md)).
