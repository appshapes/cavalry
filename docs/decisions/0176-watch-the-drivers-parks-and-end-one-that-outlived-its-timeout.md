# 0176. Watch the driver's parks, and end one that outlived its timeout

- **Status:** Accepted
- **Date:** 2026-11-26
- **Invariants affected:** 3, 4, 5; the idle path of 2, and nothing a message passes
  through
- **Supersedes:** none. It decides what ADR 0171 left open for a driver: what gets a
  thread back while its program runs.

## Context

ADR 0171 found that on one machine, a Mac on macOS 26.6.1, a timed wait of 1 ms is,
about once in 38 million, never ended by its timer. The thread is sound and returns
at once when something else wakes it. That decision made a stop wake every thread it
joins. It left the time before a stop: a thread whose park loses its wake-up does no
work until something unparks it, and nothing unparked a driver's conductor or its
recorder at all, or its sender, receiver and replayer until the conductor's next
request to them.

What that costs was seen the same day. A test process that had stood on the laptop
for 31 hours, a driver and two clients with seven waiting threads, was sampled twice
four seconds apart: one client's conductor, the driver's sender and its receiver each
stood in `nanosleep`, and the replayer in a timed wait on a semaphore, each in one
stack in every sample. Four of seven threads were held. A driver left running on
such a machine goes silent one thread at a time.

A held conductor is noticed from outside in 10 s, by its clients' `driver-timeout`.
A held recorder, sender, receiver or replayer is not noticed by anything.

## Decision

A driver on macOS has a sixth thread, the watch, which ends a park of the other five
that outlived its timeout. Each of the five stores, before it parks, the time on the
nano clock at which the park is due to end, and clears it when the park has
returned: the park of at most 1 ms of its idle strategy's backoff, and the park of
1 s of a sender, a receiver or a replayer that holds nothing to do. The watch parks
for 100 ms at a time. After each park it reads the five due times, unparks a thread
whose park is more than 200 ms past due, and counts it in a new static counter,
`driver.parks_overdue`.

The watch's own park can be lost as any other can. It stores its due time too, and
the conductor reads it on each idle pass, one load and one compare, and unparks the
watch when it is more than 200 ms past due, counting it in the same counter. So each
of the two gets the other back. A lost wake-up then costs its thread a stall of at
most 300 ms past its park's timeout, the grace and one period of the watch, while
the watch's own park is on time. A park is counted once, however many looks it takes
to end: a conductor that spins sees the watch overdue many times before the watch
has woken.

What is not ended: the conductor's park and the watch's lost together, within the
300 ms in which one would have ended the other. That leaves the driver as ADR 0171
describes it, and is the product of two rates of one in tens of millions. The
watch's park lost together with another thread's is ended in two steps, the
conductor's of the watch and then the watch's of the thread, about 600 ms in all.
And a conductor that finds work on every pass looks at the watch on none of them: a
watch whose park is lost in such a stretch leaves the other four unwatched until
the conductor next idles.

The watch is on where a timed wait has been seen to outlive its timer, which is
macOS, and off elsewhere: on Linux 381 million waits showed none, and a thread and
two stores for each park would be spent for nothing. Off means no sixth thread and no
stores. The choice is made by the system the driver is built for and is no
configuration key. The crate's own tests turn it on and off on every system, so the
pull request's check, which runs on Linux, runs the watch.

The counter counts a park that was late as well as one that was lost: the watch
cannot tell a park that is 200 ms overdue from one that would never return. The
grace is set against how late a sound park gets. In 1.8 billion waits of at most
1 ms, on the laptop and on both of CI's runners, the longest wait seen to return by
itself took 53 ms, once, on the laptop while the gates ran beside the probe; without
them the longest was 20 ms. A grace of 50 ms, the first tried, would have counted
that wait as lost. `doctor` says both readings: a few counts a day on a Mac are lost
wake-ups and need nothing done, and many a minute are a host that could not run the
driver's threads for 200 ms at a time.

The thread-start hook of `Driver::start_with_threads` does not run on the watch. It
is on no message's path, it parks nearly all the time, and `DriverThread` keeps its
five names.

Not decided here, and still as ADR 0171 left them: a client's conductor, which has
no second thread of its client to watch it; the bound of a stop's join; and the
sleeps on a caller's own thread. Each is a later change of work item 338.

## Alternatives considered

- **Have the five unpark each other in a ring.** Lost: every loop would carry the
  period's check and a sibling's thread, on five paths in place of one, and a thread
  with nothing to do parks for a second, so its neighbour would wait that long.
