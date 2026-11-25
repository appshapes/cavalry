# 0157. Take the C ABI to version 7 with a service's timers

- **Status:** Accepted
- **Date:** 2026-11-25
- **Invariants affected:** 1, 2, 3, 6
- **Supersedes:** none. Takes `spec/c-abi.md` and `spec/vectors/c-abi.json` to
  version 7.

## Context

ADR 0151 gave a cluster's service timers: a program schedules and cancels them
while it handles an entry, and is handed each one once, at one position beside
every member. The Rust library has had them since work item 419's pull request 354:
`Replies::schedule_timer`, `Replies::cancel_timer` and `Entry::Timer`.

A program in any other language reaches a service through the C ABI, which was at
version 6 and had no timer. Version 6 said what such a program saw of a timer event
in the log: an entry of no kind it knows, `CAVALRY_CLUSTER_ENTRY_OTHER`, every one of
them (ADR 0159). To keep that true while the Rust library decided which timer
events to hand over, the library carried a switch for the service that the C ABI
makes, which handed it the events the library passes over.

The five clients are generated over the header, so what the header gives a C
program is what each of them can give its own.

## Decision

Version 7 of the C ABI gives a service's program its timers, as the Rust library
has them and under its names.

- `int cavalry_cluster_service_schedule_timer(cavalry_cluster_service_t *service,
  int64_t correlation_id, int64_t deadline)` and `int
  cavalry_cluster_service_cancel_timer(cavalry_cluster_service_t *service, int64_t
  correlation_id)`, for a call from inside the service's handler, as
  `cavalry_cluster_service_reply` and `cavalry_cluster_service_action` are: a call
  from anywhere else is −1 with `CAVALRY_ERR_CLUSTER_NO_ENTRY`.
- A schedule answers a timer code, `CAVALRY_CLUSTER_TIMER_SCHEDULED`, 1, or
  `CAVALRY_CLUSTER_TIMER_REFUSED`, 2, where the service holds its 1,024 pending
  timers and the id is none of them. A cancel answers 0.
- A timer that is handed over is an entry of a kind of its own,
  `CAVALRY_CLUSTER_ENTRY_TIMER`, 7, after the kinds of version 6, with the timer's
  id in a new field, `correlation_id`, at the end of
  `cavalry_cluster_entry_values_t`. The fields of version 6 keep their places, and
  the struct is 56 bytes where it was 48.
- A timer event that the library passes over reaches no program, in C as in Rust.
  The switch is gone from the library.

## Alternatives considered

**The timer's id in `session_id`.** The struct would keep its size. A timer has no
session, and a program that reads `session_id` of every entry, to route a reply,
would read a timer's id as one. A field that says what it holds costs eight bytes
of a struct that a program fills on its own stack.

**A place for the new field among the others.** After `session_id`, where an `i64`
sits without padding. Every field of version 6 after it would move. At the end the
old fields stay where a reader of version 6's page expects them, and the seven
bytes of padding after `timed_out` were there already.

**−1 with an error code for a refused schedule.** It is how most calls of the ABI
say that they did nothing. A full table is not the caller's mistake and not a
failure of the cluster: it is an answer that a correct program handles at every
call, beside every member alike, as a reply's `CAVALRY_CLUSTER_REPLY_NO_SESSION`
is. A reply code is the ABI's form for that, so a timer code is too, and −1 stays
for a null and for a call from outside the handler.

**Keep handing a C program every timer event as `_OTHER`.** A program would then
be handed its own timer twice, once as `_TIMER` and once more for each event the
library passes over, and would have to tell them apart by keeping the table the
library keeps. That every service decides the same from the log is the rule of ADR
0151, and a program that decides for itself is outside it.

**Give the calls and leave the entry's kind at `_OTHER`, within version 6.** No
struct would change. A program could not tell a timer from a suspend, nor one timer
from another. A new function is a new declaration in any case, and a new
declaration is a new version.

## Evidence

- `spec/vectors/c-abi.json`, written by `spec/vectors/c-abi.py`: 98 functions and
  106 defines, the struct with its eighth field; `cavalry-ffi`'s conformance test
  holds the committed header to it, and `clients/generate-test.py` holds the
  clients' parse of the header to it.
- `crates/cavalry-ffi/tests/cluster_service.rs`,
  `a_program_schedules_a_timer_and_is_handed_it_once_beside_every_member`: three
  members over real drivers and a service through the ABI beside each. A timer is
  handed to every service once, as `_TIMER` with its id, at one position, at a time
  no earlier than its deadline; a timer scheduled and cancelled in one handling is
  handed to none; every other entry's `correlation_id` is 0; and of 1,025 schedules
  the 1,024th answers `_SCHEDULED` and the 1,025th `_REFUSED`, beside every member.
  `a_service_refuses_what_the_page_says_and_outlives_its_client` holds the two
  calls' refusals, a null and a call from outside the handler.
- `conformance/c/cluster.c`, the C runner's cluster round: each service's program
  schedules a timer at the message it takes, and all three are handed it once, with
  its id, at one position and no earlier than its deadline.
- The rule itself is held by `crates/cavalry-raft/tests/timer_search.rs` and by
  `just replay-check`'s timers; the two calls here reach the same `Replies`.

## Consequences

- A program built against version 6 is refused at its first call with
  `CAVALRY_ERR_ABI_VERSION` until it is rebuilt, and a generated client's package
  of version 6 refuses a version 7 library when it loads until it is regenerated.
  Nothing is released (ADR 0003), so no built program meets either. The five
  packages are regenerated in this change: their bindings hold the new functions,
  the new defines and the struct's new field, and none of them wraps a timer yet.
  Each client's own change gives its programs the calls.
- A program of version 6 was handed every timer event as `_OTHER`. A program of
  version 7 is handed the timers that are its own, as `_TIMER`, and no timer event
  besides. A program that scheduled no timer is handed none, where it was handed
  whatever timer events its log held; a handler that matched the kinds it uses and
  passed over the rest sees no difference.
- A program that handles `_TIMER` must take or abort the entry as it does any
  other: an aborted timer is handed over again, the same handing.
- The per-entry cost is the library's, one compare for a program that schedules no
  timer. The two calls allocate nothing and wait for nothing.
- `cavalry_cluster_entry_values_t` is the first struct of the ABI to grow. A
  client that sized it by hand, and none does, would have to follow.
- A reason to revisit: a program that needs to read the timers it holds, their
  count or their deadlines, which the Rust library does not give either.

## References

- Work item 419 on the Brigade Board, which this decision was made in.
- ADR 0151, the timers' rule; ADR 0158, the order of two timers due together; ADR
  0159, what version 6 said of a timer event; ADR 0146, version 6; ADR 0123, the
  cluster service in C.
- `spec/c-abi.md`, "Functions", "Result codes", "Constants" and "Versions and what a
  reader rejects"; `spec/cluster.md`, "The service", "Timers".
