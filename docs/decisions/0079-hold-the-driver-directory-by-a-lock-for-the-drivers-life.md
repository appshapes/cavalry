# 0079. Hold the driver directory by a lock for the driver's life

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 3, 5, 6
- **Supersedes:** none. Amends `spec/client-protocol.md`, "The driver directory", within
  version 8: the directory gains a file, `pid`, and a starter's first step, a lock on
  the directory. The three files a client reads, their order and their bytes are
  unchanged, and nothing on the wire changes.

## Context

One driver runs in a directory, and a driver that starts has to tell a directory
whose driver is alive from one whose driver is gone. It told by the command ring's
heartbeat: fresh, and the directory is `driver-active`; stale, −1, or no readable
ring, and the directory is a dead driver's, which the starter removed and made anew.
The page said that of two drivers starting at the same moment "the second finds the
first's directory and waits or refuses", and that a driver "never removes a file
whose creator is alive".

Neither held while a driver was starting. The driver creates the directory, opens
and recovers its log directory, and only then creates `counters`, `command-ring` and
`broadcast-buffer`. Until the ring exists and is sized, a second starter finds a
directory with no ring, or with a ring of 0 bytes, takes it for a dead driver's and
removes it under the first. Card 250 measured it on `master`: 300 rounds of two
`Driver::start` on one directory, the second 0 to 15 ms after the first. In 148 the
second was refused with `driver-active`. In 84 it failed with an I/O error that named
no driver. In 57 the first failed, its directory removed under it. In 11 both
drivers' files ended up in one directory and the driver that ran could serve no
client until it was restarted.

The heartbeat has a second weakness, which no order of files mends. A driver whose
process is stopped for longer than the driver timeout, 10 s, by a debugger, `SIGSTOP`
or a paused virtual machine, has a stale heartbeat and is alive. A starter removed its
files. And two starters that both judged a directory dead could each remove it, the
second removing the first's new files.

Two `cavalry run` within a few milliseconds is what a service manager's restart
racing a start by hand does, and what two units with the default directory do at
boot.

## Decision

A driver holds its directory by an exclusive advisory lock of the operating system
on the directory itself, from before it reads or changes anything there until its
threads have stopped. The operating system drops the lock when the process ends,
however it ends. The driver writes its pid in a file there, `pid`.

A driver that starts creates the directory when it is not there and tries the lock
once, without waiting. Refused, a driver is running or starting there: the starter
answers `driver-active`, names the directory and the pid in `pid`, and changes
nothing.

With the lock, `pid` tells the starter whose the directory was. A pid there is a
driver of this version, which held this lock and has gone, whatever its heartbeat
says: the starter takes the directory at once. No `pid` is a new directory or a
driver of an earlier version, which holds no lock, and the starter goes by the
heartbeat as before. Then it writes its own pid, removes everything else in the
directory and creates its files.

The lock is the kind the log directory has had since ADR 0025, taken the same way,
with the standard library's `File::try_lock`. The driver crate gains no `unsafe` and
no dependency.

## Alternatives considered

- **Create `command-ring` first, `not-ready`, and write its magic when the other files
  are whole,** the card's first proposal: a second starter then always finds a ring
  that is `not-ready` and waits. Lost: it leaves the moment between the directory and
  the ring, which the log directory's recovery sits in today; it does nothing for a
  driver that is stopped and alive; and it does nothing for two starters that both
  judge a directory dead. Each of those is a removal of a live driver's files, which
  only something the operating system holds for a live process can rule out.
- **A lock on a file in the directory, `lock`.** Built first. Lost in its review: a
  file can be unlinked while its lock is held, by a cleaner of the temporary
  directory or by hand, and the next starter then locks a new file and holds nothing
  against the first. The directory cannot be taken from under its files that way.
- **Go by the heartbeat once the lock is taken, for every directory.** Built first.
  Lost in its review: a driver of this version that was killed leaves a heartbeat that
  is fresh for ten seconds, and a service manager's restart was refused for that
  long, naming a pid that had gone, though the lock already said so. The `pid` file
  is what tells a directory of this version from one of an earlier version.
- **Hold the lock for the start alone,** and go on telling a running driver by its
  heartbeat. Lost: it mends the start and leaves the stopped driver.
- **Wait for the lock** in place of refusing. Lost: the second `cavalry run` would
  hang for as long as the first runs, where a refusal that names the first is what
  an operator can act on.

## Evidence

