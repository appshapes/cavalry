# 0033. Record a bench host's baseline from every process it has run at one tree

- **Status:** Proposed
- **Date:** 2026-10-30
- **Invariants affected:** 2, 6
- **Supersedes:** the re-record bound in ADR 0032's Decision, "not from a run whose
  slowest exclusive run is above the baseline's"; its cadence decision and the rest of
  ADR 0032 stand. Extends the slowest-run rule of ADR 0016 from the runs of one process
  to the runs of every process the host has run at the baseline's tree.

## Context

The latency gate compares a row's median p99.99 over twenty-one runs with the slowest
run in the baseline, and fails it beyond 5 percent and beyond one step of the clock
(ADR 0016, ADR 0029). ADR 0016 chose the slowest run because a row's twenty-one runs
spread 10 to 20 percent inside one process; `--update` wrote one result file, so a
baseline was one process's twenty-one runs, and every baseline recorded so far was.

Once ADR 0032 had removed the cadence edge on kip, the same tree read differently from
one process to the next on rows no commit had touched. Master at 4a09b6e, CI run
40961831660, six runs of `bench (kip)` on 2026-10-29 and 2026-10-30, the checked
profile's median p99.99 in ns: `claim_commit_32_concurrent` 162, 156, 154, 162, 162 and
158 against a baseline whose slowest run was 154, three of them red at +5.2 percent;
`claim_commit_32_exclusive` 124, 106, 102, 134, 148 and 107 against 132;
`claim_commit_32_recorded` 241, 239, 238, 218, 229 and 232 against 254. The unchecked
profile's `claim_commit_32_recorded` row read 249, 250 and 234 (225 to 257) against 234,
two of them red at +6.4 and +6.8 percent, and 234 to 250 in every process of the two
days, where the baseline's median was 217: the bench host's same row had drifted the
same way after the baseline of 7f62673, and its two profiles were re-recorded from
master's run at 96fb91a (pull request 62); kip's had no CI job until a4d7e5d and stayed
at 7f62673, recorded from one process on 2026-10-26.

So a one-process baseline measures the band of the one process that happened to run,
and a row's median moves between processes of one tree by a tenth on the concurrent and
client claim rows and by half on the exclusive row, 102 to 148, more than the 5 percent
the rule allows above the slowest run. Which process recorded the baseline then decides
which later runs fail. ADR 0032 forbade re-recording from one high process, and the
reason is right: no one process is the host's band. Nor are three: the first version of
this decision recorded kip's baselines from the first three processes of each profile,
and the first process judged against them, pull request 69's run 41016312787 at the
same data plane, read the unchecked `claim_commit_32_client` row at 189 (180 to 198)
against a slowest run of 176 from processes whose medians were 169, 171 and 175, +7.4
percent, a row the three had put below the 202 of the one process before them. The
gate's floor was built for a clock that cannot resolve a step (ADR 0029); this is a host
whose processes do not agree, and the baseline has to hold what the host has measured,
not a sample of it.

## Decision

