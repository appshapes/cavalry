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
| An image's position, `subscription.position` | `cavalry-client`, `subscription.rs`: `poll` (line 389), `controlled_poll` (510), `bounded_controlled_poll` (575), `bounded_poll_at` (648) | The thread that polls the subscription | In the image, which its subscription holds and polls by `&mut`: one thread at a time. The conductor takes it in `announced` and hands it over a queue |
| The conductor's own counter, `client.conductor_parks_overdue` | `conductor.rs`, `publish_overdue` (504) | The client's conductor | In the conductor, which no other thread reaches |
| A counter the client added: a cluster session's `cluster.replies_stale` | `cluster.rs`, `ClusterSession::poll` (760) | The thread that polls the session | The crate's `Counter`, which is not `Clone` and sends `remove counter` when it is closed or dropped |
| A cluster service's counters | `service.rs`: `Replies::reply` (1113, 1151, 1161), `restore` (1425, 1429, 1430), `store_counts` (1959 to 1961), `snapshot` (2160, 2172, 2190, 2195) | The thread that polls the service | The same |
| A cluster member's counters | `cavalry-raft`: `module.rs`, `Stored::add` (2761) and `Stored::store` (2772); `timers.rs` (281, 351); `toggle.rs` (61, 143, 144); `snapshot.rs` (236, 350) | The member's consensus thread | The same |
| `cluster.control_toggle`, for `cavalry snapshot` | `crates/cavalry/src/snapshot.rs`, `wait` (374) | The command's own thread | The same |

The release word has stores of its own, and they are in two places. A handle's drop,
`owned.rs`, on whichever thread drops the handle, which is the thread that polls for
an image that is taken out. And the conductor, by a record's id, in `release_unheld`
of `conductor.rs`, for a record that no handle of this client exists on.

A publisher stores into no record: it reads its limit and writes its log, a file of
its own that is given to nobody else. `cavalry-ffi` stores nothing itself: its
functions reach the rows above on its caller's thread. Every handle of the table is
an `Owned` or holds one. Before this decision the
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
   when it gives the handle up, and once: giving a handle up and dropping it are one
   act. The driver loads it with an acquire load. A subscription gives an image's
   handle up on the thread that polls, between two polls, when it takes the image
   out; a counter's owner when it removes the counter. For a record the client was
   told of and never took a handle on, an image it does not add or one announced for
   a subscription it has removed, its conductor stores the word by the record's id,
   and only into a record that holds no counter or is still that image's. So the
   thread that stores and the thread that says so are one, and no second thread is in
   the argument.
2. **The client's lock.** A client creates `clients/<client id>.lock` in the driver's
   directory before its first command and holds an exclusive advisory lock on it, as
   the driver holds one on the directory (ADR 0079). Everything of the client that can
   store into a counter keeps the one open file, every handle on a record among
   them, so the lock is let go when the last of them is dropped, no handle outlives
   it, and the system lets go of it when the process ends. When a client closes or is
   timed out the driver renames the file to `clients/<client id>.gone`, before it frees
   anything of the client's, tries its lock ten times a second, and lets every
   record held for the client go when it takes it. Only the driver removes a lock
   file, after it has taken the lock.

**A lock file is its driver's client's, by an order and not by a name.** A client
opens a handle on the directory `clients/` first. Then it maps the command ring, the
broadcast buffer and the counters file, takes its id from the ring, makes its lock
file through the handle, locks it, and only then sends its first command. A file is
made in no directory that has been removed, and a driver that starts removes
everything the driver before it left, `clients/` with it, before it creates anything
of its own. So a lock file that is made says that the directory the handle is on
was still there, that the next driver had created nothing yet, and that everything
the client opened between is of the one driver whose `clients/` that is. The name
alone could not say it: client ids begin again with each driver, and a path names
the directory of whichever driver holds it then. Every mapping a client makes later
is a second handle on one it opened in that order. A client that cannot make and
lock its file does not attach: a client without its lock sends no command.

**A command that names a client that has gone is dropped.** The driver knows such a
client until it has taken its lock, has nothing of its left to free, and has read the
command ring up to where its producers stood when the lock was taken. A client
writes a command before it lets go of its lock, so every command of a client that has
gone is read and dropped before the driver forgets the client, by that order and by
no wait. The driver registers nobody for such a command and answers nothing, and
`client_protocol.dropped_command` counts it. A first command whose client's lock the
driver can take is of a client that ended after it wrote the command, and is dropped
the same way. Until this decision a command of a client the driver had forgotten was
a new client's first, as it is in Aeron.