- **A watch with nobody watching it.** Lost: its park is a timed wait as any other,
  and once it is lost the driver is back where it was, with a counter that says
  nothing.
- **Unpark all five every period and count nothing.** Lost: invariant 4. The
  pathology would be cured and unseen, `doctor` could not say that a host has it,
  and each of five threads would take one more duty cycle ten times a second for
  ever.
- **Count each thread's turns and call a thread held when its count stands still.**
  Lost: a store on every duty cycle of every loop, where the due time costs two
  stores for each park, on the idle path alone; and a thread that parks for a second
  stands still for a second by design.
- **Turn the watch on everywhere.** Lost for now: no other system has shown the
  fault, and the cost would be paid by every Linux driver. The Revisit says what
  turns it on.
- **A configuration key for the watch.** Lost: nobody could say when to set it
  (invariant 5). The system decides.
- **A longer grace, or a shorter.** 200 ms is near four times the longest sound wait
  seen. 50 ms, the first tried, is below it: it would count the laptop's own
  lateness while its gates run. A second would leave a held conductor silent for a
  tenth of the 10 s after which its clients give the driver up.

## Evidence

- **The tests**, in `cavalry-driver`:
  - `the_watch_ends_a_park_that_outlived_its_timeout_for_each_thread`: five drivers
    with a watch, in each of which one thread alone parks for an hour where it would
    park for its timeout. The counter rises in each, so the watch unparked that
    thread.
  - `the_conductor_ends_the_watchs_park_when_it_outlived_its_timeout`: the watch's
    first park lasts an hour and no other thread is held. A watch that is parked
    counts nothing, so the count is the conductor's.
  - `a_driver_without_a_watch_counts_no_overdue_park_though_a_thread_is_held`, and
    `close_ends_every_thread_though_no_park_would_time_out`, which now holds all five
    of the stop's unparks, since a park of a second is held for an hour too.
  - `a_park_is_overdue_only_past_its_due_time_and_the_grace_and_only_while_parked`.
- **`doctor`**: `a_park_the_watch_ended_is_explained_from_its_counter`, in
  `cavalry-control`.
- **How late a sound park gets.** From the probe's runs of ADR 0171, the longest
  single wait that returned by itself: 15.9 ms on the laptop alone in its first run
  and 11.6 ms in its third, 19.1 ms on CI's macOS runner and 6.7 ms on CI's Linux
  runner. In the laptop's second run, with `just test` beside it, the longest grew
  from 9 ms to 20 ms in the first nine minutes and to 52.6 ms in the four after,
  in a `nanosleep` that no signal had reached. A wait that stuck in those runs is
  not in these figures: it did not return until it was woken at 30 s.
- **A driver with the watch, left idle on the laptop**: IDLERUN
- **The cost of a held thread without it**: the process of 31 hours, in Context,
  whose samples are with work item 338.
- Twenty runs of the tests that wait on a clock, on both of CI's runners, are in the
  pull request's body with their run's id.

## Consequences

Easier: a driver on a Mac keeps working through a lost wake-up, and says so in a
counter. A test over a driver no longer waits for work that a held thread of the
driver would never do.

Harder: one more thread in a driver on macOS, and a counter whose count has two
meanings, which its `doctor` sentence must keep saying. A reader of a sample sees a
sixth thread, `cavalry-watch`, which the how-to names.

Not covered: a client's conductor is still held until its client's close, and a
driver times that client out after 10 s. A caller's own thread in a sleep is still
held without a bound.

To maintain: `watch.rs`, the two constants, and the counter's two sentences.

Revisit to turn the watch off: when the probe of ADR 0171 runs clean on the laptop
that showed the fault, after an update of its system, for one billion waits, which
at one in 38 million would show 26. Then the watch has nothing to end, and the
how-to says since which version. Revisit to turn it on elsewhere: at one sighting on
another system of a thread held in a timed wait, by two samples as the how-to takes
them, or of one stuck wait in the probe there. Revisit the grace when
`driver.parks_overdue` rises on a machine where the probe is clean: then lateness
reaches 200 ms there, and the count is not telling the two apart.

## References

- Work item 338, with the probe's runs and the account of the process of 31 hours.
- ADR 0171; `docs/how-to/find-a-wait-that-does-not-return.md`.
- `crates/cavalry-driver/src/watch.rs`, `idle.rs`, `lib.rs`; `counters.toml`,
  `driver.parks_overdue`.
- ADR 0011 (a slot of its own for a value one thread writes and another reads).
