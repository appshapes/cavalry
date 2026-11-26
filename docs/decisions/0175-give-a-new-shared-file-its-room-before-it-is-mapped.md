# 0175. Give a new shared file its room before it is mapped

- **Status:** Accepted
- **Date:** 2026-11-26
- **Invariants affected:** 3, 4
- **Supersedes:** ADR 0027 in one part, what happens where a driver directory cannot
  hold a file: on Linux that is found before the file is mapped, on every kernel, and
  no longer by the populate advice alone. ADR 0172 in one part, what a refused log
  costs. Changes one sentence of `spec/log-buffer.md`, "File", within version 1: no
  byte of the file changes and no version moves.

## Context

The driver creates each shared file, a log buffer, the counters file, the command
ring and the broadcast buffer, in steps: it creates the file, gives it its length,
maps it, and makes every page resident (ADR 0027). A file that is only given its
length is sparse. The filesystem finds a page for it when a mapping first touches
one, and when it has none the kernel answers the touch with a bus error, which ends
the process.

ADR 0027 made that an error on Linux by the populate advice,
`madvise(MADV_POPULATE_WRITE)`, which reports a page the kernel cannot back. It says
that "a driver directory out of memory fails here, by name", and that a kernel
without the advice, before 5.14, "falls back to the touch loop". It does not say
what the touch loop meets in such a directory. The second independent read of work
item 492 ran it: under a seccomp profile that answers the advice as such a kernel
does, a driver whose directory had no room for a log was killed by a bus error, and
the file stayed, with the filesystem full (work item 497).

Three things make this more than a corner.

- The kernels before 5.14 are the ones under RHEL 8, Ubuntu 20.04, Debian 11 and
  Amazon Linux 2, and a container built on a newer system still runs on its host's
  kernel. Work item 481 sets the oldest C library an artefact runs on at those
  systems' own.
- Since ADR 0168 every cluster service asks its driver for a `cavalry:ipc` log of
  192 MiB when it first opens its member's log. A container's default `/dev/shm` is
  64 MiB. So on such a host the case is a cluster member's default path, and a
  member's driver that dies takes the member with it.
- No gate could see it: every CI runner's kernel has the advice.

ADR 0172 left a second matter open. Since it, a refused log leaves no file, and each
refused ask populates all the room the filesystem has before it fails and gives it
back: 15.5 to 23.1 ms an ask on the conductor's thread in a container with 57,328
KiB free. It named the way out, asking the filesystem for its room first, and left
it "for when the cost of a refusal is shown to matter".

## Decision

On Linux, a new shared file is given the room for every byte of it before it is
mapped. `allocate::sized` in `cavalry-shm` replaces the bare length at each of the
four places a file is created, and does three things in order.

1. It asks the filesystem how much room it has free, with `fstatvfs`, and refuses a
   file longer than that with nothing asked of the filesystem. Every free block is
   counted, the ones kept for the superuser too, so the look refuses nothing an
   allocation could be given, and a filesystem that reports no blocks at all, as a
   tmpfs with no size does, is not asked. The answer can be stale by the next call,
   so the look only ever refuses.
2. It asks for the room itself, with `fallocate`. The filesystem gives all of it or
   fails with its own error, and nothing is written and no page is touched. An
   interrupted call is made again, eight times at the most.
3. Where the filesystem cannot allocate ahead and says so, `EOPNOTSUPP`, the file is
   written through with zeros, which asks for the same room a block at a time and
   fails with an error and never with a signal.

Then the file is mapped and its pages are made resident as ADR 0027 has it, by the
populate advice or by the touch loop. Either now meets pages the filesystem has
already given, so no kernel answers a touch of a new file with a bus error for want
of room. Cavalry sets no oldest kernel for this reason: the driver starts on every
Linux kernel that the Rust standard library runs on, and a directory with no room is
a refusal, by name, on each of them.

A failure of any of the three is the creation's error, and the file is removed as
ADR 0172 has it. The error says what could not be done, the two numbers where the
look at the free room refused, and what to do.

On every other system a file is given its length alone, as before. macOS is the one
Cavalry runs on: what was measured there, and why it is left, is under Alternatives
and is work item 505.

