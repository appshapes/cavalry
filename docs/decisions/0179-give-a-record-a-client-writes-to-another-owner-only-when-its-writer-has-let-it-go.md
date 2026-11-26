# 0179. Give a record a client writes to another owner only when its writer has let it go

- **Status:** Accepted
- **Date:** 2026-11-26
- **Invariants affected:** 1, 3, 4, 6; the control plane of 2, and nothing a poll that
  moves a position passes through
- **Supersedes:** ADR 0012 on one point: the bytes of a value slot after the value
  are no longer all reserved. The first eight are the release word, and
  `spec/counters-file.md` is at version 1. ADR 0062 on two points: a client's counter
  that is freed is no longer "reused after the counter reuse timeout as any record
  is", but only once its writer has let it go; and its Consequence that a client that
  was only slow "may store into a record that was freed", "bounded the same way by
  the counter reuse timeout", no longer holds, since such a store lands in a record
  that is still that client's. The rest of both stands, and ADR 0078's keeping of
  what an `available image` names is unchanged. It also ends the order of an upgrade
  on one host that `spec/client-protocol.md` kept until version 10, as Consequences
  says.

## Context

Two kinds of counter are written by a client and not by the driver: an image's
position, which a subscriber stores after each poll, and a counter the client added
with `add counter` (ADR 0062). The driver frees such a record when the image ends,
when the subscription or the counter is removed, and when the client closes or is
timed out. Until this decision it gave a freed record to the next allocation once the
counter reuse timeout, 1 s, had passed, the first free record first.

A thread of the client can store later than that. A subscriber's thread learns that
an image ended, or that its client was timed out, from an event that the client's
conductor reads on another thread. A process that was stopped stores when it is
continued. Work item 506 reproduced both on `master` at 9eab890f:

- **A stopped client.** A subscriber's process was stopped with `SIGSTOP` for 11 s.
  The driver timed it out and freed its position's record, record 4, and a second
  client's next `add counter` was given record 4. Continued, the first process made
  298 polls before its conductor read `client timeout`, and each stored its position.
  The second client's counter read 64,000, that position, in five runs of five.
- **A slow conductor.** A subscriber whose conductor did not run for longer than the
  counter reuse timeout, while one publication of its stream ended and the next
  began. The next image's position was allocated in the record of the ended one. The
  subscriber's thread, not yet told, stored the ended image's position, 268,435,456,
  into it. The driver read that as the new image's subscriber position and set the
  publisher's limit to 301,989,888 where 33,554,432 is right. The publisher wrote 288
  MiB with nothing read. The subscriber then read three terms of it and no fault:
  98,304 messages lost, and nothing said so.

The second is invariant 1 broken with no report. Every guard in the way was a time,
and a time is no bound on a thread that is not run.

Every store that a client's process makes into a record, outside its tests, at this
decision's commit. Each goes through one of four methods of `cavalry-shm`'s `Counter`:
`increment`, `add`, `set` and `store_release`.

| Record | Stored in | By which thread | Its handle, and whether it can be shared |
| --- | --- | --- | --- |
| An image's position, `subscription.position` | `cavalry-client`, `subscription.rs`: `poll` (line 387), `controlled_poll` (508), `bounded_controlled_poll` (573), `bounded_poll_at` (646) | The thread that polls the subscription | In the image, which its subscription holds and polls by `&mut`: one thread at a time. The conductor takes it in `announced` and hands it over a queue |
| The conductor's own counter, `client.conductor_parks_overdue` | `conductor.rs`, `publish_overdue` (501) | The client's conductor | In the conductor, which no other thread reaches |
| A counter the client added: a cluster session's `cluster.replies_stale` | `cluster.rs`, `ClusterSession::poll` (760) | The thread that polls the session | The crate's `Counter`, which is not `Clone` and sends `remove counter` when it is closed or dropped |
| A cluster service's counters | `service.rs`: `Replies::reply` (1113, 1151, 1161), `restore` (1425, 1429, 1430), `store_counts` (1959 to 1961), `snapshot` (2160, 2172, 2190, 2195) | The thread that polls the service | The same |
| A cluster member's counters | `cavalry-raft`: `module.rs`, `Stored::add` (2760) and `Stored::store` (2771); `timers.rs` (281, 351); `toggle.rs` (61, 143, 144); `snapshot.rs` (236, 350) | The member's consensus thread | The same |
| `cluster.control_toggle`, for `cavalry snapshot` | `crates/cavalry/src/snapshot.rs`, `wait` (374) | The command's own thread | The same |

