# 0033. Record a bench host's baseline from three processes of master's own runs

- **Status:** Proposed
- **Date:** 2026-10-30
- **Invariants affected:** 2, 6
- **Supersedes:** the re-record bound in ADR 0032's Decision, "not from a run whose
  slowest exclusive run is above the baseline's"; its cadence decision and the rest of
  ADR 0032 stand. Extends the slowest-run rule of ADR 0016 from the runs of one process
  to the runs of three.

## Context

The latency gate compares a row's median p99.99 over twenty-one runs with the slowest
run in the baseline, and fails it beyond 5 percent and beyond one step of the clock
(ADR 0016, ADR 0029). ADR 0016 chose the slowest run because a row's twenty-one runs
spread 10 to 20 percent inside one process; `--update` wrote one result file, so a
baseline was one process's twenty-one runs, and every baseline recorded so far was.

Once ADR 0032 had removed the cadence edge on kip, the same tree read differently from
one process to the next on rows no commit had touched. Master at 4a09b6e, CI run
40961831660, three runs of `bench (kip)` on 2026-10-29, the checked profile, median
p99.99 then the band of the twenty-one runs, in ns: `claim_commit_32_concurrent` 162
(158 to 166), 156 (150 to 159), 154 (150 to 160) against a baseline whose slowest run
was 154; `claim_commit_32_exclusive` 124 (122 to 154), 106 (102 to 160), 102 (96 to
123) against 132; `claim_commit_32_recorded` 241, 239, 238 against 254. The first run
failed on the concurrent row, +5.2 percent; the next two passed it; three more runs of
the same job on 2026-10-30 read the concurrent row at 162, 162 and 158, two red and one
green. The unchecked
profile's `claim_commit_32_recorded` row read 249 and 250 (238 to 257) against 234,
+6.4 and +6.8 percent, and 241 to 250 in every process of the day, where the baseline's
median was 217: the bench host's same row had drifted the same way after the baseline
of 7f62673, and its two profiles were re-recorded from master's run at 96fb91a (pull
request 62); kip's had no CI job until a4d7e5d and stayed at 7f62673, recorded from one
process on 2026-10-26.

So a one-process baseline measures the band of the one process that happened to run,
and a row's median moves between processes of one tree by 5 percent on the concurrent
row and by a third on the exclusive row, more than the 5 percent the rule allows above
the slowest run. Which process recorded the baseline then decides which later runs
fail: among the three runs above, a baseline from the third holds the exclusive row's
slowest run at 123, and the branch run of the same day read that row's median at 130,
+5.7 percent; a baseline from the second holds it at 160. ADR 0032 forbade re-recording
from one high process for this reason, and the reason is right: no one process is the
host's band. The gate's floor was built for a clock that cannot resolve a step (ADR
0029); this is a host whose processes do not agree, and the baseline has to say so.

## Decision

