# 0175. Give a new shared file its room before it is mapped

- **Status:** Accepted
- **Date:** 2026-11-26
- **Invariants affected:** 3, 4
- **Supersedes:** ADR 0027 in one part, what happens where a driver directory cannot
  hold a file: on Linux that is found before the file is mapped, with the populate
  advice or without it, and no longer by the populate advice alone. ADR 0172 in one
  part, what a refused log costs. Changes one sentence of `spec/log-buffer.md`,
  "File", within version 1: no byte of the file changes and no version moves.

## Context

The driver creates each shared file, a log buffer, the counters file, the command
ring and the broadcast buffer, in steps: it creates the file, gives it its length,
maps it, and makes every page resident (ADR 0027). A file that is only given its
length is sparse. The filesystem finds a page for it when a mapping first touches
one, and when it has none the kernel answers the touch with a bus error, which ends
the process.

ADR 0027 made that an error on Linux by the populate advice,
`madvise(MADV_POPULATE_WRITE)`, which in its words reports "a page the kernel cannot
back as an error at creation, by name". Of a kernel without the advice, before 5.14,
it says that it and every other system "fall back to one read per 4 KiB page". It
does not say what that read meets in a directory with no room. The second
independent read of work item 492 ran it: under a seccomp profile that answers the
advice as such a kernel does, a driver whose directory had no room for a log was
killed by a bus error, and the file stayed, with the filesystem full (work item 497).

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
four places a file is created, and does three things.

1. It asks the filesystem for the room, with `fallocate`. The filesystem gives all
   of it or fails with its own error, and nothing is written and no page is touched.
   Every signal is held off the calling thread for the length of the call, and the
   thread's mask is put back after it: a tmpfs before Linux 6.11 answers a signal
   that arrives during the call by undoing the whole allocation. A call that is
   interrupted all the same, as by a stop or a freeze of the process on such a
   kernel, which no mask holds off, is made again, eight times at the most.
2. Where the filesystem cannot allocate ahead and says so, `EOPNOTSUPP` or `ENOSYS`,
   the file is written through with zeros, which asks for the same room a block at
   a time and fails with an error and never with a signal.
3. On a tmpfs, and on no other filesystem, it first asks how much room is free,
   with `fstatfs`, and refuses a file longer than that with nothing more asked. A
   tmpfs that cannot meet an allocation takes all the room it has before it fails
   and gives it back; the look saves that. A tmpfs that reports no blocks at all, as
   one with no size does, is not looked at. The look only ever refuses: room can go
   between it and the allocation, and the allocation is what the rest rests on.

Then the file is mapped and its pages are made resident as ADR 0027 has it, by the
populate advice or by one read a page. Either now meets pages the filesystem has
already given. On a tmpfs, which the default directory is on wherever `/dev/shm`
exists, a page given stays the file's, so no touch of a new file is answered with a
bus error for want of room, with the advice or without it. Cavalry sets no oldest
kernel for this reason.

A failure of any of the three is the creation's error, and a log buffer's file is
removed as ADR 0172 has it. The error says "could not give the file its room" and
gives the system's error. It says that the filesystem has no room only where the
error is the filesystem's own for that, `ENOSPC`, or for a quota, `EDQUOT`, with
the file's length and the free room where the look refused. A call still
interrupted at its ninth try says that, and every other error says that the
refusal was not for want of room.

On every other system a file is given its length alone, as before. macOS is the one
Cavalry runs on: what was measured there, and why it is left, is under Alternatives
and is work item 505.

The gate `just shm-exhaustion` runs on Linux in a directory with no room for a log.
It runs its test of a driver a second time as a kernel without the advice runs it:
`scripts/no-populate-advice.c` puts a seccomp filter on itself that answers the two
populate advices with `EINVAL`, which is such a kernel's answer, checks that the
answer is that, and runs the command under it. The other tests of the same program
then run under it too, so that a log that does fit is created and used by the path
such a kernel takes. And it runs three tests of the allocation in that directory:
the allocation's own refusal with no look before it, a write-through's refusal, and
a file removed and asked for again at once.

## Alternatives considered

- **An oldest kernel, 5.14, checked at the driver's start.** One line of code and a
  `doctor` check. Lost: it turns away RHEL 8, Ubuntu 20.04, Debian 11 and Amazon
  Linux 2 as hosts, containers on them included, for a failure that asking the
  filesystem first does not have.
