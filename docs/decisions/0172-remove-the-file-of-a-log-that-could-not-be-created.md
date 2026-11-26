# 0172. Remove the file of a log that could not be created

- **Status:** Accepted
- **Date:** 2026-11-26
- **Invariants affected:** 3, 4
- **Supersedes:** none. Adds one paragraph to `spec/log-buffer.md`, "File", within
  version 1: no byte of the file changes and no version moves.

## Context

The driver creates a log in steps: it creates the file, sizes it, maps it, makes
every page of it resident (ADR 0027), and then publishes the header. When a step
after the first failed, `LogBuffers::create_at` returned the error and left the
file. For a publication over `cavalry:ipc` or `cavalry:udp` and for an image the
conductor counted `driver.log_create_failed`, refused, and did not remove it
either. The file's name holds a correlation id that is new at each ask, so each
ask left another file.

The failure that matters is a driver directory with no room, `/dev/shm` on Linux,
and there the file is what takes the room: the pages the filesystem could still
give went to the file that failed, and stayed with it. GitHub issue 238 measured
it on 2026-11-12 in a container whose `/dev/shm` is 64 MiB, for an image's log
that a sender's setups asked for ten times a second: 50 files after 5 s, 149 after
15 s and 249 after 25 s, the first holding the 8,172 KiB the filesystem had left
and the others nothing, all still there after the driver closed. ADR 0168 made it
pressing: every cluster service now asks its driver for a `cavalry:ipc` log of 192
MiB when it first opens its member's log, and again once a second while it is
refused.

