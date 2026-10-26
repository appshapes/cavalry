# 0027. Pre-touch every shared mapping when it is made, with no key to turn it off

- **Status:** Accepted
- **Date:** 2026-10-26
- **Invariants affected:** 2, 3, 5
- **Supersedes:** none

## Context

Card 136. The second bench host, `kip` (card 91, an Intel Core i9-13900H tuned on three
P-cores), is repeatable and its single-thread rows match `bench`'s, yet its `one_way`
p99.99 under `--pin 6,8,10` is 1,613 ns against `bench`'s 600 ns, within 2 percent over
21 runs. Tracing on kip during one `cavalry-bench --messages 1000000 --pin 6,8,10
--repeat 1` (the `setup-second-host` session, 2026-10-26, read-only) found the cause in
Cavalry, not in the host. `exceptions:page_fault_user` counted 332,471 faults in the run:
the publisher 107,767, the client's subscriber 97,911, the driver's conductor 49,162 and
its recorder 49,127; by 4 MiB bucket on the isolated CPUs, exactly 2,048 per bucket across
the log buffers, one fault per 4 KiB page per mapping, and three per page over the
counters file and the command ring. `sched:sched_waking` to the isolated CPUs named only
Cavalry's threads, never a kernel worker: the publisher woke the client's subscriber
29,788 times, the subscriber woke the recorder 10,458 times, the recorder woke the
subscriber 9,561 times and the publisher woke the recorder 7,087 times.

The code explains both counts. No mapping in `cavalry-shm` is populated or pre-touched,
and the driver, every client and the bench's subscriber each map a log buffer in a
mapping of their own, so the first touch of every page of every term is a page fault in
each mapping, inside the measured loops: a kernel entry per 64 messages of 64-byte
frames, after a warm-up that covers one term of three. Two threads faulting the same
fresh page race on the page's lock; the loser sleeps and the winner wakes it, which is an
interrupt when the sleeper's CPU is tickless. At kip's rate those sleeps are the p99.99.
Transparent huge pages are `never` on both hosts and `/dev/shm` carries no `huge=`
option, so `bench` hides the same faults only because its subscriber trails its publisher
by more (one_way p50 250 ns against kip's 205) and loses fewer races.

Aeron has the fix: `aeron.pre.touch.mapped.memory`, which touches every page of a
mapping when it is made. Aeron defaults it off, because the touch costs time at map time
and commits memory the file would otherwise take lazily.

## Decision

Every mapping `cavalry-shm` makes, the log buffers, the counters file, the command ring
and the broadcast buffer, in every mapper, the driver, a client, the recorder, `doctor`
and the bench, is made resident when it is made, in `create` and `open`, on the control
plane. On Linux, `madvise(MADV_POPULATE_WRITE)` for a writable mapping and
`MADV_POPULATE_READ` for a read-only one fault every page in with one call and report a
page the kernel cannot back as an error at creation, by name, with the `--dir` to change
and the `doctor` check that explains it; a kernel without them (before 5.14) and every
other system fall back to one read per 4 KiB page. There is no key to turn it off: a
page fault is a kernel entry and a sleep on the data plane, which invariant 2 forbids,
and a knob whose only use is a faster-looking benchmark is refused by `CLAUDE.md`.

## Alternatives considered

- **Aeron's key, default off.** Lost to invariant 5: the default would be the one that
  faults, and a new key needs a reason its default cannot be right, which this one has
  not; and to `CLAUDE.md`, which refuses a knob added to make a benchmark pass.
- **Huge pages, by host policy (`shmem_enabled`) or `MAP_HUGETLB`.** Lost: the spec's
  64 MiB term still faults 32 times per mapping, the race remains, and the fix would live in the
  host's configuration, where `doctor` can only recommend it; `MAP_HUGETLB` needs reserved
  pages and fails on a laptop.
- **One mapping per process, shared between the client and an embedded driver.** Lost:
  the driver and its clients are separate processes by design, and the bench embeds the
  driver only to measure the user's path (ADR 0015); the subscriber's own read-only
  mapping is what a client has.
- **Pre-touching with a write to each page.** Lost: a mapping opened after the creator may
  have a peer writing the page, and a read-modify-write of one byte could revert that
  write; a read touches the page as surely, and on Linux the populate advice makes the
  page writable without writing it.
- **`MAP_POPULATE` at `mmap`, memmap2's `MmapOptions::populate`.** Lost: on a shared
  mapping it populates for reading only, so the first store to each page of a writable
  mapping still faults to make the page writable, and `mmap` does not fail when a page
  cannot be populated, so a directory that cannot hold the log is still found by the
  publisher's bus error; the populate advice does both.
- **Doing nothing, and calling kip's number the host's.** Lost: the trace names
  Cavalry's own threads, and `bench` pays the same faults with less to show for them.

## Evidence

The kip trace above, on card 91 in full. The pull request that implements this carries
the same trace after the change, the residency test over `/proc/self/smaps` on Linux,
and the bench host's rows before and after; what it proves goes in its commit message,
since an ADR is accepted on the evidence that informed it.

## Consequences

- A log buffer's pages are allocated at `add_publication` and `add_subscription`, not as
  the stream reaches them: some tens of milliseconds for the spec's 192 MiB log on the control
  plane, and a visibly larger resident set from the start. A bound moves with it
  (invariant 3): tmpfs charges a page against the mount's `size=` and against memory
  when the page is allocated, not when `ftruncate` sizes the file, so a log that cost
  the host the pages its stream had reached now costs the whole log, and a host holds
  as many live publications and subscriptions as its driver directory holds whole logs,
  192 MiB each for the spec's three 64 MiB terms, and the next is refused by name. That
  is the trade this decision accepts under invariant 3: an explicit bound, returned to
  the caller at creation, in place of a directory that ran out under a live stream.
- A driver directory that cannot hold the log fails at creation with an error that names
  `--dir` and `doctor`, instead of a bus error in a publisher later. The changelog says
  so.
- On systems without the populate advice, a writable mapping's first store to a page may
  still take a fault that marks the page dirty; the Linux gate is where the budget is held.
- `docs/how-to/run-the-bench.md` says how to see page faults on the pinned CPUs with the
  kernel's tracepoints, read-only, so a host that faults again is found the same way.

## References

- Card 136; card 91, which holds the trace and the host records.
- `crates/cavalry-shm/src/pretouch.rs`; `crates/cavalry-shm/tests/pre_touch.rs`.
- Aeron's `aeron.pre.touch.mapped.memory` (`MediaDriver.Context.preTouchMappedMemory`).
- [ADR 0015](0015-measure-in-process-with-per-runner-baselines.md),
  [ADR 0016](0016-run-the-latency-gate-on-a-dedicated-host.md).