A publisher stores into no record: it reads its limit and writes its log, a file of
its own that is given to nobody else. `cavalry-ffi` stores nothing itself: its
functions reach the rows above on its caller's thread. Before this decision the
image's handle and the conductor's were `cavalry-shm`'s `Counter`, which is `Clone`,
and nothing in the crate cloned one. A store into a counter the client added first
checks that the client is open, with one load. The check and the store are two steps,
so the check was never a guard: a thread can be stopped between them.

Aeron has the same shape. Read from its source on 2026-11-26: `Image.poll` stores the
position with `subscriberPosition.setRelease` after a check of the image's own
`isClosed` flag, with no check that the counter is still the image's; the client's
conductor closes images on another thread, with no handshake with the thread that
polls; the driver waits for nothing from the client before it frees a counter; and a
freed counter is reused after `aeron.counters.free.to.reuse.timeout`, 1 s by default.

## Decision

**A record whose value a client writes is given to another owner only when its
writer has said that it will not store again, or when the client is gone.** The
driver frees the record when it does today, so that no reader takes it for a counter,
and holds it: it sets no reuse deadline. It lets the record go, and counts the counter
reuse timeout from that moment, in two cases.

1. **The release word.** Each value slot of the counters file gains a word after the
   value, in bytes that were reserved. The one thread that holds the client's handle
   on the record stores 1 there, with a release store, as its last store to the slot,
   when it gives the handle up. The driver loads it with an acquire load. A
   subscription stores it on the thread that polls, between two polls, when it takes
   an image out; a counter's owner when it removes the counter; every handle when it
   is dropped. For a record the client was told of and never took a handle on, an
   image it does not add or one announced for a subscription it has removed, its
   conductor stores the word by the record's id. So the thread that stores and the
   thread that says so are one, and no second thread is in the argument.
2. **The client's lock.** A client creates `clients/<client id>.lock` in the driver's
   directory before its first command and holds an exclusive advisory lock on it, as
   the driver holds one on the directory (ADR 0079). Everything of the client that can
   store into a counter shares the one open file, so the lock is let go when the last
   of them is dropped, and by the system when the process ends. When a client closes
   or is timed out the driver gives the file the name `clients/<client id>.gone`,
   tries its lock ten times a second, and lets every record held for the client go
   when it takes it.

In `cavalry-client` a handle on such a record is one type, `Owned`. It is not
`Clone`, it stores the word in its drop, and nothing else in the crate holds a
counter's writing handle. `cavalry-shm`'s own `Counter` stays `Clone` for the driver.

A client that cannot take its lock attaches all the same. Its release words are read
as any client's. What it leaves held with no word when it closes or is timed out is
out of use until the driver starts again.

The counters file goes to version 1, since a reserved field became a field, and
`spec/client-protocol.md` to version 11. A reader of either side rejects the other's
counters file with `version` when it attaches, before it sends a command, and the
message says which side is the older and what to do. So no client of an earlier
build stores into a record of a driver of this one, and this decision carries no rule
for one.

Two counters. `counters.held`, a gauge: the records freed and held. `counters.retired`:
the records out of use until the driver starts again. `cavalry doctor` names each
client that has gone and still holds its lock, by its `.gone` file, once it has held
it for 2 s.

## Alternatives considered