A bench host's baseline for a profile is recorded from every process the host has run
at the baseline's tree, three at least. A tree is a commit and every later commit
between which `git diff --stat -- crates/ bench/src Cargo.toml Cargo.lock
bench/Cargo.toml` prints nothing, so a dependency bump, which rebuilds the harness and
the data-plane crates from other code, ends a tree as a code change does; a run of
master's job at the tree is a process of it, and so is a run of a pull request whose
head is at the tree, since a change that touches none of those paths can carry no
regression the gate measures. A run of a pull request that changes the data plane
is judged and never recorded. Each run's result file is one process. `scripts/
bench-compare.py --update` takes the files and writes one entry: for each percentile
the largest value over the processes, every process's twenty-one runs in order in
`p99_99_ns_runs`,
`processes`, the commit or the list of commits, the newest process's date, and the
clock step; it refuses files of two profiles or of two harness settings, and refuses
two processes, since a bench host's baseline is three at least and a laptop's is one.

The gate's compared value stays the slowest run in the entry, now the slowest over
every process, and the rule stays ADR 0029's: a median more than 5 percent and more
than one clock step above it fails. The first line of the comparison says `baseline
from N processes`. On the baseline's tree nothing the gate exists to catch has changed,
so a process of that tree that reads a row above the baseline is the host's own band,
and it joins the baseline: `scripts/bench-compare.py --join <result>` adds the process
to the entry, in a commit whose message carries the run and the row, by the procedure
in `docs/how-to/run-the-bench.md`, step 4. A process of another tree never joins. The
host's one deliberate change, an update of its packages or kernel, is the re-record of
`docs/how-to/set-up-a-bench-host.md`, "Updating the host", as before.

A baseline is re-recorded at a new tree when master's own run fails or brushes the gate
there with no data-plane change behind it, from the first three processes of the tree
and every one after them. The commit lists every gated row's slowest run before and
after and names the cause of every row that loosens by more than 5 percent, or the card
that will find it. Both CI jobs run the unchecked profile when the checked comparison
failed, so every run yields both profiles' results. A laptop records one process as
before, and its entry says `processes: 1`.

## Alternatives considered

- **Rerun a red master until it passes.** Lost: six runs of the same tree on kip,
  three red at the checked profile, two red at the unchecked and one green, on rows no
  commit touched; a rerun is a coin flip whose cost has no end, and a gate that fails an
  unchanged commit teaches people to rerun it (ADR 0029).
- **Re-record from one process, as before.** Lost: which process is recorded decides
  which later runs fail, 123 against 160 for the exclusive row's slowest run among the
  first three processes, and ADR 0032's bound against a high process was right about
  one process and says nothing about several that were not chosen.
- **Exactly three processes, the first three, fixed.** The first version of this
  decision, lost on its first test: three processes put the unchecked client claim row
  at 176, below the one process before them, and the fourth read 189 (Context). A
  fixed count is a sample of the band, and the band is what the gate needs.
- **Compare the result's median with the slowest process's median.** Lost: the
  exclusive row's medians across eight processes are 102 to 148: a 5 percent rule on
  medians fails an unchanged tree, which ADR 0016 found inside one process and the
  slowest run was chosen to avoid.
- **Compare with the median of the processes' slowest runs.** Lost: a mean when there
  are two, and a result that lands above it but below a run the host measured on an
  unchanged tree fails though the host measured it.
- **Widen the tolerance to 10 percent.** Lost, as in ADR 0029: the `one_way_*` rows at
  280 ns would move 28 ns before failing, where the gate has caught 5 percent, and the
  rows that hold within 2 percent between processes would lose a sensitivity they have.
- **Run several processes per CI job and judge the median of their medians.** Lost for
  now: the baseline side stays one process unless it is recorded from several, the
  result side's offset is covered by a baseline that holds the band, and the job's six
  minutes on kip become eighteen on every push. It returns if the band keeps growing
  (Revisit).
- **Exempt the exclusive row, or the rows that spread.** Lost: ADR 0032's third
  alternative; an exempt row lets a regression pass, and the client and concurrent rows
  spread too.

## Evidence

Kip's two profiles re-recorded in this change from every process at the tree of master
4a09b6e: the checked profile from the six attempts of `bench (kip)` in CI run
40961831660 on 2026-10-29 and 2026-10-30 and pull request 69's runs 41016312787
(5b6eab7, 4a09b6e merged with fc32d3c) and 41017975115 (7c72c95, 4a09b6e merged with
8a4f469), both with no change under the tree's paths, eight processes; the unchecked
profile from attempts 2, 3 and 6, since a checked comparison that failed skipped it in
attempts 1, 4 and 5, and the same two pull request runs, five processes. Five million
messages, twenty-one runs per row, pinned to CPUs 6,8,10, clock
step 1 ns, 0 allocations. Each gated row's slowest run in the baseline, before (7f62673,
one process) and after, with every process's median in order and the band of all the
runs, in ns:

```text
kip-linux, 8 processes
  row                          before  after  change   medians by process; the runs' band
  claim_commit_32_client          189    198   +4.8%   176 173 177 190 183 174 184 175; 166 to 198
  claim_commit_32_concurrent      154    169   +9.7%   162 156 154 162 162 158 166 155; 150 to 169
  claim_commit_32_exclusive       132    162  +22.7%   124 106 102 134 148 107 123 105; 96 to 162
  claim_commit_32_recorded        254    257   +1.2%   241 239 238 218 229 232 245 235; 212 to 257
  one_way_32_client               324    342   +5.6%   316 316 314 317 317 315 320 316; 304 to 342
  one_way_32_concurrent           295    293   -0.7%   289 281 281 284 287 281 291 281; 276 to 293
  one_way_32_exclusive            280    282   +0.7%   269 265 266 268 275 267 269 264; 262 to 282
  one_way_32_recorded             416    423   +1.7%   389 386 389 388 390 386 390 387; 382 to 423

kip-linux-unchecked, 5 processes
  row                          before  after  change   medians by process; the runs' band
  claim_commit_32_client          202    198   -2.0%   169 171 175 189 171; 164 to 198
  claim_commit_32_concurrent      176    172   -2.3%   154 148 170 158 146; 145 to 172
  claim_commit_32_exclusive       165    159   -3.6%   125 124 134 125 106; 105 to 159
  claim_commit_32_recorded        234    257   +9.8%   249 250 234 251 246; 225 to 257
  one_way_32_client               344    356   +3.5%   351 350 351 352 350; 340 to 356
  one_way_32_concurrent           291    305   +4.8%   300 300 303 301 301; 296 to 305
  one_way_32_exclusive            292    307   +5.1%   300 299 301 302 303; 286 to 307
  one_way_32_recorded             402    434   +8.0%   388 389 392 386 386; 381 to 434