- **`fallocate` alone, with no look at the free room.** It closes the bus error and
  was measured first. Where the log is longer than the whole tmpfs the kernel
  refuses at once, 0.17 to 0.97 ms an ask. Where the log would fit an empty tmpfs
  and does not fit the free room, the kernel takes all the free room before it fails
  and gives it back: 44.3 to 50.1 ms an ask with 151,536 KiB free, no better than
  the populate advice there, 40.2 to 54.8 ms in two runs, and the filesystem is full
  for its other users meanwhile. Lost to the look first on a tmpfs, which makes both
  cases 0.13 to 0.41 ms.
- **The look on every filesystem.** This decision was first written so. The
  independent read of it made filesystems of 800 MiB on loop devices, left 100 MiB
  free, removed the file that took the rest and asked for a log at once. XFS and
  btrfs still counted the removed file: the look refused a log that fit, five times
  of five on each, and with the look silenced the same ask was given, two of two.
  On XFS a log created, removed and asked for again at once was refused three times
  of three, the filesystem saying 61,411,328 bytes free where 250 MiB were; 3 ms
  later the answer was right. ext4 was given all five. And those three refuse an
  allocation they cannot meet in 0.01 to 1.09 ms, so a look saves nothing there.
  Lost: the look is taken on a tmpfs alone, which counts its blocks as they are
  taken and gives a removed file's back before the removal returns.
- **No look on a tmpfs mounted with huge pages**, where the read found the look
  wrong by about 2 MiB a file (Consequences). Whether a mount has them is in
  `/proc/self/mountinfo` and nowhere in the filesystem's own answer: a file to read
  and search at every creation, for a refusal within 2 MiB a file of a full
  filesystem. Lost to saying the limit.
- **The look at the free room alone, and no allocation.** Lost: room can go between
  the look and the touch, and then the touch is the bus error again. The look is
  there for the cost and the allocation for the guarantee.
- **Leave an interrupted call to its eight tries.** This decision was first written
  so, on the reasoning that the driver's stop interrupts the call once. The read
  found in the kernel's source that a tmpfs's `fallocate` looks for a pending signal
  at each page and undoes everything for any signal, from 3.5, where it first has
  the call, to 6.10; 6.11 and later answer only a signal that ends the process. An
  allocation of 192 MiB took 21.6 ms on the laptop, so a thread that takes a signal
  more often than that, as under a profiler's timer in a program that embeds the
  driver, would fail every try. Lost to holding the signals off.
- **Allocate in pieces, each shorter than a signal's period.** It needs no signal
  mask. Lost: a failure after the first piece leaves the earlier pieces taken until
  the file is removed, the length of a piece is a guess at a period nobody states,
  and one call under a mask is simpler to say.
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
- **The calls through `libc`, in `unsafe` blocks of `cavalry-shm`.** The crate may
  hold `unsafe`. Lost to `nix`, which this crate takes since ADR 0179 and the
  driver and the client took before it, whose wrappers keep the blocks out of this
  crate: one feature more of it, `signal`, and no new crate in the lock file.
- **A test of the old kernel's path in a container with a seccomp profile**, as the
  read ran it. Lost: CI's job is not in a container, and a filter that the test's
  own run installs needs nothing of the runner but a C compiler.

## Evidence

All of it from one laptop, an Apple M5 Pro with macOS 26.6.1: the Linux runs in
Docker, `rust:1` on Linux 7.0.14 on aarch64, in debug builds. No run's number is a
bound. No kernel but that one was run: what is said of an older one is from a
filter that answers as it does, or from the kernel's source, and says which.

- **The bus error, and its end.** The test of a driver whose directory has no room,
  five asks for a `cavalry:ipc` log, 201,330,688 bytes, run under
  `scripts/no-populate-advice.c`. On `master`'s source the test program was killed
  at the first ask, "signal: 7, SIGBUS", and left `publications/2.logbuffer` at its
  full length with the filesystem at 100%, in a `/dev/shm` of 64 MiB and in one of
  256 MiB with 100 MiB taken. With this decision each of the five asks is refused
  with error code 12, no log file is there after any, and the filesystem has the
  room it had, in both directories, with the advice and without it.