The gate `just shm-exhaustion` runs its test a second time on Linux as a kernel
without the advice runs it. `scripts/no-populate-advice.c` puts a seccomp filter on
itself that answers the two populate advices with `EINVAL`, which is such a kernel's
answer, checks that the answer is that, and runs the command under it. The other
tests of the same program then run under it too, so that a log that does fit is
created and used by the path such a kernel takes.

## Alternatives considered

- **An oldest kernel, 5.14, checked at the driver's start.** One line of code and a
  `doctor` check. Lost: it turns away RHEL 8, Ubuntu 20.04, Debian 11 and Amazon
  Linux 2 as hosts, containers on them included, for a failure that asking the
  filesystem first does not have.
- **`fallocate` alone, with no look at the free room.** It closes the bus error and
  was measured first. Where the log is longer than the whole filesystem the kernel
  refuses at once, 0.17 to 0.97 ms an ask. Where the log would fit an empty
  filesystem and does not fit the free room, the kernel takes all the free room
  before it fails and gives it back: 44.3 to 50.1 ms an ask with 151,536 KiB free,
  no better than the populate advice there, 40.2 to 54.8 ms in two runs, and the
  filesystem is full for its other users meanwhile. Lost to the look first, which
  makes both cases 0.14 to 0.83 ms.
- **The look at the free room alone, and no allocation.** Lost: room can go between
  the look and the touch, and then the touch is the bus error again. The look is
  there for the cost and the allocation for the guarantee.
- **`posix_fallocate`.** The C library's version writes a byte to each block where
  the filesystem cannot allocate ahead, and musl's does not and fails. One
  behaviour on both, written here, is simpler to state.
- **Write every new file through with zeros, on every system.** It needs no call
  beyond the standard library and it refuses on macOS too. Lost: it writes 192 MiB
  for each `cavalry:ipc` log where `fallocate` writes nothing, and on macOS that is
  192 MiB to the volume for a file that today takes none until it is used.
- **The same on macOS, with `fcntl(F_PREALLOCATE)`.** Measured on the laptop with a
  probe and no Cavalry code: on a volume of 64 MiB it does refuse a file of 192
  MiB, where the bare length and a store to every page both succeed. On the laptop's
  own volume it made the touch of the file's pages cost 139 to 159 ms in two runs,
  where the sparse file's cost 19 to 51 ms, and every cluster service creates such
  a log in its first poll. Left for work item 505, which first finds out what a
  full volume does to a mapped log there.
- **The call through `libc`, in an `unsafe` block of `cavalry-shm`.** The crate may
  hold `unsafe`. Lost to `nix`, which the driver and the client already take, whose
  wrappers keep the block out of this crate: one more name on `cavalry-shm`'s list
  of dependencies, and no new crate in the lock file.
- **A test of the old kernel's path in a container with a seccomp profile**, as the
  read ran it. Lost: CI's job is not in a container, and a filter that the test's
  own run installs needs nothing of the runner but a C compiler.

## Evidence

All of it from one laptop, an Apple M5 Pro with macOS 26.6.1: the Linux runs in
Docker, `rust:1` on Linux 7.0.14 on aarch64, in debug builds. No run's number is a
bound, and nothing was run on the bench hosts.

- **The bus error, and its end.** The test of a driver whose directory has no room,
  five asks for a `cavalry:ipc` log, 201,330,688 bytes, run under
  `scripts/no-populate-advice.c`. On `master`'s source the test program was killed
  at the first ask, "signal: 7, SIGBUS", and left `publications/2.logbuffer` at its
  full length with the filesystem at 100%, in a `/dev/shm` of 64 MiB and in one of
  256 MiB with 100 MiB taken. With this decision each of the five asks is refused
  with error code 12, no log file is there after any, and the filesystem has the
  room it had, in both directories, with the advice and without it.
