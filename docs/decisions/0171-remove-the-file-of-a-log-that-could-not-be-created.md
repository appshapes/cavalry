# 0171. Remove the file of a log that could not be created

- **Status:** Accepted
- **Date:** 2026-11-26
- **Invariants affected:** 3, 4
- **Supersedes:** none. Adds one paragraph to `spec/log-buffer.md`, "File", within
  version 1: no byte of the file changes and no version moves.

## Context

The driver creates a log in four steps: it creates the file, sizes it, maps it and
makes every page of it resident (ADR 0027), and then publishes the header. When a
step after the first failed, `LogBuffers::create_at` returned the error and left
the file. The conductor counted `driver.log_create_failed`, answered error code 12,
and did not remove it either. The file's name holds the command's correlation id,
so a caller that asked again left another file each time.

The failure that matters is a driver directory with no room, `/dev/shm` on Linux,
and there the file is what takes the room: the pages the filesystem could still
give went to the file that failed, and stayed with it. GitHub issue 238 measured
it on 2026-11-12 in a container whose `/dev/shm` is 64 MiB, for an image's log
that a sender's setups asked for ten times a second: 50 files after 5 s, 149 after
15 s and 249 after 25 s, the first holding the 8,172 KiB the filesystem had left
and the others nothing, all still there after the driver closed.

ADR 0168 made it pressing. A cluster service now adds its publication to its
member, a `cavalry:ipc` log of 192 MiB, when it first opens its member's log, and
asks again once a second while the driver refuses it. Every service is a caller
of this path now, where only a program with a timer or a snapshot was.

## Decision

A log that the driver cannot create leaves no file. `LogBuffers::create_at`
removes the file it created when any step after the creation fails, before it
returns the error, and drops its mapping first, so that the filesystem has the
file's pages back. A file that was already at the path fails the creation itself
and is not removed. Every log the driver makes is created there, a publication's,
an image's and a replay's, so the rule holds for all of them from one place, and
`spec/log-buffer.md` says it.

## Alternatives considered

- **Remove the file in the conductor, in each path that creates a log.** The
  conductor has four, and removes a log by its correlation id where a counter is
  refused after the log was made. It lost to the one function they all call: a
  fifth path would have to remember, and the function that made the file is the
  one that knows it did.
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

## Evidence

- **The measured case, in a container.** `rust:1` on Linux 7.0.14 on aarch64, in
  Docker with its default `/dev/shm` of 64 MiB, the driver's directory on it. A
  client asks the driver five times for a `cavalry:ipc` publication, 192 MiB.
  Without the removal, the first ask is refused with "could not make the mapping
  resident", and leaves `publications/2.logbuffer`, 201,330,688 bytes long, with
  0 KiB available on the filesystem where 57,328 were. With the removal, each of
  the five asks is refused with that error, the directory holds no log file
  after any of them, and 57,328 KiB are available after each as before the
  first. The test is
  `a_driver_whose_directory_has_no_room_leaves_no_log_file` in
  `crates/cavalry-client/tests/publication.rs`, marked to be run by hand, since a
  test cannot mount a small filesystem for itself; its comment has the command.
- **In the gate, on every runner.** Two tests make the creation fail for real,
  after the file exists, with no hook in the code: each runs itself again as a
  child process whose files the operating system limits to less than the log,
  so that sizing the file fails. `a_log_that_cannot_be_created_leaves_no_file` in
  `crates/cavalry-shm/tests/log_buffer.rs` asks three times and finds no file
  after each, and then creates the log in the same directory with no limit.
  `a_driver_that_cannot_create_a_log_refuses_it_and_leaves_no_file`, beside the
  container's test, starts a driver under a limit of 32 or 64 MiB: five asks for
  a `cavalry:ipc` publication are each error code 12 and one more in
  `driver.log_create_failed`, with no log file after any, and a `cavalry:udp`
  publication of 64 KiB terms is created after them. Each of the two fails
  without the removal, with the file of the first ask in its message.

## Consequences

- A driver whose directory has no room refuses a log as often as it is asked and
  keeps what room it has. ADR 0168's consequence, a file at each ask of a service
  that is refused its publication, ends with this decision, and the sentence the
  clients' READMEs carried for it is taken out.
- A log that would fit once another is closed can now be created then: the room
  a closed log gives back is no longer behind a file that failed.
- What `cavalry doctor` says of `driver.log_create_failed` is true of the
  directory again: what it holds is what the driver's publications, images and
  replays hold.
- Where the file cannot be removed either, as in a directory made read-only
  after the file was created, it stays, and the error returned is the
  creation's. Nothing counts that.
- The failure itself still costs the driver the making of the file up to the step
  that failed, on its conductor's thread, at each ask.
- The gate's two tests need a shell with `ulimit -f` and `trap`, which every
  system Cavalry builds on has.
- Revisit if a log is ever created outside `LogBuffers::create_at`.

## References

- Work item 492; GitHub issue 238, which measured it and which this change
  closes; work item 462 and
  [ADR 0168](0168-add-a-services-counters-and-its-publication-to-its-member-when-it-first-opens-its-members-log.md),
  whose independent read found it again by reading the driver.
- `spec/log-buffer.md`, "File"; `spec/client-protocol.md`, "Commands"; `spec/udp.md`,
  "The receiver".
- [ADR 0027](0027-pre-touch-shared-mappings-at-map-time.md).
