# 0177. Watch a client's conductor, and count its overdue parks in a counter the client adds

- **Status:** Accepted
- **Date:** 2026-11-26
- **Invariants affected:** 3, 4, 5; the idle path of 2, and nothing a message passes
  through
- **Supersedes:** none. It decides for a client what ADR 0176 decided for a driver,
  and what ADR 0171 and ADR 0176 both left open.

## Context

On one machine, a Mac on macOS 26.6.1, a timed wait of 1 ms is, about once in 38
million, never ended by its timer, and its thread returns at once when something
else wakes it (ADR 0171). A driver there has a watch, a sixth thread that ends a
park of the other five that outlived its timeout (ADR 0176).

A client has one thread of its own, its conductor. It sends the keepalive, takes
the images the driver announces and hands them to the subscriptions, and answers the
commands the application's thread waits on. It idles by the same backoff, so on such
a machine its park is lost at the same rate: about once in eleven hours. A conductor
that is held does none of that. Its driver times the client out after 10 s and
removes its publications and subscriptions, and the application learns of it at its
next call. In the test process that stood on the laptop for 31 hours, one of the
two clients' conductors was among the four threads held.

Nothing in a client's process could wake that thread before its close.

## Decision

A client on macOS has a second thread, a watch, and its conductor and the watch
each end a park of the other that outlived its timeout. The conductor stores, before
it parks, the time on the nano clock at which the park is due to end, and clears it
when the park has returned. The watch parks for 100 ms at a time and unparks a
conductor whose park is more than 200 ms past due. The watch stores its own due
time the same way, and the conductor reads it on each idle pass, one load and one
compare, and unparks a watch that is more than 200 ms past due. The figures are the
driver's (ADR 0176). A lost wake-up of the conductor costs a stall of at most 300 ms
past its park's timeout, while the watch's own park is on time. A keepalive that
was due in that time goes out that much late, far inside the 10 s after which the
driver times a client out: the driver sees nothing. Not ended: both parks lost
within the 300 ms in which one would have ended the other, the product of two rates
of one in tens of millions, after which the client is as it was before this
decision.

Each of the two counts a park it ends once, by its due time, into a number in the
process. The conductor publishes that number in a counter of scope `client`, which a
client adds with `add counter` and alone writes (ADR 0062): a new counter,
`client.conductor_parks_overdue`, keyed by the client's id. The conductor adds it
itself, with a command of its own on its own producer, when the number first leaves
0, and not before: a client that never has an overdue park adds nothing and costs
the driver no record. It stores the count with a release store whenever it has
moved. A conductor whose client is closed sends no such command.

A driver that has no counter left refuses the conductor's, as it refuses any
client's. The park was ended all the same. The conductor then keeps the count in
the process and does not ask again: it asks once in a client's life, it reads the
refusal as it reads any event, and it neither waits on it nor tries again at each
pass. `doctor` then names no such client, and what it shows is the driver's own
count of the refused add, `driver.registry_full` or `counters.allocation_failed`,
with what to do about a driver that is full. The same holds for a driver that
names a record the client cannot open; that record stays the client's until its
close.

The driver frees the counter when the client closes or is timed out,
as it frees every counter of a client, so `doctor` names a client by it for as long
as the client lives: `doctor` reads every such counter and gives a finding for each
that is above 0, with the client's id.

The watch is on where a timed wait has been seen to outlive its timer, which is
macOS, and off elsewhere, as the driver's is and for the same reason. Off means no
second thread, no stores, and no counter. The crate's own tests turn it on and off
on every system. It is no configuration key and no part of the client's API: a
program that links a client does nothing to have it.

The stop is as ADR 0171 made it: a close stores the closed state, unparks the
conductor and joins it, and now unparks and joins the watch after it. A watch that
cannot be started fails the client's start, with the conductor ended.

## Alternatives considered

- **A rescue from the caller's own calls, with no thread.** The application's
  thread is the only other thread a client's process is sure to have. A look at the
  conductor's due time from `offer`, `try_claim` or `poll` would end the park at the
  next message, and would put a read of the clock and a load on the path of every
  message, which invariant 2 forbids. A look from the control calls alone,
  `add_publication` and the like, costs nothing there, but an application that only
  offers and polls makes none: its conductor stays held, and after 10 s its driver
  times the client out. That is the case that matters, an application left to run,
  and it is the one this alternative does not reach. A thread ends the park within
  300 ms whatever the application does, and costs one parked thread in each client
  process on macOS.
