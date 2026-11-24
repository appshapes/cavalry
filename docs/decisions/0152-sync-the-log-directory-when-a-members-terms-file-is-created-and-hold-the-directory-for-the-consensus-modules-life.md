# 0152. Sync the log directory when a member's terms file is created, and hold the directory for the consensus module's life

- **Status:** Accepted
- **Date:** 2026-11-24
- **Invariants affected:** 1, 4, 5, 6
- **Supersedes:** none. Amends `spec/durable-log.md`, "The driver", within version 6:
  the terms file's creation gains a sync of the log directory, and a member's
  consensus module holds the directory by a lock for its life. No byte of any file
  changes, and no file is added.

## Context

A cluster member's consensus module writes three files in the driver's log
directory: `terms`, its record of terms, its candidate term and its vote floor (ADR
0060, ADR 0145); `log-recordings`, the recordings that hold its log (ADR 0108); and
`snapshots` (ADR 0128). The review of ADR 0108 found two gaps, which work item 288
carries.

First, the creation of `terms` synced the file and not the directory, while the
creations of `log-recordings` and `snapshots` end with a sync of the directory. After
a power loss the name of an unsynced file can be gone while its bytes were synced. A
member whose `terms` file is gone has lost the votes it cast. A member's start creates
`terms` first and then the other two when they are not there, and a directory sync
makes every name in the directory durable, so at a member's first start the sync that
ends the creation of `log-recordings` already held the name of `terms`. One start was
left: a start that creates `terms` beside the other two, which happens only when
`terms` is gone. Nothing then synced its name.

That start raised a question of its own. What does a member do on a directory that
holds `log-recordings` and no `terms`? Today, with a log it is refused with
`terms-lost`: when the catalog holds a recording of the log's stream, or when the list
names a log that ends above 0 (ADR 0104, ADR 0108). With no log, it starts with its
`terms` file created again: candidate term −1, no vote floor, and its log empty. It
may have cast a vote before, or taken a leader and stored a floor, and both are gone
with the file.

Second, nothing kept a second consensus module off a log directory. The driver holds
its driver directory by a lock on the directory (ADR 0079), and its log directory by a
lock on the catalog (ADR 0025). `cavalry run` holds a driver and a member in one
process, so a second `cavalry run` on the directory is refused by the driver's lock.
But a member embedded in a program of its own, through the library or the C ABI, is a
client of the host's driver. Two such programs, with one member id or with two, reach
one log directory through one driver. Each reads `terms` and `log-recordings` into
memory at its start, and each then writes the files from its own copy. One module's
vote can overwrite the other's, and a member can vote twice in a term (invariant 1).

Aeron's consensus module keeps a mark file, `cluster-mark.dat`, in its cluster
directory. The module writes an activity timestamp into it, and a module that starts
refuses while the timestamp is younger than a timeout. A module whose process was
killed leaves a fresh timestamp, so the next start on that directory waits for the
timeout. Cavalry's driver told a live driver by a heartbeat in the same way until ADR
0079, which replaced it with a lock of the operating system for that reason.

## Decision

**The terms file's creation syncs the log directory.** A creation of `terms` ends,
after the magic's sync, with a sync of the log directory. This holds for a first
creation and for one a crash tore and the module writes again. It is one function of
`cavalry-log`, `dir::sync_directory`, which also ends the creation of
`log-recordings` and of `snapshots`. So the three creations take one code path, and
the name of `terms` no longer depends on the files created after it.

**A start with `log-recordings` and no `terms` is refused only with a log.** With a
log, the start is `terms-lost`, as before. With none, the member starts as one whose
log is empty, as on a new log directory. ADR 0145's rule is written for a member whose
log is empty, whatever emptied it. Such a member withholds its vote while a member's
report holds a log. It votes in the one case that rule allows, takes a leader only in
that rule's two cases, and stores a new vote floor when it takes one. So the vote it
may have cast and the floor it may have stored are held as for a member that lost its
whole directory. A refusal would send the operator to a new log directory, which gives
the member the same state at the cost of a new driver. Since the sync above, no crash
leaves `log-recordings` without `terms`: `terms` is created, and the directory synced,
before the other two. So this state is the work of a person or a program that removed
the file, and the start treats it as a new directory would be treated.