- **What a refusal costs.** The same five asks, timed by the client, so each time
  holds the round trip through the driver's conductor.

  | The directory | `master`, with the advice | This decision, with the advice | This decision, without it |
  | --- | --- | --- | --- |
  | `/dev/shm` of 64 MiB, 57,328 KiB free | 20.6 to 36.5 ms | 0.28 to 0.56 ms | 0.15 to 0.29 ms |
  | `/dev/shm` of 256 MiB, 151,536 KiB free | 44.0 to 54.8 ms, the first 186.3 ms | 0.15 to 0.83 ms | 0.14 to 0.41 ms |

  The refusal's message then reads "the file needs 201330688 bytes and the
  filesystem has" what it has "free".
- **A log that fits, on the old kernel's path.** The seven other tests of
  `crates/cavalry-client/tests/publication.rs` pass under the filter: publications
  are created, written, back-pressured and closed with every mapping touched page by
  page.
- **The allocation itself.** Four unit tests in `cavalry-shm`, Linux's: a sized file
  has its length and its blocks, which a file that was only given a length does not;
  a file written through has its length in zeros; the look at the free room says
  nothing where the filesystem reports no blocks and never less than is free; a
  length no file can have is refused before the filesystem is asked.
- **The failure after the file exists**, which ADR 0172's two tests make with a
  limit on the size of a file, now fails at the allocation on Linux, and each test
  still finds no file left.
- **macOS**, with the probe: the numbers are under Alternatives, and work item 505
  has what a full volume did to a mapped file in one short run.

## Consequences

- A driver whose directory has no room for a log refuses the log on every Linux
  kernel. ADR 0027's sentence, "a driver directory out of memory fails here, by
  name", is true of each, and "here" is the allocation, one step before the pages
  are touched.
- A refusal is cheap again. ADR 0172's Consequences give 15.5 to 23.1 ms for each
  refused ask, on the conductor's thread, with the filesystem full for the length of
  it. That part is superseded: an ask that cannot be met costs the driver one
  question of the filesystem, and the filesystem is never filled by it.
- The error an operator reads changed its first words, from "could not make the
  mapping resident" to "could not give the file its room", and has the two numbers
  where the look refused. Its code, its counter and the `doctor` check it names are
  as they were; that the check says nothing of room is work item 496.
- The creation of a log that fits costs one `fstatvfs` and one `fallocate` more, on
  the control plane. On a tmpfs the allocation takes the pages that the populate
  advice then only has to map. No call is added to any path a message takes.
- `cavalry-shm` takes `nix` on Linux, for `fallocate` and `fstatvfs`.
- The populate advice is no longer what finds a directory with no room. It is kept
  for what ADR 0027 chose it for: one call that makes the pages resident and
  writable.
- A filesystem that lies about its free room in the direction of less would refuse
  a file it could hold. None Cavalry's directory is put on is known to; the look
  counts every free block and skips a filesystem that reports none.
- What is not decided here, and whose it is.
  - macOS: a full volume is not refused there. Work item 505.
  - A memory limit on the driver's control group is not the filesystem's room. A
    tmpfs page is charged to the group that first takes it, at the allocation now,
    and a group at its limit meets the kernel's own handling of that, which this
    decision does not change.
  - The other three files, the counters file, the command ring and the broadcast
    buffer, are created at the driver's start. A start that fails for want of room
    now fails with the allocation's error on every kernel; the files it leaves are
    remade by the next start, as before, and ADR 0172's removal is the log
    buffer's alone.
  - Kernels before 3.5, where a tmpfs has no `fallocate`, are written through; that
    path's unit test runs, and no such kernel was run.

## References

- [ADR 0027](0027-pre-touch-shared-mappings-at-map-time.md): the pre-touch, and the
  sentence this makes true of every kernel.
- [ADR 0172](0172-remove-the-file-of-a-log-that-could-not-be-created.md): the
  removal of a log's file, and the cost this takes away.
- [ADR 0168](0168-add-a-services-counters-and-its-publication-to-its-member-when-it-first-opens-its-members-log.md):
  why every cluster service asks for a log of 192 MiB at its start.
- `spec/log-buffer.md`, "File"; `crates/cavalry-shm/src/allocate.rs`;
  `scripts/no-populate-advice.c`; the `justfile`'s `shm-exhaustion`.
- Work items 497, 481, 496 and 505.