```

The rows that loosen by more than 5 percent and why. In the checked profile:
`claim_commit_32_exclusive` and `claim_commit_32_concurrent`, the two rows that pay the
producer position's cross-core transfer, whose spread inside one process and between
processes has no cause on record since ADR 0032 and has card 168; `one_way_32_client`,
one run of 342 among 168 whose other 167 are at most 328 and whose medians, 314 to 320,
sit at the 319 before, the slowest-run rule's own cost. In the unchecked profile:
`claim_commit_32_recorded`, which every process of the two days read at 234 to 251
against a median of 217 at 7f62673, the drift the bench host's same row showed and
pull request 62 re-recorded there, with card 169 for its cause; `one_way_32_exclusive`,
whose median moved from 290 to 300 as the unchecked `one_way_*` rows moved 3 to 9
percent on kip while the unchecked claim rows moved a tenth the other way, also card
169's; and `one_way_32_recorded`, one run of 434 among 105 whose other 104 are at most 403
and whose medians, 386 to 392, are below the 395 before. Four rows tighten, the
unchecked claim rows among them, the client row from 202 to 198 with the fourth
process's 189 inside it.

The two pull request processes are the first two judged against a baseline of this
rule's shape. The first, run 41016312787, against the first version's three processes:
checked `bench-compare: ok`, unchecked red on `claim_commit_32_client`, 189 against
176, +7.4 percent, the only row over; it joined. The second, run 41017975115, against
the same three: `bench-compare: ok` on both profiles, unchecked `claim_commit_32_client`
171 (168 to 185) against 176 and checked 175 (167 to 180) against 190; it joined too,
and moved no slowest run.

What the baseline cannot tell: a process of the one-millisecond cadence in ADR 0032's
high mode, exclusive 156 and concurrent 168 at 0e97c37, also passes against 162 and
169. The cadence's record is the `producer_position_loads` count the harness writes in
every result, 248 to 253 on kip's exclusive row at two milliseconds against 495 to 503
at one, and the exclusive row's band on kip is card 168's.

Acceptance: this ADR is accepted when `bench (kip)` has passed three consecutive runs at
this tree, on master or on pull request 69, with no process joining the baseline, which
is the evidence that the baseline holds the host's band; until then it is Proposed and
the re-record stands as its first step.

## Consequences

Easier: a red `bench (kip)` on an unchanged tree is a process the baseline was missing,
with one command to add it and no coin flip; a red on a changed tree is a change. A
baseline says what its host measured in every process it ran, and the comparison's
first line says how many. Recording one at a new tree takes three runs of the job
instead of one, two of them reruns, and `--update` refuses to mix profiles or harness
settings.

Harder: the baseline grows while the host runs the tree, by one commit per process
that reads above it, until the band is covered; the acceptance criterion above is the
test of when that is. The gate on kip's exclusive row holds the line at 162 ns against
medians of 102 to 148, so a regression on that row has to move its median past 170 to
fail there; the row's own band is that wide, and the bench host's checked exclusive
row, 160 against 160 with a 10 ns step, is the tighter gate on it until card 168 finds
the spread's cause. The concurrent row's line moves from 154 to 169 and the client
row's from 189 to 198. The bench host's baselines stay one process each until master's
own run on `bench` brushes the gate, then they are re-recorded by this rule.

We now maintain the `processes` field, `--join`, the procedure in the two how-to
pages, and the CI jobs' unchecked steps that run after a failed comparison. The jobs
build with the `stable` toolchain unpinned, so two processes of one tree days apart
can be two compilers' binaries; the tree rule cannot see that, and a baseline whose
band a compiler moved is a re-record under this rule, not a join.

Revisit: when a process joins the baseline more than once a week after the band's first
week, so that the band keeps growing and several processes per job (the seventh
alternative) is the cheaper truth; when card 168 names the cause of the exclusive row's
spread and narrows it, which is the day kip's baselines tighten again; when the gate's
compared statistic changes.

## References

- ADR 0015 (one baseline per profile), ADR 0016 (the slowest run), ADR 0029 (the clock
  step floor), ADR 0032 (the cadence, and the bound this supersedes); card 161 (the
  measurements that found the cadence edge), card 168 (the exclusive row's spread),
  card 169 (the recorded row's drift and kip's unchecked rows); pull request 62 (the
  bench host's re-record of 2026-10-29), pull request 69 (this decision, and the review
  that recorded its first version's failure).
- `scripts/bench-compare.py` (`update`, `joined`), `scripts/bench-compare-test.py`
  (`Processes`), `bench/baseline.json`, `.github/workflows/ci.yml`;
  `docs/how-to/run-the-bench.md`, steps 3 and 4; `docs/how-to/set-up-a-bench-host.md`,
  step 7.
- CI run 40961831660 (master at 4a09b6e, attempts 1 to 6), runs 41016312787 and
  41017975115 (pull request 69 at 5b6eab7 and 7c72c95), run 40958773217 (pull request
  68).