A snapshot does not move a start to the other side. A snapshot of a position above 0
is one the member took of a log it held to that position, or one its service took of
the log it read; one of position 0 reads the log from 0, as no snapshot does. A start
that can read a snapshot above 0 reads the log from its position, so it needs a log
that reaches there, and is refused without one: with
`log-short` when the list names no log that reaches the position, and with
`terms-lost` when it names one (`kept_log` in `cavalry-raft`). A snapshot that the
start cannot read is passed over, counted in `cluster.snapshots_passed_over`, and the
start reads from position 0, so the member starts with its log empty, under ADR 0145
as above. A start from a snapshot a peer sent is feature 5's, work item 365, and the
decision that builds it places its own directory.

**A consensus module holds its log directory for its life.** At its start, after
it opens the driver's catalog and before it reads or writes any of its three files,
the module takes an exclusive advisory lock of the operating system (`flock(2)`, Rust's
`File::try_lock`) on the log directory itself. It tries the lock once and never
waits. The module keeps the lock until it is dropped, and the operating system drops
the lock when the module's process ends, however it ends. So no hold outlives its
module, and a member whose process was killed starts again on its directory at once.
The driver's lock is on the catalog, a file of its own, so the driver's lock and the
module's never meet.

**The refusals.** A module that finds the directory held refuses to start with
`log-directory-held`, whatever its member id and whatever process holds the
directory. It writes nothing. Its message names the directory, says what to do, and
names `lsof <directory>`, which lists the process that holds the directory open on
Linux and on macOS.

A lock that fails for any reason other than being held is not `log-directory-held`:
it is `log-directory-lock`, which also writes nothing, and the member does not start.
That is what the driver does with its own lock on the catalog (ADR 0025), and for the
same reason: a member that started without the hold could be the second writer this
decision exists to stop, and nothing would show it. The filesystems that refuse are
network ones. On NFS an exclusive lock of `flock(2)` is sent to the server as a
lock that needs a descriptor open for writing, and a directory is never open for
writing, so every start there is refused. The driver's lock is on a file it opens for
writing, so the driver itself started there before this decision. `cavalry doctor
--log-dir` has warned since ADR 0025 that a log directory on such a filesystem does
not make a byte durable on this host, so a member's vote was not durable there
either. A member that ran on one meets the refusal at its first start under this
version. The refusal says to give it a log directory on a disk of this host, by
copying the whole directory there with its driver stopped. In an upgrade one member
at a time, that move is made for each member before it is upgraded.

**The doctor note.** `cavalry doctor --log-dir` never tries the lock: a try at the
moment a member starts would refuse that member. It reports the hold in a note,
`log-directory-held`, in three forms:

- On Linux it reads `/proc/locks` and names the pid of each process that holds the
  directory's lock.
- On Linux, where that table shows no holder and the directory holds a `terms` file,
  so that it is a member's, it says that no holder is visible from here, and never
  that the directory is free. The kernel lists only the locks whose holder is in the
  reader's pid namespace, so a `doctor` run from another container sees no line for
  a directory a member holds. On btrfs the table gives a file in a subvolume another
  device number than `stat` does, so the match by device and inode misses there.
- Elsewhere, macOS among them, there is no such table, and for a member's directory
  the note says that the hold cannot be read here and names `lsof <directory>`.

The finding on an overlay or network filesystem now names the module's lock beside
the catalog's, since neither may hold there.

## Alternatives considered

- **Refuse a start with `log-recordings` and no `terms`.** It would name the state with
  an error of its own. It lost because ADR 0145 already holds what the member lost, as
  above, and because the remedy it would give, a new log directory, reaches the same
  state at a higher cost.
- **Lock the `terms` file instead of the directory.** The lock would need the file to
  exist. A start refused afterwards would leave an empty `terms` file. The next start
  would then pass the check that refuses a log beside no `terms` file, and read the
  empty file as a creation a crash tore. The directory's lock creates nothing, comes
  before every check, and covers all three files.
- **A lock file of the module's own, holding its pid, as the driver directory's `pid`
  file does (ADR 0079).** A refusal could then name the pid on every platform. It lost
  because it adds a file to a directory whose page says that nothing else lives there,
  for a name that `lsof` and, on Linux, `/proc/locks` give without one.
- **The driver's claim extended to its member.** The module would ask its driver for
  a member's claim over the command ring. That needs a command and a response, a
  change to `spec/client-protocol.md`, and a release when the driver sees the client
  gone. A killed module would then hold its claim until its client timed out, which is
  the stale claim this decision exists to avoid.
- **A mark file with a heartbeat, as Aeron has.** A killed module's mark would refuse
  the next start until the timeout passed. ADR 0079 left this design for the same
  reason.
- **Have `doctor` try the lock to tell whether it is held.** A shared or an exclusive
  try, however short, refuses a module that starts at that moment.