A bench host's baseline for a profile is recorded from three processes: the first three
runs of master's job on that host at one tree, where a tree is one commit of master or
consecutive commits of master between which `git diff --stat -- crates/ bench/src`
prints nothing. Each run's result file is one process. `scripts/bench-compare.py
--update` takes the three files and writes one entry: each percentile from the slowest
process, the three processes' twenty-one runs in order in `p99_99_ns_runs`, `processes:
3`, the commit or the list of commits, the newest process's date, and the clock step;
it refuses files of two profiles or of two harness settings. The gate's compared value
stays the slowest run in the entry, now the slowest over the three processes, and the
rule stays ADR 0029's: a median more than 5 percent and more than one clock step above
it fails. The first line of the comparison says `baseline from 3 processes`.

The three are the first three runs that reached the profile, never a choice among
them: a run that failed the gate is one of the three. A baseline is re-recorded when
master's own run fails or brushes the gate with no data-plane change behind it, from
master's runs and never from a pull request's, which would be the gate loosened to
pass a benchmark. The commit lists every gated row's slowest run before and after and
names the cause of every row that loosens by more than 5 percent, or the card that will
find it. Both CI jobs run the unchecked profile when the checked comparison failed, so
every run yields both profiles' results. A laptop records one process as before, and
its entry says `processes: 1`.

## Alternatives considered

- **Rerun a red master until it passes.** Lost: six runs of the same tree on kip,
  three red at the checked profile, two red at the unchecked and one green, on rows no
  commit touched; a rerun is a coin flip whose cost has no end, and a gate that fails an
  unchanged commit teaches people to rerun it (ADR 0029).
- **Re-record from one process, as before.** Lost: which process is recorded decides
  which later runs fail, 123 against 160 for the exclusive row's slowest run among the
  three processes above, and ADR 0032's bound against a high process was right about
  one process and says nothing about three that were not chosen.
- **Compare the result's median with the slowest process's median.** Lost: the three
  processes' exclusive medians are 124, 106 and 102, and the fourth, fifth and sixth
  processes read 134, 148 and 107: a 5 percent rule on medians fails an unchanged tree,
  which ADR 0016 found inside one process and the slowest run was chosen to avoid.
- **Compare with the median of the three slowest runs.** Lost: a mean when there are
  two, and a result that lands above it but below a run the host measured on an
  unchanged tree fails though the host measured it.
- **Widen the tolerance to 10 percent.** Lost, as in ADR 0029: the `one_way_*` rows at
  280 ns would move 28 ns before failing, where the gate has caught 5 percent, and the
  rows that do not spread between processes would lose a sensitivity they have.
- **Run three processes per CI job and judge the median of their medians.** Lost: the
  result side is the smaller half of the problem, the baseline side stays one process
  unless it is recorded from three, and the job's six minutes on kip become eighteen on
  every push.
- **Exempt the exclusive row, or the rows that spread.** Lost: ADR 0032's third
  alternative; an exempt row lets a regression pass, and the concurrent row spreads too.
- **More than three processes, or as many as the day produced.** Lost: the slowest run
  over N processes grows with N, each process bringing its rarest slow run, so a
  baseline from every run available loosens by collection; three is where the three
  bands overlap on every row above and nobody records a fourth to pass. Fixed, not a
  minimum.

## Evidence

Kip's two profiles re-recorded in this change from master at 4a09b6e, CI run
40961831660, the first three attempts of `bench (kip)` on 2026-10-29 that reached each
profile (attempts 1 to 3 for the checked profile; the unchecked profile for attempts
2, 3 and 6, since a checked comparison that failed skipped it),
five million messages, twenty-one runs per row, pinned to CPUs 6,8,10, clock step 1 ns,
0 allocations. Each gated row's slowest run in the baseline, before (7f62673, one
process) and after (three processes), with the median and the band of each process, in
ns:

```text
kip-linux
  row                          before  after   change   median (band) per process
  claim_commit_32_client          189    190    +0.5%   176 (171-182); 173 (167-186); 177 (171-190)
  claim_commit_32_concurrent      154    166    +7.8%   162 (158-166); 156 (150-159); 154 (150-160)
  claim_commit_32_exclusive       132    160   +21.2%   124 (122-154); 106 (102-160); 102 (96-123)
  claim_commit_32_recorded        254    257    +1.2%   241 (223-257); 239 (227-253); 238 (231-256)
  one_way_32_client               324    342    +5.6%   316 (304-322); 316 (310-342); 314 (310-317)
  one_way_32_concurrent           295    293    -0.7%   289 (283-293); 281 (276-285); 281 (278-284)
  one_way_32_exclusive            280    277    -1.1%   269 (266-277); 265 (262-273); 266 (263-272)
  one_way_32_recorded             416    407    -2.2%   389 (385-400); 386 (383-393); 389 (386-407)

kip-linux-unchecked
  row                          before  after   change   median (band) per process
  claim_commit_32_client          202    176   -12.9%   169 (164-172); 171 (166-174); 175 (173-176)
  claim_commit_32_concurrent      176    172    -2.3%   154 (148-162); 148 (146-153); 170 (168-172)
  claim_commit_32_exclusive       165    149    -9.7%   125 (123-145); 124 (122-149); 134 (128-147)
  claim_commit_32_recorded        234    257    +9.8%   249 (238-257); 250 (239-257); 234 (225-255)
  one_way_32_client               344    356    +3.5%   351 (340-355); 350 (344-354); 351 (345-356)
  one_way_32_concurrent           291    305    +4.8%   300 (299-305); 300 (298-303); 303 (301-305)
  one_way_32_exclusive            292    307    +5.1%   300 (286-305); 299 (288-307); 301 (296-306)
  one_way_32_recorded             402    434    +8.0%   388 (385-397); 389 (383-397); 392 (385-434)