A replay's path did the opposite, and could be wrong the other way. Its undo
removed the file at the replay's path whether or not the replay had created it.
Where the directory had no room that was the replay's own file. But a `start
replay` whose correlation id was a live publication's failed its creation on that
publication's file, and then removed it.

## Decision

A log that the driver cannot create leaves no file, and a file that the creation
did not make is not removed. `LogBuffers::create_at` removes the file it created
when any step after the creation fails, before it returns the error, and drops
its mapping first, so that the filesystem has the file's pages back. A file that
was already at the path fails the creation itself and stays. Every log the driver
makes is created there, a publication's, an image's and a replay's, so the rule
holds for all of them from one place, and a replay's undo removes its log only
where the replay made one. `spec/log-buffer.md` says both.

## Alternatives considered

- **Remove the file in the conductor, in each path that creates a log.** The
  conductor has four, and the one that did remove, the replay's, removed a file
  that was not its own. It lost to the one function they all call: the function
  that made the file is the one that knows it did.
- **Leave the file and have the driver's start remove it.** The start already
  removes everything the last driver left in its directory. That frees the room
  at the next start, and not for the driver that runs, which is the one that
  needs it.
- **Ask less often.** ADR 0168 paces a service's ask to once a second, and a
  sender's setups come ten a second by `spec/udp.md`. A slower ask leaves fewer
  files and the first still takes the room.
- **Remove the file only where the touch of its pages failed.** That is the
  failure issue 238 measured. A file that could not be sized or mapped is as
  much a log that was not created, and one rule is simpler to state.
- **Ask the filesystem for its room before the file is sized, and refuse with
  nothing created.** It would make a refusal cheap, where the removal makes it
  cost what the Consequences say. It is no replacement, since room can go between
  the question and the touch, and it needs a call that the standard library does
  not have, so a dependency or an `unsafe` block in `cavalry-shm` for it. Left
  for when the cost of a refusal is shown to matter.

## Evidence

- **The measured case, in a container.** `rust:1` on Linux 7.0.14 on aarch64, in
  Docker on an Apple M5 Pro laptop, with Docker's default `/dev/shm` of 64 MiB and
  the driver's directory on it. A client asks the driver five times for a
  `cavalry:ipc` publication, 192 MiB. Without the removal, the first ask is
  refused with "could not make the mapping resident", and leaves
  `publications/2.logbuffer`, 201,330,688 bytes long, with 0 KiB available on
  the filesystem where 57,328 were. With the removal, each of the five asks is
  refused with that error, the directory holds no log file after any of them, and
  57,328 KiB are available after each as before the first. The independent read
  of this change ran it again, with the same numbers. The test is
  `a_driver_whose_directory_has_no_room_leaves_no_log_file` in
  `crates/cavalry-client/tests/publication.rs`.
- **That case as a gate.** `just shm-exhaustion <directory>` runs that test, and
  CI's Linux job mounts a filesystem of 64 MiB and runs it there, since a test
  cannot mount one for itself and `just test` passes it over. This is the "shm
  exhaustion" that `CLAUDE.md`'s gate 3 names, which no test held before.
- **In `just test`, on Linux and macOS.** Two tests make the creation fail for
  real, after the file exists, with no hook in the code: each runs itself again
  as a child process whose files the operating system limits to less than the
  log, so that sizing the file fails. `a_log_that_cannot_be_created_leaves_no_file`
  in `crates/cavalry-shm/tests/log_buffer.rs` asks three times and finds no file
  after each, creates the log in the same directory with no limit, and then
  holds that a creation which finds that file there fails and leaves it, still
  opening. `a_driver_that_cannot_create_a_log_refuses_it_and_leaves_no_file`,
  beside the container's test, starts a driver under a limit of 32 or 64 MiB:
  five asks for a `cavalry:ipc` publication are each error code 12 and one more
  in `driver.log_create_failed`, with no log file after any, and a `cavalry:udp`
  publication of 64 KiB terms is created after them. Each fails without the
  removal, with the file of the first ask in its message. The read ran both in
  the container too, where `sh` is another shell: three of three passed with the
  removal and none without. The driver's largest file of its own is 6,291,584
  bytes, under the limit.
- **A replay under a live publication's id.**
  `a_replay_under_a_live_publications_id_leaves_that_publications_log`, in the
  conductor's tests, is the read's probe: the replay is refused with error code
  12 and counted, and the publication's log is there and opens. With the undo as
  it was, the file is gone.
- **What the two in `just test` do not hold.** They fail at the sizing, and only
  the container's test, which the CI step runs, reaches the touch of the pages. A
  removal made only where the sizing fails would pass `just test` and fail
  `just shm-exhaustion`. That recipe fails when its one test did not run.

## Consequences

- A driver whose directory has no room refuses a log as often as it is asked and
  keeps what room it has. ADR 0168's consequence, a file at each ask of a service
  that is refused its publication, ends with this decision, and the sentence the
  clients' READMEs carried for it is taken out.
- Each refused ask now costs the driver more than the ones after the first did.
  The read measured it in the container, twelve asks for a `cavalry:ipc` log
  with 57,328 KiB free: with the removal every ask took 16.0 to 23.1 ms in one
  run and 15.5 to 18.1 ms in another, debug build and release alike, since each
  populates all the room there is and gives it back; without it the first took
  16.7 ms and the later ones 0.3 to 0.9 ms, the room being gone. Twelve asks for
  a log of an image's length, 48 MiB, with 48,128 KiB free, took 12.5 to 15.3 ms
  each. That is on the conductor's thread, which serves every client of the
  driver: an image asked for ten times a second costs it about 125 to 155 ms of
  each second where nearly a log's room is free, and less with less, for as long
  as the directory has no room, and the filesystem is full for its other users
  for the length of each ask. The last alternative above is the way out if that
  is shown to matter.
- A log that would fit once another is closed can now be created then: the room
  a closed log gives back is no longer behind a file that failed.
- What `cavalry doctor` says of `driver.log_create_failed` is true of the
  directory again: every file under `publications/` and `images/` is a live or
  lingering publication's, an image's or a replay's.
- Where the file cannot be removed either, as in a directory made read-only
  after the file was created, it stays until the driver's next start, and the
  error returned is the creation's. Nothing counts that. A driver that is killed
  between the creation and the removal leaves the file the same way.
- The driver's own three files, its counters, its command ring and its broadcast
  buffer, are created the same way at its start and are not changed here: their
  names are fixed, and a start that fails leaves them to the next start in that
  directory, which removes them, so a retry does not multiply them.
- A recording that cannot be started leaves a catalog slot and an index file
  under a new recording id each time, by `cavalry-log`'s own stated rule, bounded
  by the catalog. That is not this decision's.
- The error a client reads where the directory has no room names a `doctor`
  check that says nothing of room, and `doctor` has no check of a driver
  directory's free room: work item 496.
- All of this is of a kernel that has the populate advice, Linux 5.14 and later.
  On an earlier kernel a driver whose directory has no room is not refused a log
  but killed by a bus error, and its file stays: the read ran that too, under a
  seccomp profile that answers the advice as such a kernel does. It was so
  before this decision, ADR 0027 does not say it, and it is work item 497.
- A command whose correlation id is in use is still taken by the driver: a
  replay under a live publication's id now leaves that publication its log, and
  is refused with a code and a counter that name the wrong cause, and two
  replays under one id are not this decision's. Work item 498.
- CI's Linux job gains a step that mounts a filesystem with `sudo` and prints
  the kernel's release. The gate's two tests in `just test` need a shell with
  `ulimit -f` and `trap`, which every system Cavalry builds on has.
- Revisit if a log is ever created outside `LogBuffers::create_at`, and when a
  refusal's cost on the conductor's thread is measured to hold a driver's other
  clients up.

## References

- Work item 492; GitHub issue 238, which measured it and which this change
  closes; work item 462 and
  [ADR 0168](0168-add-a-services-counters-and-its-publication-to-its-member-when-it-first-opens-its-members-log.md),
  whose independent read found it again by reading the driver; work items 496,
  497 and 498, found by this change's reads.
- `spec/log-buffer.md`, "File"; `spec/client-protocol.md`, "Commands"; `spec/udp.md`,
  "The receiver".
- [ADR 0027](0027-pre-touch-shared-mappings-at-map-time.md).