## Evidence

- `crates/cavalry-log/src/terms.rs`,
  `a_creation_of_the_terms_file_syncs_the_log_directory_by_the_path_the_other_two_files_take`.
  It counts the calls of `dir::sync_directory` on its thread. A creation by `open` and
  by `create` each sync the directory once, and so does a creation a crash tore,
  written again, both shorter than the header and with a zero magic. An open of a
  whole file syncs none. The creations of `log-recordings` and `snapshots` take the
  same path. No test sees a sync reach the disk. With the sync taken out of `terms.rs`,
  the test fails at its first count, "a creation by open".
- `crates/cavalry-raft/tests/log_directory.rs`:
  - `a_second_module_on_a_held_log_directory_is_refused_and_writes_nothing` starts two
    modules, member ids 1 and 2, on a directory that a module of member 1 holds. Each
    is refused with `log-directory-held`, the three files are byte for byte as they
    were, and member 1 starts once the first module is dropped.
  - `a_member_whose_process_was_killed_starts_again_on_its_log_directory` runs member
    1's module in a child process, as a client of the test's driver. While the child
    runs, a start in the test's process is refused. After `SIGKILL` the start succeeds
    at once, well under its 5 s bound.
  - With the lock taken out of `hold_log_dir`, both tests fail.
  - `a_member_whose_terms_file_is_gone_beside_no_log_starts_with_its_log_empty` stores
    a candidate term of 7 and a vote floor of 4096 and starts the member once. It then
    removes `terms` and starts the member again. The member starts at append position
    0, with candidate term −1 and no floor, and with the `log-recordings` list it kept,
    unchanged.
- `crates/cavalry-control/src/doctor.rs`:
  - `the_holders_of_a_files_flock_are_read_from_proc_locks` reads `proc(5)`'s lines.
    A waiter, a lock of another kind, inode or device, and a repeated pid are each
    excluded. A device's major and minor numbers are read past their low bits.
  - `a_log_directory_a_module_holds_is_a_note_that_names_its_pid_on_linux` checks
    that, on Linux, a held directory gives the note with the test's own pid. A
    member's directory that no visible process holds gives the note that no holder
    is visible from here on Linux, and that the hold cannot be read here elsewhere.
    A directory of no member, unheld, gives none. On every platform, `doctor` leaves
    the holder's lock as it was.
- The module tests' member that is lost with its process, `std::mem::forget` in
  `src/module/tests.rs`, now drops its hold first, as the operating system would at
  the end of the process. Without that, the start that follows is refused, which is
  the rule working.

## Consequences

- Each of a member's three files has one writer, as the catalog has. A second member
  is refused at its start, with a reason and a remedy, and no longer overwrites the
  first member's files.
- A start on a log directory costs one more `open` and one `flock`. A creation of
  `terms` costs one more directory sync, once per member's directory. Nothing runs per
  message.
- Mixed versions: a module of an earlier tree takes no lock, so the refusal holds only
  between modules of this tree or later, on one directory. A module of this tree that
  starts beside an earlier one on the same directory is not refused, as before this
  decision. That case was already a fault, and an upgrade one member at a time does
  not start it.
- A member whose log directory is on NFS, or on another filesystem that refuses the
  lock, no longer starts there, and is moved to a disk of this host, as the Decision
  says.
- On an overlay or network filesystem, a lock of the operating system may not hold
  across hosts. `cavalry doctor --log-dir` already warns about such a directory, and
  the warning now names both locks.
- `doctor` names the module's pid on Linux alone, and only for a holder in its own pid
  namespace and on a filesystem whose device numbers match. Elsewhere, and from
  another container, the refusal's `lsof`, run where the member runs, is the way to
  find it.
- Revisit if a module comes to be started by a supervisor that keeps the previous
  process alive while the next one starts. The next one would then be refused until
  the previous one ends, and the supervisor would need to wait for it.

## References

- Work item 288 on the Brigade Board, a part of feature 3 of the beta slice, work item
  363 (ADR 0138). It was filed from the review of ADR 0108 (card 228).
- `spec/durable-log.md`, "The driver"; ADR 0025, ADR 0060, ADR 0079, ADR 0104, ADR
  0108, ADR 0128, ADR 0145.
- `crates/cavalry-log/src/dir.rs`, `sync_directory`; `crates/cavalry-log/src/terms.rs`;
  `crates/cavalry-raft/src/module.rs`, `hold_log_dir`;
  `crates/cavalry-control/src/doctor.rs`, `flock_holders`.
