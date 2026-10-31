# 0034. Run the latency gate's second host on grip and the soak host on kip

- **Status:** Accepted
- **Date:** 2026-10-31
- **Invariants affected:** 2, 3, 6
- **Supersedes:** none. Settles the "second host" of ADR 0016's Revisit: the second
  gate host is `grip`; `kip`'s run as the second host, 2026-10-29 to 2026-10-31, the
  day `bench (kip)` was removed, stands as the record it is.

## Context

Three machines are reserved for Cavalry. `bench`, an AMD Ryzen 7 2700X, has run the
latency gate since 2026-10-12 (ADR 0016). `kip`, a Minisforum MS-01 with a hybrid Intel
Core i9-13900H and 64 GB, was tuned on three P-cores on 2026-10-26, joined to `bench` by
a direct cable with hardware timestamps and chrony across it, and has run the gate's
second job, `bench (kip)`, since 2026-10-29 (card 91, card 161). `grip`, a Dell with an
Intel Core i7-10700, eight cores under one L3, 16 GB and a 256 GB NVMe drive, arrived
on 2026-10-29 as a stock Ubuntu Server 26.04.1 install, reserved by card 159 for the
soak host: a driver recording for days, with `doctor` and the counters sampled, where
invariant 3 is checked at the scale of a week rather than a CI job's seconds. A soak
host is by design untuned, since what it looks for is growth, not nanoseconds.

The gate compares a run's median p99.99 with the slowest run over every process the
host has run at the baseline's tree, three at least (ADR 0033), so a host whose medians
wander from one process to the next widens its own band and gates less. On `kip` the
exclusive claim row's median read anywhere from 102 to 148 ns over the eight processes
at the tree of 4a09b6e, with no cause on record after `perf c2c` found nothing to
realign (card 168); the concurrent and client claim rows moved by a tenth. On `bench`
the same row read 149 to 160 over twelve processes at that tree, and the concurrent row
140 to 150. On 2026-10-31 Rjae asked for a pinned stock run on `grip` before the two
roles were given out, so that the choice rested on a measurement.

## Decision

`grip` is the latency gate's second host and `kip` the soak host. `grip` is tuned by
`bench/host/setup-ubuntu.sh` with CPUs 4 to 7 isolated and 0 to 3 for housekeeping,
runs the organization runner `grip` with the label `grip-linux`, and carries the job
`bench (grip)` in `.github/workflows/ci.yml`, pinned to CPUs 4, 5 and 6, against its
own profiles `grip-linux` and `grip-linux-unchecked`, whose baselines are recorded
from the job's first three processes at one tree (ADR 0033). `bench (kip)` runs beside
it until `bench (grip)` has been green over a handful of pushes to `master`; the change
that then removes `bench (kip)`, adds `bench (grip)` to the required checks of `master`
and un-tunes `kip` by the set-up script's own undo marks this decision Accepted. `kip`
then takes card 159's soak set-up, and its 64 GB and 4 TB drive carry the recording.

## Alternatives considered

- **`kip` stays the gate's second host and `grip` is the soak host, as card 159 was
  written.** Lost: the gate wants the host whose rows hold still, and the soak host
  wants nothing of a host but room and days. `kip`'s 46 ns process-to-process band on
  the exclusive claim row is a third of the row, has no cause on record, and the
  likeliest one is the hybrid part itself; `grip`'s band on the same row, pinned at
  stock, is 2 ns. Giving the gate the host that wanders and the soak the host that
  holds still serves neither.
- **Three gate hosts.** Lost: a third job on every push buys no coverage the two hosts
  do not give, costs twenty minutes of a third machine per push, and leaves the soak
  with no host at all. A soak host is a different job, not a third copy of this one.
- **Wait for card 168's cause before choosing.** Lost: the card has been open since
  2026-10-29, `perf c2c` on `kip` showed the publisher's rare modified-line hits at 136
  to 190 cycles and nothing to realign, and the week of soak that card 159 wants
  cannot start until a host has the role. If the cause is found and is a tuning, `kip`
  can return under a new decision.
- **Tune `grip` first and compare the two tuned.** Lost for now: the stock probe already
  separated the hosts by an order of magnitude on the band that matters, and the tuned
  comparison follows for free from `bench (grip)`'s first processes, which the
  acceptance of this decision reads.

## Evidence

Pinned stock run on `grip`, 2026-10-31: tree 88424f7, `just bench --messages 5000000 --repeat 21 --pin 2,4,6`, both profiles, three
processes each, no isolation, the powersave governor, SMT on with the siblings 10, 12
and 14 idle, clock step 1 ns. Medians of the 21 runs' p99.99, in nanoseconds, by
process:

| Row | grip checked | grip unchecked | kip checked, 8 processes | bench checked, 12 processes |
| --- | --- | --- | --- | --- |
| `claim_commit_32_exclusive` | 84, 84, 86 | 81, 79, 79 | 102 to 148 | 149 to 160 |
| `claim_commit_32_concurrent` | 90, 90, 95 | 90, 88, 88 | moves by a tenth | 140 to 150 |
| p50 of the exclusive row | 16 to 17 | 16 to 17 | 18 to 19 | 69 to 80 |