- **Compare before the store.** A store that succeeds only when the cell holds the
  writer's last value. It narrows and does not close: in the second case the stray
  writer stores over the driver's 0 only if the values differ, two readers of one
  stream in step hold the same value, and it puts a locked instruction on every poll
  that moves.
- **An owner word that the writer checks before it stores.** The thread can be
  stopped between the check and the store. A narrower window is still a time.
- **The mark as −1 in the value itself.** No layout change. It lost because a
  position would then be read as a position everywhere but in the driver's flow
  control, its cleaning, the receiver's readers and `doctor`, each of which would
  need the exception; because the driver's bounds check on a subscriber's position
  evicts the client for a negative one; and because a counter an application added
  may hold any value, −1 among them, and would have no mark.
- **An answer from the client as a command**, sent when its conductor has read the
  event. It lost because the conductor is not the thread that stores: a poll inside a
  handler that takes two seconds stores when the handler returns, after the conductor
  answered. An answer sent on the subscription's own word would be right, and is what
  the release word is, with no message and no queue between two threads.
- **A shared lock on an image's log, held by each subscriber.** No word and no
  message. It lost on its grain: the driver would learn only that no subscriber
  holds the log, so one process that is stopped would keep every other subscriber's
  record of that image held.
- **A longer time.** Any time is passed by a process that is stopped for longer.
- **The driver checks what it reads.** A subscriber's position above its publisher's
  is told apart; one below it is not, and a counter a client added is read by nobody
  who could check it.
- **Never allocate a freed record of a client's again.** It closes the hazard and
  spends a record for each image and each counter for the driver's life: a stream
  whose publications come and go would fill the counters file.
- **Keep the counters file at version 0, with a rule for an older client**, whose
  records the driver would put out of use when it goes. It lost because a newer
  client beside an older driver would be as exposed as before, and because the rule
  would be carried for ever for clients that exist nowhere: 0.1.0 is the first
  release.

## Evidence