**One dependency.** `cavalry-shm` takes `nix` 0.32 with the feature `fs`, for
`openat`, `open` with `O_DIRECTORY | O_NOFOLLOW`, and `unlinkat`: the standard
library makes and removes a file by a path alone. The driver already takes that
version, so the lock file gains no crate, the `unsafe` the calls need is `nix`'s, and
none of the three is on a message's path: a client calls them when it attaches.

In `cavalry-client` a handle on such a record is one type, `Owned`. It is not
`Clone`. Its drop is the one place the word is stored, and its release takes the
handle and drops it, so one handle stores one word by construction. It holds the
client's shared state, which owns the lock. Nothing else in the crate holds a
counter's writing handle. `cavalry-shm`'s own `Counter` stays `Clone` for the driver.

The driver can learn that a client is gone only by a lock the client held. Three
kinds of client give it nothing to try: one of which it finds no lock file at its
first command, which a client of this version never is, so that it is a file
another hand removed; one whose lock file could not be renamed or was
removed by another hand; and one that goes while the driver's list of 256 clients
that have gone is full. Such a
client's release words are read as any client's. What it leaves held with no word
when it closes or is timed out is out of use until the driver starts again. Nothing
is refused for it, and nothing of it is given away.

The counters file goes to version 1, since a reserved field became a field, and
`spec/client-protocol.md` to version 11. A reader of either side rejects the other's
counters file with `version` when it attaches, before it sends a command, and the
message says which side is the older and what to do. So no client of an earlier
build stores into a record of a driver of this one, and this decision carries no rule
for one.

Four counters. `counters.held`, a gauge: the records freed and held.
`counters.retired`: the records out of use until the driver starts again.
`client.without_lock`: the clients of which the driver found no lock file at their
first command, so that `cavalry doctor` warns of a directory that another hand has
changed. `client_protocol.dropped_command`: the commands dropped because their
client had gone. `doctor` also names each
client that has gone and still holds its lock, by its `.gone` file, once it has had
that name for 2 s.

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
- **A lock file made by its path.** The first build. A client of the driver before a
  restart, stopped between taking its id and making its file, makes it in the next
  driver's directory under an id that driver gives out again, and the next driver
  takes it for its own client's lock.
- **The start timestamps of the driver's files in the file's name.** Two runs of a
  driver would be told apart by a clock: one that is stepped back, or is coarser
  than a start, gives two runs one name. That is a likelihood and not a fact.
- **A number from the system's random source in the command ring's header.** It
  does not repeat in practice, changes a shared layout and its page, and is still a
  likelihood.
- **The counters file's device and file numbers in the name**, with a second look at
  the three names when a client attaches. This was built, and stood for an hour.
  POSIX promises that two files that exist at once on one device have different
  numbers. A Linux tmpfs, which `/dev/shm` is, does not keep that promise in every
  case: its numbers were 32 bits from one counter that wraps, and since kernel 5.9 a
  tmpfs mounted with 32-bit numbers, the default there, can still give one number to
  two files once the counter has come round. The chance is small and the claim was
  "cannot".
- **A client that cannot take its lock attaches all the same**, and what it leaves
  is put out of use. The second build. It lost because such a client leaves records
  out of use for the driver's life, and because nothing needs it: a directory that
  keeps no advisory locks is one in which the driver itself does not start (ADR
  0079), so a client's lock is refused only where its user may not write. A client
  that does not attach says which file it could not make, and the driver then
  registers no client that it cannot ask about, short of another hand in its
  directory.
- **Register a late command's client again and keep its predecessor's records under
  the id.** It lost because two clients would then share one id while the first
  one's process still runs, and every rule here is by client id.
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
- **The driver's own tests**, in `conductor.rs`, `mod held`: a removed counter held
  until its word; a gone client's records held until the driver takes its lock; the
  lock tried once an interval; a client without a lock; a record freed after its
  client's lock was taken, which is not held; the held records looked at in turn,
  past one cycle's 64; the list of the clients that have gone at its capacity of
  256, where the next client's records are put out of use and counted, and a
  command of that client's that comes late registers it again with its lock; a lock
  file removed by another hand, which is no answer; a first command of a client
  that has ended; and those of the second and third reads below.
