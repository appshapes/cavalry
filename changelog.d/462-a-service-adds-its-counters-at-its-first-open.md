- A cluster service now adds its five counters and its publication to its member when
  it first opens its member's log, and no longer in the poll in which its program
  first schedules a timer. That poll waited tens of milliseconds for the driver, with
  the entries behind it waiting too: the publication's log is 192 MiB, which the
  driver makes resident as it creates it. A program's first timer now costs what a
  later one costs. Every service holds those counters and that log from its first
  open, a program that schedules no timer included, and a driver that refuses one of
  them is the error of a service's first polls, where it was the error of the poll of
  its first schedule
  ([ADR 0168](docs/decisions/0168-add-a-services-counters-and-its-publication-to-its-member-when-it-first-opens-its-members-log.md)).