`grip`'s runs on the exclusive row spanned 72 to 86 ns with single stalls of 171 to
1,411 ns in three of the 126 runs, the untuned host's interrupts; its cross-thread rows
(`claim_commit_32_client`, `_recorded`, every `one_way_32_*` row) read their p99.99 in
microseconds, 1.2 to 15 µs at the median with maxima to 516 µs, as every untuned host's
do, and say nothing about the part until it is tuned. `kip`'s and `bench`'s numbers are
the processes recorded in `bench/baseline.json` at the tree of 4a09b6e (ADR 0033; cards
168 and 170). The result files are on card 159, with `lscpu -e` of the part.

Tuned, 2026-10-31: `bench/host/check.sh 4,5,6` on `grip` reports isolated and tickless
CPUs 4 to 7, SMT off, the performance governor, turbo off, idle states POLL and C1 on
CPU 4, huge pages never, no swap, two timers active (`logrotate`, `fstrim`); the
isolated CPUs' `LOC` interrupt count read 253 to 263 against 8,826 to 166,739 on the
housekeeping CPUs. The baselines and `bench (grip)`'s first processes are this
decision's acceptance evidence and go in the commit that accepts it.

Acceptance, 2026-10-31 *(acceptance)*: `grip`'s baselines, `grip-linux` and
`grip-linux-unchecked`, were recorded at 0782ec5 from four of its job's processes
(master run 41196273792, attempts 3 to 5, and the baselines pull request's own run,
joined when it read the checked client claim row above the three's band) and merged as
c8b8015; master's `bench (grip)` was then green on three pushes in a row, c8b8015 (run
41202271712), e5943f9 (41203122340) and 068647d (41204043241). The three hosts' checked
`claim_commit_32_exclusive` row, from their baselines, the per-process medians of
p99.99 in ns: `grip` 77, 77, 75 and 74 over four processes (runs 73 to 92); `bench`
150, 150 and 150 over three (runs 120 to 180, one 10 ns step); `kip` 124, 106, 102,
134, 148, 107, 123 and 105 over eight (runs 96 to 162). The concurrent row: `grip` 79
to 81, `bench` 140 to 150, `kip` 154 to 166. `grip` holds a row's median within 3 ns
from one process to the next, `bench` within one step, and `kip`'s exclusive row
wanders by 46 ns, the band card 168 asks about, which is now a question about the soak
host. The accepting change removes `bench (kip)` from `ci.yml`; the two steps outside
the repository, adding `bench (grip)` to the required checks of `master` and un-tuning
`kip` by the set-up script's own undo marks, follow its merge by hand, and card 159
records them; `kip`'s baselines stay in `bench/baseline.json` as the record of its run
as the second host, compared by nothing.

## Consequences

Easier: both gate hosts hold a row's median within a few steps from one process to the
next, so the slowest-run rule of ADR 0033 gates with a band the hardware set and not
one process's wander. Card 159's soak starts on a host with 64 GB and room for a log
directory of its own, and card 168, the exclusive row's band, becomes a question about
the soak host, which no merge waits on.

Harder: the cable and the chrony link that joined `bench` and `kip` for the UDP rows are
on the host that leaves the gate; when the UDP rows come, the cable moves to `grip`'s
wired port `eno2`, an Intel I219-LM with hardware timestamps, and the set-up page's
link steps are run again there. `kip` is un-tuned by deleting the files
`setup-ubuntu.sh` names, unmasking its timers, `update-grub` and a reboot, and its
runner is removed, as card 159's soak host wants a stock machine; nothing is
reinstalled. Until the acceptance, three bench jobs run on every push. The change that
proposes this decision touches `bench/host/`, so it ends a tree for every host's
baselines (ADR 0033): `bench`'s and `grip`'s are recorded from the first three
processes of the new tree, as the set-up page's step 7 says, and `kip`'s are not; a red
`bench (kip)` at a later tree is rerun or left, never joined and never re-recorded,
since the job is required by nothing and goes with the acceptance. `grip` reaches
the network over Wi-Fi alone today, so a reboot that does not come back needs a hand on
the machine, as kip's and bench's do.

Revisit: when card 168 names `kip`'s cause and it is a tuning rather than the part, to
decide whether `kip` returns as a third gate host or a replacement; when the UDP rows
need the second host's cable; when `grip`'s first baselines show a band wider than
the stock probe's, which would say the tuning, not the part, set the band.

## References

- `.github/workflows/ci.yml`, the `bench-grip` job; `bench-kip`, the job this acceptance
  removed;
  `bench/host/setup-ubuntu.sh`, `bench/host/check.sh`
- `docs/how-to/set-up-a-bench-host.md`, "Further hosts"; `docs/how-to/run-the-bench.md`
- ADR 0016 (the dedicated host), ADR 0033 (the baseline from every process at one
  tree); cards 91, 159, 161, 168 and 170
