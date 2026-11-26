# 0171. Park the idle loops where a stop can wake them, and wake every thread at a stop

- **Status:** Accepted
- **Date:** 2026-11-26
- **Invariants affected:** 3, 5, 6; the idle path of 2, and nothing a message passes
  through
- **Supersedes:** none

## Context

Four times in eleven days a test run on a laptop did not end. Each time one thread
stood in the join of another, and the other was in one wait of at most a millisecond
in every sample taken: twice a driver's thread after its stop, the first a sender on
2026-11-15, and twice a client's conductor after `Cavalry::shutdown`, on 2026-11-22
and on 2026-11-26. The runs were stopped by hand after 8 to 46 minutes. Since 2026-11-23 the laptop's gates are taken one session at
a time, so each such run held every session's gates. Work item 338 and GitHub issue
245 have the stacks.

The code said the join ends at once. A stop stores a flag, each loop reads the flag
every pass, and a pass that found no work waits for at most 1 ms. The wait was
`std::thread::sleep`, in the driver's `Idler::idle` and in the client conductor's
backoff. `Driver::stop` unparked the replayer, the sender and the receiver, which
park while they hold nothing to do; but each of those sleeps, and does not park,
while it has something, and an unpark does not reach a sleep. Nothing woke the
driver's conductor, its recorder, or a client's conductor.

A probe with no Cavalry code in it, `scripts/sleep-probe.rs`, found the cause on
2026-11-26. On that laptop, macOS 26.6.1 on Apple silicon, one timed wait of 1 ms in
20 to 34 million is not ended by its timer. The thread is sound: it returns at once
when a signal or an unpark wakes it. Every kind of timed wait tried shares the
fault, so no other call is a remedy. The same probe found nothing on CI's Linux
runner or on CI's macOS runner, macOS 26.3 in a virtual machine. Evidence has the
counts.

## Decision

A thread of Cavalry that idles waits where its stop can wake it, and a stop wakes
every thread it then joins. The driver's backoff, `Idler::idle`, and the client
conductor's backoff wait in `std::thread::park_timeout` where they waited in
`std::thread::sleep`, for the same 1 µs doubling to 1 ms. `Driver::stop` stores the
stop flag and then unparks all five threads, the conductor and the recorder with the
three it woke before, and joins them. A client's close stores the closed state and
then unparks its conductor, and joins it. So no join waits on an idle wait's
timeout: the flag is read by a thread that the unpark has woken, or that was not
parked.

What this does not decide is what gets a thread back while its program runs. A park
that loses its wake-up holds its thread until something unparks it, and such a
thread does no work. The driver's conductor unparks the sender, the receiver and the
replayer with every request it gives them, so one of those comes back with the next
request. Nothing unparks the driver's conductor, its recorder or a client's
conductor before a stop. A driver whose conductor is held answers no command and
stores no heartbeat, and its clients close with `driver-timeout` after 10 s. A
client whose conductor is held sends no keepalive, and the driver times it out after
10 s and counts it in `client.timed_out`. A recorder that is held writes nothing,
and nothing bounds that. On the laptop measured, a thread that idles by backoff
waits about 940 times a second, which at one wait in 20 to 34 million is one lost
wake-up for each such thread in six to ten hours. In a driver with nothing to send,
to receive or to replay, two threads idle that way, the conductor and the recorder,
and the other three park for a second at a time: one lost wake-up in three to five
hours. With all five at work and idling between, it is one in one to two hours. Work item 338's second
change decides the rescue and its counter, and the bound of a stop's join for a
thread that does not end for any other reason.

The contract is also `Idler`'s, which is public: a loop that idles with it and is
stopped by another thread is stopped by its flag and then an unpark of its thread.
An unpark that comes while the thread is not parked makes its next park return at
once, which costs one duty cycle. `cavalry run`'s member loop idles with an `Idler`
on a thread of its own, and `cavalry run` now unparks that thread after it stores
the member's stop flag and before it joins it; that loop's wait of 100 ms after a
cycle that failed is a park too, which the same unpark ends. The driver's start
unparks a conductor it stops because a later thread could not be started.

Not covered: a wait in `std::thread::sleep` on the caller's own thread. These are a
client's wait for the driver's answer to a command, in steps of 50 µs up to its
deadline, a client's wait to attach, the cluster client's waits, a service's pauses
around a snapshot, the wait of 1 ms between two readings of a recording's catalog
entry whose checksum does not hold, and every test's own sleep. They stay sleeps
because the thread is the caller's: a park there would take an unpark that the
caller's program meant for a park of its own. One of them that loses its wake-up
holds its caller without a bound. Two threads that a stop joins can be in one: the
member thread of `cavalry run`, while its module waits for the driver's answer to a
command or closes its client, and the driver's replayer, while it reads a catalog
entry again. A stop that falls in such a wait, on a machine that loses that
wake-up, still hangs, at a small part of the odds it had. The second change weighs
them with the join's bound.

## Alternatives considered

- **Keep the sleep, and give the join a bound.** Lost as the remedy: at each lost
  wake-up the stop would wait out its bound, leave a thread behind and report an
  error, where an unpark ends the wait at once and costs nothing. The bound is still
  wanted for a thread that does not end for another reason, and is the second
  change's.
- **Another timed wait.** Lost on the evidence: `nanosleep`, `park_timeout`, a
  condition variable's timed wait, `mach_wait_until` and `kevent` with a timeout
  each stuck on the laptop, the last three in the third run. What is lost is the timer's wake-up of a waiting thread,
  below all of them.