```

The rows that loosen by more than 5 percent and why: `claim_commit_32_exclusive` and
`claim_commit_32_concurrent`, the two rows that pay the producer position's cross-core
transfer, whose spread inside one process and between processes has no cause on record
since ADR 0032 and has card 168; `one_way_32_client`, one run of 342 among sixty-three
whose other sixty-two are at most 328 and whose medians are 314 to 316 against a median
of 319 before, the slowest-run rule's own cost. In the unchecked profile:
`claim_commit_32_recorded`, which every process of the day read at 234 to 250 against
a median of 217 at 7f62673, the drift the bench host's same row showed and pull request
62 re-recorded there, with card 169 for its cause; `one_way_32_exclusive`, whose median
moved from 290 to 300 as the unchecked `one_way_*` rows moved 3 to 9 percent on kip
while the unchecked claim rows moved a tenth the other way, also card 169's; and
`one_way_32_recorded`, one run of 434 among sixty-three whose other sixty-two are at
most 403 and whose medians, 388 to 392, are below the 395 before. Six rows tighten,
the unchecked claim rows by a tenth.

Judged against the new baselines, every process measured on the two days before they
were recorded, and not recorded in them, passes, the two that failed the old baseline
among them: the checked profile's fourth and fifth attempts (2026-10-30), concurrent
162 (159 to 167) and 162 (150 to 164)
against 166, exclusive 134 (128 to 162) and 148 (142 to 159) against 160, `bench-compare:
ok` on both, where both had failed the concurrent row at +5.2 percent against 154; and
pull request 68's run 40958773217 at the same data plane (64c8aec), checked concurrent
153 against 166, exclusive 130 against 160, recorded 253 against 257, every row within
the baseline; the sixth attempt's checked process, concurrent 158 (154 to 164),
exclusive 107 (103 to 151), client 174 (166 to 192), `bench-compare: ok`. In the
unchecked profile, the same pull request's run, concurrent 147, exclusive 106,
recorded 243 (237 to 258) against 257, and the two master processes of the
one-millisecond cadence, a4d7e5d and 0e97c37, recorded 243 and 241 against 257, all
`bench-compare: ok`.

The first process judged against the new baselines after they were recorded does not
pass: the `bench (kip)` run of the pull request that records them, 41016312787, on
2026-10-30 at 06:50, at the merge of the change with master (5b6eab7), with no crate
code changed. The checked profile passed, `claim_commit_32_client` 184 (182 to 192)
against 190; the unchecked profile failed on `claim_commit_32_client`, median 189 (180
to 198) against the slowest run of 176, +7.4 percent, with `check.sh` ok, 0
allocations, clock step 1 ns and every other row within its baseline. That row is one
the re-record tightened, from 202 to 176: the three recorded processes read it at 169
(164 to 172), 171 (166 to 174) and 175 (173 to 176), and the one process of 7f62673 at
187 (182 to 202), so the fourth process's twenty-one runs all lie above the slowest of
the sixty-three recorded and inside the band of the one process they replaced, which
would have passed it. The three bands overlapped on this row among the three
processes and not with the fourth: four medians from 169 to 189 where no one
process's band is wider than 18 ns, which is the Revisit clause below on the
baseline's first day. What the decision does about that run is not decided in this
revision of the ADR: a rerun until green is the first alternative, lost; a re-record
from a pull request's run is what the Decision forbids; and the Decision does not yet
say what else. It has to before this ADR is accepted.

What the three-process baseline cannot tell: a process of the one-millisecond cadence
in ADR 0032's high mode, exclusive 156 and concurrent 168 at 0e97c37, also passes
against 160 and 166. The cadence's record is the `producer_position_loads` count the
harness writes in every result, 248 to 253 on kip's exclusive row at two milliseconds
against 495 to 503 at one, and the exclusive row's band on kip is card 168's.

Acceptance: this ADR is accepted when master's `bench (kip)` has passed on its first
three pushes after this re-record without a rerun, which is the evidence that the
baseline holds the host's band; until then it is Proposed and the re-record stands as
its first step.

## Consequences

Easier: master's runs on kip stay green on an unchanged tree, and a red `bench (kip)`
is a change again. A baseline says what its host measured in three processes, and the
comparison's first line says so. Recording one takes three runs of the job instead of
one, two of them reruns, and `--update` refuses to mix profiles or harness settings,
or to record two processes or four.

Harder: the gate on kip's exclusive row holds the line at 160 ns against medians of 102
to 148, so a regression on that row has to move its median past 168 to fail there; the
row's own band is that wide inside one process, and the bench host's checked exclusive
row, 160 against 160 with a 10 ns step, is the tighter gate on it until card 168 finds
the spread's cause. The concurrent row's line moves from 154 to 166. The bench host's
baselines stay one process each until master's own run on `bench` brushes the gate,
then they are re-recorded by this rule.

We now maintain the `processes` field, the three-run procedure in the two how-to pages,
and the CI jobs' unchecked steps that run after a failed comparison.

Revisit: when a host's medians spread between processes by more than its runs spread
inside one, so that three processes no longer cover the band; when card 168 names the
cause of the exclusive row's spread and narrows it, which is the day kip's baselines
tighten again; when the gate's compared statistic changes.

## References

- ADR 0015 (one baseline per profile), ADR 0016 (the slowest run), ADR 0029 (the clock
  step floor), ADR 0032 (the cadence, and the bound this supersedes); card 161 (the
  measurements that found the cadence edge), card 168 (the exclusive row's spread);
  pull request 62 (the bench host's re-record of 2026-10-29).
- `scripts/bench-compare.py` (`update`), `scripts/bench-compare-test.py` (`Processes`),
  `bench/baseline.json`, `.github/workflows/ci.yml`; `docs/how-to/run-the-bench.md`,
  steps 3 and 4; `docs/how-to/set-up-a-bench-host.md`, step 7.
- CI run 40961831660 (master at 4a09b6e, attempts 1 to 6), run 40958773217 (pull
  request 68), run 41016312787 (the `bench (kip)` job of the pull request that
  records the baselines, at 5b6eab7: the first process judged against them, red on
  the unchecked `claim_commit_32_client` row).