- **The two reproductions**, each written as what must hold. Both failed on `master`
  at 9eab890f with the figures of Context, and pass with this decision:
  `a_client_that_was_stopped_does_not_store_into_a_record_another_client_owns`
  (`crates/cavalry-client/tests/stray_store.rs`, a child process stopped with
  `SIGSTOP`) and
  `a_slow_conductor_does_not_store_an_ended_images_position_into_the_next_images_record`
  (`conductor.rs`, with a test-only flag that holds the conductor's thread). In the
  second the next image's position now stands at 0 in another record, the
  publisher's limit is its window, 33,554,432, it writes 32 MiB, and the subscriber
  reads every message of it.
- **The other tests**, in `stray_store.rs`:
  - `a_poll_inside_its_handler_stores_into_a_record_that_is_still_its_own`: a handler
    that sleeps across the publication's end, the linger, the free, the counter reuse
    timeout and the next publication's start.
  - `what_a_client_without_its_lock_leaves_unreleased_is_out_of_use`.
  - `every_record_held_for_an_image_is_let_go_whoever_was_told_of_it`: sixty rounds of
    publications and subscriptions that come and go, after which `counters.held` is 0.
  - `a_client_refuses_an_older_drivers_counters_file_before_it_sends_a_command`.
- **`cavalry-shm`**: `a_held_record_is_given_out_only_when_its_writer_has_let_it_go`,
  the version's two messages, and under loom
  `a_driver_that_acquires_the_release_word_sees_the_writers_last_store`.
- **`doctor`**: `a_client_that_has_gone_and_holds_its_lock_is_named_by_its_file`.
- **The upgrade gate** runs a program and the driver it is a client of from one build
  in every round (`conformance/upgrade/run.sh`, "The schedule"), so no round crosses
  this version with a client on one side and its driver on the other. It does run
  the newer build's `doctor` on members of the older build, and passes with the
  read-only open of Consequences: its older build here is 031817e9, whose counters
  file is version 0.
- Twenty runs of the tests that wait on a clock, on both of CI's runners, are in the
  pull request's body with their run's id.

## Consequences

**An upgrade on one host crosses this version with its driver and every client of it
stopped together.** Until version 10 the page said that an upgrade restarts the
clients before the driver, and that a client on a driver of an earlier version is
answered with code 6 and keeps running: a newer client ran beside an older driver for
the length of an upgrade. Across version 11 neither order works. A client of either
side refuses the other's counters file when it attaches, with a message that says
which side is the older, and what to do: stop the driver and every client of it, and
start them from the newer build, the driver first. Between hosts nothing changes: no
frame and no message moves, so a cluster is still upgraded one member at a time, each
member with the driver and the service beside it. `cavalry doctor` of the newer build
still reads a driver of the older one, which that upgrade's steps need: a reader that
only reads, and is no client, also reads a counters file of version 0, since it
stores nothing. The upgrade gate found that this was needed: its first run with this
decision failed at its first step, where the newer build's `doctor` could not read a
member of the older build.

Easier: no thread of a client stores into a record another owner holds, whatever the
client's threads were doing and for however long. A reviewer of a new store in the
client asks one question, whether it goes through `Owned`.

Harder:

- A record is reusable later than before: when its writer has let it go and the
  counter reuse timeout has passed, where it was the timeout alone. After a client's
  own close that is when its objects are dropped.
- Records are held for long by a client that closed and keeps an object of the client
  alive, by a process that was timed out and is stopped, and by a subscription whose
  thread no longer polls, which its client's queue of 64 image events closes. Held
  records count against the counters file's capacity. They are bounded by it, counted
  in `counters.held`, and `doctor` names the first two.
- Each client holds one more open file. The driver makes a system call at a client's
  first command, and three every 100 ms for each client that has gone and still holds
  a record, on its conductor's thread, on the control plane.
- A caller of the C ABI that uses a handle from a second thread while the first
  closes it stores after the word. That was the caller's fault before, and the page
  now says so.

Not closed by this: a writer that never took part. A program that maps the counters
file writable and stores into a record that was never its own is outside it, as it
is outside Aeron's.

To maintain: the release word and the held state in `cavalry-shm`; the driver's held
records and its tries at the locks; `Owned` and the client's lock in
`cavalry-client`; `doctor`'s check.

Revisit when a client's handle must be shared between threads: then the word is
stored when the last holder lets go, which a count would say. Revisit the lock where
a driver's directory is on a filesystem that keeps no advisory locks: `counters.retired`
above 0 is the sign.

## References

- Work item 506, with both reproductions' figures, and with the owner's ruling of
  2026-11-26 on the counters file's version, Brigade decision d-20261126-b8a6ee: the
  header goes to version 1, and across this change a host's driver and all its
  clients stop and are upgraded together. Issue 274, which is not this: its
  `Overrun` is a client's queue of image events found full.
- ADR 0012 and ADR 0013 (the counters file and a value's one writer), ADR 0062 (a
  client's counters, and the hazard it named), ADR 0078 (what an `available image`
  names is kept), ADR 0079 (the driver's lock), ADR 0136 and ADR 0141 (the upgrade
  gate).
- `spec/counters-file.md`, version 1, "Metadata record" and "Value slot";
  `spec/client-protocol.md`, version 11, "The records a client writes".
- `crates/cavalry-shm/src/counters/`; `crates/cavalry-driver/src/conductor.rs`, `hold`,
  `let_go_released`, `try_locks` and `client_gone`; `crates/cavalry-client/src/owned.rs`,
  `subscription.rs`, `conductor.rs`.
- Aeron: `aeron-client/src/main/java/io/aeron/Image.java` and `ClientConductor.java`,
  `aeron-driver/src/main/java/io/aeron/driver/Configuration.java`, and Agrona's
  `org/agrona/concurrent/status/CountersManager.java`, at their `master` on 2026-11-26.