- **Never park on macOS: spin or yield.** Lost: a core for each of five threads on a
  laptop, where the default must be right (invariant 5), to avoid a fault that a
  wake-up cures.
- **A thread that wakes the others on a period.** Not here: it answers the running
  case, and its own wait can be lost as any other can. The second change weighs it
  against the threads waking each other.
- **Wake a thread with a signal.** Lost: a signal needs a handler, which is the
  process's and not a library's to install, and an unpark does the same for a thread
  that parks.
- **Change nothing, since Linux and CI's macOS do not show it.** Lost: every session
  works on such a Mac, the laptop is now the first net for macOS (ADR 0170), and
  each hang held all the gates for half an hour. On Linux the change is one wait
  exchanged for another of the same length.

## Evidence

The probe starts threads that each repeat the backoff, a wait of 1 µs doubling to
1 ms, and a main thread that watches them with a park of 200 ms. It prints a thread
whose one wait has lasted 3 s, and after 30 s wakes it. Each run is one process.

| Machine | Run | Waits | Stuck | Of which |
| --- | --- | --- | --- | --- |
| The laptop: an Apple M5 Pro, macOS 26.6.1, kernel xnu-12377.161.13 | 20 minutes, 128 threads | 135 million | 4 | 3 in `std::thread::sleep`; 1 in one call of `nanosleep`, with no signal |
| The same, a `just test` beside it for part of the run | 40 minutes, 192 threads | 412 million | 19 | 8 in `std::thread::sleep`, 4 in `nanosleep`, 7 in `std::thread::park_timeout` |
| The same | RUN3 |
| CI's Linux runner: 4 virtual processors, Ubuntu 24.04, kernel 6.6 | 30 minutes, 192 threads | 381 million | 0 | |
| CI's macOS runner: a virtual machine of 6 processors, macOS 26.3, kernel xnu-12377.81.4 | 30 minutes, 192 threads | 305 million | 0 | |

- In the first run nothing woke a stuck thread, and the first, stuck at 507 s, had
  not returned when the run ended at 1,200 s.
- In the second run no stuck wait returned by itself within 30 s, and all 18 that the
  probe then woke returned within the watcher's next look, 200 ms: the sleepers by a
  signal, the parked ones by `unpark`. The nineteenth stuck in the run's last
  seconds.
- A sample of the process at a stuck wait shows a sleeper in `nanosleep`,
  `__semwait_signal`, the stack of every sighting, and a parked one in
  `semaphore_timedwait_trap`.
- The standard library's loop after an interrupted sleep is not the cause: in two
  short runs a signal interrupted 140,000 sleeps, and every remainder that
  `nanosleep` wrote was right.
- The two runners ran in `sleep probe (trial)` run 45768708805, from a throwaway
  branch, each posting a line off the runner every minute. At the laptop's rate the
  macOS runner would have shown 9 to 15. The run does not say which difference
  matters: the newer system, or the metal against a virtual machine.

The tests: `cavalry-driver`'s
`a_backoff_that_has_parked_returns_when_its_thread_is_unparked` and
`close_ends_every_thread_though_no_park_would_time_out`, and `cavalry-client`'s
`close_ends_the_conductor_though_its_park_would_not_time_out`. Each has a backoff
park for an hour, which stands for the park whose timeout never comes, and ends in
time only when the stop unparks the thread. With the recorder left out of the stop's
unparks, or the client's unpark removed, its test fails after its bound of 20 s. The
driver's test holds the conductor's and the recorder's unparks: its other three
threads have nothing to do and wait in their own park of a second. The three ran
twenty times in a row on each of CI's runners before the pull request, `test times`
run 45773733451.

No message passes through the idle path, so the change adds no work for a message.
The latency gate's next night measures `master` with it, and every row is read as
before (ADR 0139): each of them has threads that idle by backoff between messages.

## Consequences

Easier: a stop of a driver or of a client whose threads idle ends on a machine that
loses a timed wake-up, which is the stop of every sighting, and the laptop's gates
are no longer held by it.

Harder: a thread that is unparked while it is not parked turns once more than it
would have. A reader of a hang now needs two samples to tell a lost wake-up from a
loop that does not end, which `docs/how-to/find-a-wait-that-does-not-return.md`
sets out.

Still true: a thread that loses a wake-up while its program runs is held until
something wakes it, as the Decision sets out, and a test that waits for such a
thread's work fails at its own deadline or waits without one.

To maintain: the probe, which no gate runs. It is one file, built by `rustc` alone.

Revisit when the second change of work item 338 lands: it names what wakes a held
thread while the driver runs, and this decision's third paragraph then describes the
past. Revisit when the night's bench shows a row that moved with this change: then
the park's cost on Linux is measured against the sleep's, on the bench hosts, and
the stop's unpark stays whichever wait the loops then use. Revisit when the probe
runs clean for an hour on the laptop that showed the fault, after a system update:
nothing is undone, and the how-to's first paragraph says since which version.

## References

- Work item 338, with the sightings and the probe's runs; GitHub issue 245.
- `scripts/sleep-probe.rs`; `docs/how-to/find-a-wait-that-does-not-return.md`.
- `crates/cavalry-driver/src/idle.rs`, `crates/cavalry-driver/src/lib.rs`
  (`Driver::stop`), `crates/cavalry-client/src/conductor.rs` (`Backoff`),
  `crates/cavalry-client/src/client.rs` (`Cavalry::shutdown`),
  `crates/cavalry/src/main.rs` (the member's stop in `cavalry run`).
- ADR 0041 (a sender with nothing to send parks), ADR 0139, ADR 0170.