- **Have the driver wake a client's conductor.** Lost: the driver cannot unpark a
  thread of another process, and a signal needs a handler, which is the
  application's to install and not a library's.
- **Count in the driver's own counter, `driver.parks_overdue`.** Lost: a counter's
  value is written by its owner alone (ADR 0013), and that one is the driver's. A
  client's count would also read as the driver's threads being held.
- **Add the client's counter at its start.** Lost: a record of the driver's
  counters file and a command for every client, on every start, to count what
  nearly no client ever has.
- **Count nothing for a client.** Lost: invariant 4. A conductor that stalled for
  300 ms took no image in that time, and nothing would say why a subscription
  connected late.
- **One watch for the process's clients.** Lost for now: a process with several
  clients is rare, the watch parks nearly all the time, and a thread shared by
  clients would need a registry and a lifetime of its own.

## Evidence

- **The tests**, in `cavalry-client`:
  - `the_watch_ends_the_conductors_park_and_the_conductor_counts_it_in_its_own_counter`:
    over a real driver, a client whose conductor's first park lasts an hour, which
    stands for the park whose timeout never comes. The client has no counter until
    the park is ended; then its counter is in the driver's counters file with 1,
    which only a conductor that went on can have added and stored. It stays 1 for
    half a second, the client takes `add publication` as before, and the driver
    frees the counter at the client's close.
  - `the_conductor_ends_its_watchs_park_and_counts_it_once`: the watch's first park
    lasts an hour and the conductor is not held. A watch that is parked counts
    nothing, so the count of 1 is the conductor's.
  - `close_ends_the_conductor_though_its_park_would_not_time_out`, from ADR 0171,
    now on a client with no watch: it adds no counter, and its close still ends a
    conductor parked for an hour.
  - `a_driver_with_no_counter_left_refuses_the_conductors_own_and_is_asked_once`: a
    second client adds counters until the driver refuses one, and then a client's
    held conductor is woken. The driver's two counters of a refused add move by one
    between them and stay there for half a second, the client has no counter, and
    it takes `add publication` as before.
  - `an_overdue_park_is_counted_once_however_many_looks_it_takes`.
- **`doctor`**:
  `a_clients_conductor_that_its_watch_woke_is_explained_from_the_clients_counter`,
  in `cavalry-control`: one client above 0 is named with its id, one at 0 is not.
- **The rate and the grace** are ADR 0171's and ADR 0176's: one wait in 38 million
  on the laptop, and a sound park at most 53 ms late in 1.8 billion waits.
- Twenty runs of the tests that wait on a clock, on both of CI's runners, are in the
  pull request's body with their run's id. The first such run failed, at the first
  test's last line: it read the counter as freed as soon as the client's close
  returned, and the driver frees it a duty cycle later. The test waits for it now.
- No client was left running for an hour as a driver was for ADR 0176: a client's
  conductor idles by the same backoff and is held to the same grace, and that
  hour's count of 0 is the evidence for both.

## Consequences

Easier: an application left running on such a Mac keeps its client. A client whose
conductor was woken says so where `doctor` reads it, by its id.

Harder: every client process on macOS has one more thread, `cavalry-client-watch`.
A conductor now sends a command of its own, once, which until now only the keepalive
was. The count of a client that has closed is gone with its counter.

Not covered, and still as ADR 0171 left them: the bound of a stop's join, and the
sleeps on a caller's own thread, in which the application's thread can be held
inside a call of the client's.

To maintain: `watch.rs` in `cavalry-client`, a second copy of the driver's slot and
of its two figures, since the client does not link the driver; the conductor's
`add counter`; `doctor`'s check.

Revisit with ADR 0176's clauses, which turn both watches off or on together: off
when the probe of ADR 0171 runs clean for a billion waits on the laptop after a
system update, on elsewhere at one sighting. Revisit the copy when a third program
needs the slot: then it belongs in a crate both link.

## References

- Work item 338, with the plan of this change and the account of the process of 31
  hours.
- ADR 0171, ADR 0176; ADR 0062 (a client's counters); ADR 0013 (a counter's owner).
- `crates/cavalry-client/src/watch.rs`, `conductor.rs`, `client.rs`;
  `counters.toml`, `client.conductor_parks_overdue`;
  `crates/cavalry-control/src/doctor.rs`.
- `docs/how-to/find-a-wait-that-does-not-return.md`.