- **What a refusal costs on a tmpfs.** The same five asks, timed by the client, so
  each time holds the round trip through the driver's conductor. `master`'s are of
  one run of the five, and this decision's of two.

  | The directory | `master`, with the advice | This decision, with the advice | This decision, without it |
  | --- | --- | --- | --- |
  | `/dev/shm` of 64 MiB, 57,328 KiB free | 20.6 to 36.5 ms | 0.14 to 0.41 ms | 0.14 to 0.38 ms |
  | `/dev/shm` of 256 MiB, 151,536 KiB free | 44.0 to 54.8 ms, the first 186.3 ms | 0.14 to 0.28 ms | 0.13 to 0.41 ms |

  The refusal's message then reads "the file needs 201330688 bytes and the
  filesystem has" what it has "free".
- **A log that fits, on the old kernel's path.** The seven other tests of
  `crates/cavalry-client/tests/publication.rs` pass under the filter: publications
  are created, written, back-pressured and closed with every mapping touched page by
  page.
- **The allocation itself.** Unit tests in `cavalry-shm`, Linux's. On the test's own
  directory: a sized file has its length and its blocks, which a file that was only
  given a length does not; a file written through has its length in zeros; a length
  no file can have is refused before the filesystem is asked; the look answers for a
  tmpfs that reports its blocks and for nothing else. With the allocation's answer
  given by the test, which fails unless every signal is held off the thread when it
  is asked, and the thread's mask afterwards what it was: `EOPNOTSUPP` and
  `ENOSYS` are written through; a call interrupted twice is given at its third try;
  one interrupted at every try ends at its ninth and says so, and not that there is
  no room; of six errors only `ENOSPC` and `EDQUOT` are said to be a want of room; a
  look that finds less than the file refuses with nothing allocated. In the gate's
  directory with no room: the allocation refuses by itself, with the look silenced,
  and the room is what it was; a write-through is refused by a write; and a file of
  three quarters of the free room is given, removed and asked for again at once,
  eight times, and never refused.
- **Filesystems on a device**, by the independent read's probe, on loop devices of
  800 MiB with 100 MiB free. A sparse file of a log's length, mapped: one read a
  page passed and a store was a bus error, on ext4, XFS and btrfs alike. Asked for
  with `fallocate` first, each refused, in 0.06, 0.01 and 1.09 ms. So the
  allocation also takes away a bus error at a publisher's store where `--dir` names
  such a filesystem with no room. Each was also refused with the allocation
  answered as a filesystem without it answers, by the write-through, in 8 to 43 ms,
  with no file left. An unsized tmpfs and one with `huge=always` each created a
  log; a bind mount of the laptop's own filesystem created one of 3 MiB, and the
  container's overlay gave an allocation of 8 MiB.
- **Signals**, by the same read. On 7.0.14 a timer signal every 500 µs never
  interrupted the call, as `mm/shmem.c` at 6.11, at 6.12 and on the kernel's main
  branch says of a kernel that new. The older kernels' behaviour is from the same
  file at 3.5, 4.18, 5.10, 5.14, 5.15, 6.1 and 6.6 to 6.10, and was not run. At
  5.10 a signal that is held off wakes no thread and sets no thread's pending flag,
  one sent to the process included; a stop or a freeze of the process sets the
  flag whatever the mask.
- **The failure after the file exists**, which ADR 0172's two tests make with a
  limit on the size of a file, now fails at the allocation on Linux, and each test
  still finds no file left.
- **macOS**, with the probe: the numbers are under Alternatives, and work item 505
  has what a full volume did to a mapped file in one short run.

## Consequences

- A driver whose directory is on a tmpfs with no room for a log refuses the log
  on a Linux kernel with the populate advice and on one without it. What ADR 0027 says
  of the advice, "an error at creation, by name", is true without it, and the step
  that fails is the allocation, one before the pages are touched.
- A refusal is cheap again on a tmpfs. ADR 0172's Consequences give 15.5 to 23.1 ms
  for each refused ask, on the conductor's thread, with the filesystem full for the
  length of it. That part is superseded where the look refuses: such an ask costs
  the driver one question of the filesystem, and the filesystem is not filled by
  it. Where room goes between the look and the allocation, the allocation fills and
  gives back: the read ran two programs that each asked for a log where one fit,
  and both were refused, six times of six, in 23 to 26 ms each. One conductor
  creates one file at a time, so that takes a second driver or another program on
  the same tmpfs. A cluster service asks again a second after a refusal (ADR
  0168); any other client is handed the refusal and asks again or does not.