- `crates/cavalry-client/tests/two_drivers.rs`: two `Driver::start` on one directory in
  two threads, the second from 0 to 20 ms after the first in even steps, each with a
  log directory of its own; a round is right when one driver runs, the other is
  refused with `driver-active`, and a client of the one that runs adds a
  subscription and a publication and gets a message through. 1,000 rounds, release
  build:

  | Host | `master` at e86e721 | With the lock |
  | --- | --- | --- |
  | Apple M5 Pro, macOS 26 | 526 rounds wrong | 0 rounds wrong, 57 s |
  | Intel i9-13900H, Linux 7.0 | 229 rounds wrong | 0 rounds wrong, 115 s |

  On `master` the wrong rounds are of two kinds. The second starter failed with an
  I/O error that names no driver, `File exists` on the counters file or the command
  ring, or on the directory: 335 on macOS, 197 on Linux. Or the first starter failed,
  its directory removed under it while it made its files: 191 on macOS, 32 on Linux.
  The test runs 100 rounds in `just test`, 6 s.
- `crates/cavalry-driver/tests/driver.rs`: the running driver's pid is in `pid`, and the
  refusal names it; a driver that stopped leaves its directory and the next driver
  removes what it left, a stray file and a stray directory included; a directory
  with a fresh heartbeat and a pid in `pid`, as a killed driver of this version
  leaves it, is taken over at once; a directory with a fresh heartbeat and no `pid`,
  as a driver of an earlier version makes it, is refused with the ring's creator pid,
  nothing removed and no `pid` written, and is taken over once its heartbeat is −1.
- `crates/cavalry-driver/src/conductor.rs`: a second `prepare_dir` while the first
  holds the lock is `driver-active` with the holder's pid and removes nothing, and
  takes the directory once the first lets go.
- `just bench` on the laptop, Apple M5 Pro, unpinned, while other sessions kept it busy
  (load average 6.5): two runs of each build, `master` at a9ad4bd and this change, in
  turn. Two runs of one build differ more than the builds do; `one_way_32_exclusive`
  on `master` read p99.99 79,167 ns in one run and 9,751 ns in the next. So one row,
  `one_way_32_client`, was run alone twelve times on each build in the order
  `master`, change, change, `master`: the p50 is 83 ns on both, and the median p99.99
  is 10,539 ns on `master`, from 7,627 to 29,359, and 11,775 ns with the change, from
  5,295 to 44,703. The laptop shows no difference; the gate hosts' rows in CI are
  the judgement. Allocations per message: 0 in every run. The change is in the
  driver's start and stop, and no message passes through it.

## Consequences

Easier: exactly one of two drivers started together runs, at any offset; the refusal
names the driver that holds the directory even while that driver is still starting;
a stopped driver keeps its files; and a driver that was killed is replaced at once,
where the heartbeat held the next start up for ten seconds. The page's sentence that
a driver never removes a file whose creator is alive is true of this version.

Harder: the directory holds one more file, and a starter no longer removes the
directory itself, only what is in it. The pid a refusal names is read 10 ms after the
refusal, from a file the holder writes once it has the lock: a holder that is
slower than that to write it is named by no pid, or by the pid of the driver before
it, and the message says where the pid came from.

A driver of an earlier version beside one of this version is told as before, by its
heartbeat, with the weakness it had: an earlier version's starter does not look at
the lock, and can still remove the directory of a driver of this version that is in
the first milliseconds of its start. Mixed versions run; the start is safe once both
are of this version.

A file system with no advisory locks on directories would refuse the lock with an
error, and the driver would not start there. `/dev/shm` and `/tmp` on Linux and the
temporary directory on macOS have them: a second lock on a locked directory is
refused on each, tried on 2026-11-11.

Revisit if a driver directory ever has to live on a file system without such locks,
and if `doctor` should try the lock to tell a driver that is stopped from one that
has gone.

## References

- `spec/client-protocol.md`, "The driver directory".
- `crates/cavalry-driver/src/conductor.rs`, `prepare_dir`, `DirLock` and `holder_pid`;
  `crates/cavalry-driver/src/lib.rs`, `Driver`; `crates/cavalry-driver/src/error.rs`,
  `DriverError::Active`.
- `crates/cavalry-client/tests/two_drivers.rs`; `crates/cavalry-driver/tests/driver.rs`.
- ADR 0025 (the log directory's lock), ADR 0072 (a client that connects while a
  driver creates its files).
- Card 250.