- **`doctor`**: `a_client_that_has_gone_and_holds_its_lock_is_named_by_its_file`.
- **An independent read of the first build** found four faults that every gate,
  twenty runs on both runners and seven mutants had passed over. A handle stored its
  word twice, at its release and again at its drop, and the second store could set
  the next owner's word. A handle could outlive the lock by the order in which a
  struct's fields are dropped. A client removed its lock file by name when it went,
  and the driver took a missing file for a client that was gone, so a client of the
  driver before a restart could free a new client's records. And a record freed
  after its client's lock was taken, as a position over UDP is, was held for ever
  and counted nowhere. Each is closed by structure in the Decision above, and each
  has a test: `a_handle_stores_its_release_word_once`,
  `over_udp_a_position_is_held_until_its_writer_lets_it_go`,
  `a_client_of_the_driver_before_a_restart_removes_nothing_of_the_next_drivers`, and
  the driver's.
- **A second independent read, of the corrected build**, was asked for every path by
  which a store of a client's can come after the driver may have let the record go,
  and for what makes each impossible, a type or an order and never a time. Sixteen
  mutants were caught and three sets of twenty runs had passed on both runners. It
  found the release word's half sound and two faults in the lock's half. A lock
  file was made by its path, which is the first alternative above: only the width of
  a window stood between a client of the driver before a restart and the records of
  the next driver's client. And a client that was stopped past its timeout and
  continued sent a keepalive before it read `client timeout`, which is certain and
  no race; the driver registered a second client of that id, with no lock, and that
  client's own timeout put the first one's held records out of use for good, with
  `client.without_lock` and `counters.retired` counting a fault that was the
  driver's. Each is closed by the Decision, and has a test:
  `no_file_is_made_through_a_handle_on_a_directory_that_was_removed` in
  `cavalry-shm`, on which the whole order stands, and
  `a_lock_is_not_taken_in_the_directory_of_a_driver_that_started_since` in the
  client; `a_client_that_has_gone_is_not_registered_again_by_a_command_that_comes_late`
  in the driver; and the stopped client's test in `stray_store.rs`, which now
  reads both counters at 0. It also found the table of held records without its
  test at capacity: `the_table_of_held_records_is_full_when_the_counters_file_is`.
- **A third independent read, of the build on the handle**, was asked for every
  place in the lock's half where two things could be taken for one, a name, a path,
  a directory, a handle, a restart, an id that starts over, a file number, a process
  and its fork, and for the fact that keeps each pair apart. It traced the order of
  the Decision through one restart and two, a start that fails part-way, and a
  driver killed between `clients/` and its ring, and found it sound in each. It
  found one place still kept apart by a time: the driver could forget a client that
  had gone while a command the client wrote was still unread, and that command then
  registered a second client. That is closed by the ring's position above, with
  `a_client_whose_lock_is_taken_is_kept_until_its_commands_are_read`. It found that
  a client's removal of a file it had made and never announced compared file
  numbers, the fact this decision had rejected: now through the handle. And that a
  dropped command had no counter. It asked for a test of the order on a client's
  real attach, which had none:
  `a_client_stopped_in_its_attach_across_a_restart_attaches_to_the_driver_that_runs`
  holds a client at three steps of its attach while the driver is closed and
  another started, and lets it go on.
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
- Each client holds one more open file. The driver makes two system calls at a
  client's first command, one more when the client goes, and two every 100 ms for
  each client that has gone and whose lock it has not taken, on its conductor's
  thread, on the control plane.
- A client that closes cleanly leaves its lock file for the driver to remove, which
  the driver does within 100 ms of the client's last object being dropped. A client
  whose attach fails after its first command leaves a file that the driver removes
  when it times the client out.
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
  `let_go_released`, `try_locks` and `client_going`; `crates/cavalry-client/src/owned.rs`,
  `subscription.rs`, `conductor.rs`, and `client.rs` for the order a client attaches
  in; `crates/cavalry-shm/src/client_lock.rs`, the handle on `clients/`.
- Aeron: `aeron-client/src/main/java/io/aeron/Image.java` and `ClientConductor.java`,
  `aeron-driver/src/main/java/io/aeron/driver/Configuration.java`, and Agrona's
  `org/agrona/concurrent/status/CountersManager.java`, at their `master` on 2026-11-26.