- The error an operator reads changed its words, from "could not make the
  mapping resident" to "could not give the file its room", and has the two numbers
  where the look refused. Its code, its counter and the `doctor` check it names are
  as they were; that the check says nothing of room is work item 496.
- The creation of a log that fits costs one `fallocate` more, two calls for the
  signal mask and, on a tmpfs, one `fstatfs`, on the control plane. On a tmpfs the
  allocation takes the pages that the populate advice then only has to map. What a
  fitting log's creation takes with this decision against without it was not
  measured. No call is added to any path a message takes.
- A signal sent to the thread that creates a file waits for the allocation's end.
  The read's one measurement of the call for 192 MiB was 21.6 ms.
- `cavalry-shm`, which takes `nix` with its feature `fs` since ADR 0179, takes its
  feature `signal` too, for the thread's signal mask.
- The populate advice is no longer what finds a directory with no room. It is kept
  for what ADR 0027 chose it for: one call that makes the pages resident and
  writable.
- What this does not hold, and whose it is.
  - A tmpfs mounted with huge pages, `huge=always` and its like, which no
    container's or system's default `/dev/shm` is. A file there is charged for the
    whole of its last huge page, and the part past the file's end is not counted
    as free, though an allocation that would otherwise fail takes it back. The read
    ran it on a tmpfs of 385 MiB: after one log the filesystem said 200,278,016
    bytes free, the look refused a second log of 201,330,688, and with the look
    silenced the second was created. So the look can refuse a file that fits, by up
    to about 2 MiB for each file the filesystem holds. Nobody's yet: the way out is
    under Alternatives.
  - A filesystem that does not keep for a file the room it allocated ahead. By the
    read of the kernel's source and no run, btrfs at 5.10 asks for room again when a
    page that was allocated ahead is first written, and has none to fall back on: a
    store into such a page of a full btrfs can still be a bus error there. The
    sentence in `spec/log-buffer.md` is therefore of a tmpfs. `doctor` already warns
    of a driver directory that is not in memory.
  - A memory limit on the driver's control group is not the filesystem's room, and
    here something may change. A tmpfs page is charged to the group that first
    takes it, which is now the allocation. The read ran a log of 192 MiB under a
    limit of 128 MiB, three times each way, and all six containers ended killed,
    with status 137. Without this decision the shell that had started the program
    lived to print that the program was killed. With it the shell printed nothing.
    The read takes that to be the kernel killing the shell, the container's first
    process, since pages that are allocated and not yet mapped count against no
    process when the kernel chooses; it read no kernel log to say so. Where the
    driver is the container's first process the two are the same. A `doctor` check
    of the group's limit against a log's size goes with work item 496.
  - macOS: a full volume is not refused there. Work item 505.
  - The other three files, the counters file, the command ring and the broadcast
    buffer, are created at the driver's start. A start that fails for want of room
    now fails with the allocation's error; the files it leaves are remade by the
    next start, as before, and ADR 0172's removal is the log buffer's alone. A
    filesystem may leave such a file holding part of its room, as ext4 left 88 MiB
    of a refused 192, until then.
  - Kernels before 3.5, where a tmpfs has no `fallocate`, are written through; that
    path's tests run with the answer given by the test, and no such kernel was run.
  - A filesystem on a network, NFS and its like, and ZFS: none was run. A file
    written through to a server may be told that the server is full only later
    than the write, by the read's reasoning and no run.
  - The oldest kernel a driver starts on. By a search of the crates, the populate
    advice is the only thing Cavalry asks of a kernel that is newer than 3.5; no
    kernel but the laptop's was run.

## References

- [ADR 0027](0027-pre-touch-shared-mappings-at-map-time.md): the pre-touch, and the
  sentence this makes true without the populate advice.
- [ADR 0172](0172-remove-the-file-of-a-log-that-could-not-be-created.md): the
  removal of a log's file, and the cost this takes away.
- [ADR 0168](0168-add-a-services-counters-and-its-publication-to-its-member-when-it-first-opens-its-members-log.md):
  why every cluster service asks for a log of 192 MiB at its start.
- `spec/log-buffer.md`, "File"; `crates/cavalry-shm/src/allocate.rs`;
  `scripts/no-populate-advice.c`; the `justfile`'s `shm-exhaustion`.
- Work items 497, 481, 496 and 505.
