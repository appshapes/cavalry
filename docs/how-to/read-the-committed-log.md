# Read a cluster's committed log from a service

A how-to page: one task, run a program beside a cluster member that reads every entry
the cluster has committed, in order, each once. What a service reads and how it is
bounded is [`spec/cluster.md`, "The service"](../reference/cluster.md#the-service); why
it reads a replay of its member's recording is
[ADR 0066](../decisions/0066-feed-the-committed-log-to-a-service-through-a-replay-of-the-members-recording.md),
and how it reads a log that is in several recordings is
[ADR 0103](../decisions/0103-list-the-recordings-of-a-members-log-in-its-driver-directory-for-a-service-to-read-in-order.md).
In Aeron, this is a clustered service ([glossary](../glossary.md)).

A service is a program that applies the cluster's log: every member has one beside it,
each applies the same entries at the same positions, and so each reaches the same
state. An entry is committed once a quorum of members, two of three, has it on disk. A
service is given an entry only then, and only once its own member has it on disk too.

In this version nothing outside a member appends to the log: a cluster started with
`cavalry run` holds one entry, the first of its leader's term. The steps below read
that entry. A program that embeds a member appends with `ConsensusModule::offer`, and
its services read what it appends in the same way.

## Prerequisites

- A cluster of three members running on this host, started as
  [Run a cluster of three members](run-a-cluster.md) says, with member 1's driver
  directory at `/tmp/cavalry-member-1` and member 0's at `/tmp/cavalry-member-0`.
- The example service built: `cargo build --release -p cavalry --example cluster-service`.
  Its source is `crates/cavalry/examples/cluster-service.rs`.

## Steps

1. Read member 1's committed log, one entry:

   ```sh
   target/release/examples/cluster-service --dir /tmp/cavalry-member-1 --entries 1
   ```

   You will see three lines:

   ```text
   cluster-service: reading member 1's log, recording 0, from position 0
   0 template 24 56 bytes
   cluster-service: 1 entries, next position 96, commit position 96
   ```

   The second line is the entry: its position in the log, 0, its template id, 24, which
   is the new leadership term event, and its length. The first and third lines are on
   stderr. `next position` is where the entry after it begins.

2. Read the leader's, member 0's:

   ```sh
   target/release/examples/cluster-service --dir /tmp/cavalry-member-0 --entries 1
   ```

   You will see the same second line. The leader's service and a follower's read the
   log the same way and see the same entries.

3. Start again where a service stopped. Give the next position the last run printed:

   ```sh
   target/release/examples/cluster-service --dir /tmp/cavalry-member-1 --from 96
   ```

   You will see `cluster-service: reading member 1's log, recording 0, from position
   96` and no entry: nothing has been committed past position 96. The program waits
   for the next entry. Stop it with Ctrl-C.

4. In your own program, do what the example does. `Cavalry::connect_to(dir)` connects
   to the member's driver. `client.committed_log(position)` opens the log at a
   position, 0 or the next position of an earlier run, and returns `None` while the
   driver has no member with a log yet, so a service that starts before its member
   calls it again after a pause. `log.poll(handler, fragment_limit)` hands each
   committed entry to the handler with its position and returns how many it read.
   `log.position()` is the next position, the one to keep for the next start.
   `log.is_ended()` says that no poll of this handle will deliver another entry. Stop
   reading when it returns true, not when a poll returns 0. Then read three things,
   each `None` or what went wrong. `log.failed_at()`: a position means the driver
   could not play the member's recording past it, and the entries from it on were not
   delivered, though the member may have committed them. `log.withdrawn_at()`: a
   position means the driver withdrew the replay before the service had read it, and
   the entries from that position on were not delivered. `log.faulted()`: a fault,
   `Fault::Image` or `Fault::ImagesFull`, means this client's own read of the replay
   stopped there. One poll returned that fault as its error, and the handle keeps it
   for as long as it is held, so a service that polled on can still read it. When all
   three are `None`, the handle delivered every committed entry it may read from its
   recording. "When it does not work", below, has an entry for each of the three. In
   every case `log.position()` is where to open the log again.

   A member's log is one recording at first. It is several, in a row, once the
   member has left the log and joined it again, once a later leader has begun a
   term, or once the member has caught up through a replay. A handle reads one
   recording: the one that holds the position the log was opened at, which
   `log.recording_id()` names. So when a handle is ended and all three are `None`,
   call `client.committed_log(log.position())` again. It returns the handle of the
   member's next recording at once when the log goes on in one, and `None` when the
   member has closed or left its log, or is between two recordings, as it does
   before a member has a log. A member between two recordings names its next one
   once its election has closed, so a service that is to go on asks again after a
   pause. The example does the first of these, stops on `None`, and prints a
   `reading member` line for each recording. A
   service that starts at 0 beside a member whose log is in three recordings opens
   the log three times and reads every entry once, in order.

   A poll can return 0
   while committed entries are still on their way: the service reads only what the
   member's recorder has synced. The member asks its recorder to sync as soon as an
   entry is written, so the wait is the time that sync takes, or the time to the
   recorder's own sync, once a second, when the driver refused the request. Keep
   polling until `log.is_ended()` is true: when the member closes or leaves the log,
   the replay ends at the end of the member's recording, and the driver withdraws it
   5 s later, whether or not the service has read it.

   A program in C does the same through `cavalry.h`
   ([`spec/c-abi.md`, "Functions", committed log](../reference/c-abi.md#functions)).
   `cavalry_committed_log(&log, client, position)` returns 0 and leaves `log` null
   while the driver has no member with a log yet. `cavalry_committed_log_poll(log,
   handler, clientd, fragment_limit)` hands each committed entry to the fragment
   handler, and inside the handler `cavalry_committed_log_entry_position(header)` is
   the entry's position. `cavalry_committed_log_position(log)` is the next position.
   `cavalry_committed_log_is_ended(log)` says that no poll will deliver another
   entry, and then `cavalry_committed_log_failed_at(log, &position)`,
   `cavalry_committed_log_withdrawn_at(log, &position)` and
   `cavalry_committed_log_faulted(log, &fault)` are `log.failed_at()`,
   `log.withdrawn_at()` and `log.faulted()` above: each returns `true` and writes
   what it found when the handle ended short of its entries, and all three return
   `false` when the handle delivered every committed entry it may read from its
   recording; `cavalry_committed_log(&log, client, position)` at
   `cavalry_committed_log_position(log)` then gives the next recording, or leaves
   `log` null when the member closed or left its log. The fault is the one a poll returned as −1 with
   `CAVALRY_ERR_FAULT_IMAGE` or `CAVALRY_ERR_FAULT_IMAGES_FULL`; the third function
   answers it from that poll on. `cavalry_committed_log_close(log, client)` ends the
   read.

## How to verify it worked

- Steps 1 and 2 print the same entry line on every member: the same position, the
  same template id and the same length.
- The service's last line gives a `commit position` and a `next position`. `cavalry
  doctor` on the member's directory, with the member's two variables set as
  [Run a cluster of three members](run-a-cluster.md) sets them, prints the member's
  own `commit position` in its `note cluster-member` line. The service's is the same
  or smaller, never larger, and its `next position` is never past it.
- A second run with `--from` and the `next position` of the first prints no entry the
  first run printed.

## When it does not work

- `cluster-service: waiting for a cluster member with a log on this driver`, and
  nothing more: the driver in `--dir` runs no cluster member, or the member has no log
  yet. Run `doctor` on that directory with the member's two variables set. A member
  whose line says `election closed` has a log. `WARN cluster-no-leader` and `WARN
  cluster-joined-late` are explained in
  [Run a cluster of three members](run-a-cluster.md#when-it-does-not-work).
- `error 23 (position out of range)`: `--from` is past what this member has recorded,
  or below every recording of its log that the member keeps. The message gives the
  extent of the recording the member records into now. When `--from` is past it, use
  a `next position` that a run on this member printed, or 0. When that recording
  begins above `--from`, the member no longer keeps the recording that held that
  position: a member keeps the last 1,024 recordings of its log that it stopped, one
  for each time it joined the log, led a term or caught up through a replay, and
  forgets the oldest. Read the entries below on a member that has left the log less
  often. A member of a version of Cavalry before this file writes none, as one does
  while a cluster is being upgraded: beside it a position below its live recording
  is refused in the same way, and the entries below are read beside a member of
  this version.
- `cannot read the file of the cluster member's log's recordings at ...`, and the
  program exits 1: the member lists the recordings that hold its log in the file
  `log-recordings-<member id>` in its driver directory
  ([`spec/cluster.md`, "The log's recordings"](../reference/cluster.md#the-logs-recordings)),
  and the service could not read it, or it failed the check the message names. A
  message with `recordings-version` is a member and a service of versions that do
  not share the file's version: run both at one version. Any other check is a file
  the member did not write: stop whatever else writes in the driver directory. The
  member writes the file again when it next names a recording. A message that ends
  in an error of the operating system, permission denied among them, is the
  service's user without the right to read the driver directory's files.
- `cluster-service: the replay of member 1's recording 0 failed at position ...`,
  right after the `reading member` line, with the position you passed and no entry
  read: `--from` is a multiple of 32 at which no entry begins. The driver's replay
  finds no frame there and ends at once. `cavalry doctor` on the member shows
  `replay.failed` grown by one. Use a `next position` that a run on this member
  printed, or 0. The next entry has the same message for a recording that is damaged.
- `cluster-service: the replay of member 1's recording 0 failed at position ...`, and
  the program exits 1: the driver could not play the member's recording past that
  position, because a segment file of the recording could not be read or a frame in
  it failed a check. The message goes on with a second position, the `--from` to
  start again with: the service has read every entry below that one and none from it
  on. It is the failed position when the service had read all that the replay played.
  It is lower when the driver withdrew the failed replay, 5 s after it ended, before
  the service had read it, as the entry on a withdrawn replay below says. The member
  is still running and goes on committing, so the entries the service was not given
  are not lost to the cluster: this service has not read them. `cavalry doctor` on
  the member's directory warns of `replay.failed`, says to check the log directory's
  disk first, and names the recording when its entry in the catalog is invalid. In
  your own program, `log.failed_at()` returns the failed position and
  `log.position()` the one to start again from. Start the service again with the
  `--from` value the message gives. If the replay fails at the same position again,
  the recording is damaged there: read the entries from it on on another member.
  When the message says the
  program read no entry, `--from` was not the position of an entry, or the recording
  is damaged at that position: use a `next position` an earlier run printed, or 0.
- `--from needs a position, a multiple of 32 from 0`: the program refused the value
  before it asked the driver.
- `error 10 (resource temporarily unavailable)`: the driver already plays 64 replays.
  Each service holds one while it runs. Stop the services that are not needed.
- `cluster-service: the member's log ended`: the member closed or left its log while
  the service read it, and names no recording now. The service has read every
  committed entry up to the position in the message: it read on after the member
  stopped, up to the last commit position it had taken. Start it again with `--from`
  that position once the member runs. It then reads the recording that holds that
  position, the one the member left, and goes on in the member's next.
  When the member's driver stopped with it, as `cavalry run` stops both, the program's
  last line is `client closed (driver-closed)` and it exits 1: the position in the
  line before it still holds.
- `cluster-service: the driver withdrew the replay of member 1's recording 0 before
  this program read the entries from position ...`, and the program exits 1: the
  recording the service read had stopped, and the service did not read the rest of
  its replay within 5 s. The driver keeps a replay's image for that long after the
  replay ends and then removes it, read or not. The service has read every entry
  below the position in the message and none from it on. In your own program,
  `log.withdrawn_at()` returns that position. Start the service again with the
  `--from` value the message gives once the member runs: it reads the recording that
  holds that position again. There are two causes. The member closed or left its
  log, and the service did not poll for 5 s. Or the service read a recording below
  the member's live one, and the member's commit position stayed below that
  recording's end for 5 s: the entries past it were not committed yet, which a
  cluster with no leader, or a follower that cannot reach its leader, leaves so.
  `cavalry doctor` on the member's directory says which.
  To avoid the first, keep the service polling until `log.is_ended()` is true, and make
  each poll's work much shorter than 5 s: a handler that blocks, or a service that is
  paused or throttled for 5 s after its member stops, ends this way.
- `cluster-service: the read of member 1's recording 0 stopped on a fault: ...`, and
  the program exits 1: the service's own read of its replay stopped, and the member
  and its recording are not at fault. The message says which of three it was. A
  frame of the replay's image failed a check: the replay's log buffer in the driver
  directory is damaged, so run `cavalry doctor` on the member's directory and check
  the memory the directory is on. The replay's image was gone before this client
  read it: the service's process was held for over a second just after it opened
  the log, by a stopped process, a debugger or a host short of CPU. The image could
  not join: the log's subscription already held 256 images, each another session on
  the service log stream, stream 104 on `cavalry:ipc` of the member's driver. The
  replays of other committed logs are such sessions, but the driver holds at most 128
  replay publications
  ([`spec/client-protocol.md`, "Sizes"](../reference/client-protocol.md#sizes)), so
  the others are publications a program added on stream 104. Move those
  publications to another stream id: stream 104 carries the service log. In each
  case the service has read every entry below the position in the message and none
  from it on, and the member's recording still holds the rest.
  Start the service again with the `--from` value the message gives. In your own
  program, `log.faulted()` returns the fault once `log.is_ended()` is true, however
  many polls were made after the one that returned it.
- A service can be given an entry a moment after the cluster commits it: an entry
  reaches a service once its own member's recorder has synced it, which the member
  asks for as soon as the entry is written, and a quorum of the other members can
  have synced it first. A request the member's driver refuses, its recorder's queue
  of requests full, is not made again until another entry is written, and the entry
  then waits for the recorder's own sync, within a second. No entry is lost or
  reordered by the wait.
