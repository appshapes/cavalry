# 0139. Run the latency gate each night as a coarse check, until the slice that measures Cavalry against Aeron

- **Status:** Accepted
- **Date:** 2026-11-21
- **Invariants affected:** 2, 6
- **Supersedes:** 0050, 0071 and 0074: no pull request is measured, so none reads red
  at `master`'s tree, and the choice of which changes take the bench hosts is gone,
  since none does. Of ADR 0016, when the gate runs, on every push and pull request, and
  its place among `master`'s required checks; of ADR 0034 and ADR 0044, the same for
  `bench (grip)` and for the link rows. Of ADR 0016 and ADR 0029, the size of the rule,
  5 percent, which is 50 percent until the gate is armed again; the clock step's floor
  and the comparison with the slowest run recorded stand. Of ADR 0033 and ADR 0110, the
  occasions on which a change of the tree re-records a baseline, which give way to the
  one in Decision 5; how a baseline is recorded and who may re-record stand. It changes
  the last sentence of invariant 2 in `CLAUDE.md` and the latency row of its Gates table

## Context

ADR 0138 drew the finish line at a beta and made this change the first feature of its
slice. The owner ruled it on 2026-11-21 (work item 360): "Pull requests and landings
never take the hosts. One run each night on master fails only when a row's p99.99 is
more than 50% above the slowest run recorded. The allocation tests and the lints for
async, locks and dependencies stay in every pull request. No histograms in commit
messages and no re-records. A failure two nights in a row becomes one work item." The
ruling follows the owner's of 2026-11-15 on work item 358: the features come first, and
the numbers are worked on when Cavalry must meet or beat Aeron's.

How the gate ran until this ADR. Every push and every pull request that touched a
path able to move the bench took both bench hosts, `bench` and `grip`: `bench (linux)`
for 11.3 minutes, `bench (grip)` for 5.2 and the pair of link jobs for 4.9 (ADR 0073,
Context). A host runs one job at a time, so every session's pull requests under
`crates/` went one after another, and a merge waited on three required bench checks. A
gated row failed when its p99.99, the median of 21 passes, was more than 5 percent and
more than one step of the clock above the slowest run recorded for its profile (ADR
0016, ADR 0029, ADR 0033).

What it cost. Of the 253 commits from 2026-10-28 to 2026-11-15, 53 name a re-record in
their message and 14 changed `bench/baseline.json`. On 2026-11-10 one commit took 81
minutes from its push to its last check for 21 minutes of work on the hosts (ADR 0071).
A re-record takes two hours of both hosts, with no change under `crates/` landing
meanwhile (ADR 0110). And the gate failed with no instruction changed: six of eight
processes at one head failed a row by 5.6 to 8.1 percent with the same instructions on
the message path (ADR 0125); about one process in ten reads a claim row a tenth high
for its whole life, by where the kernel places its memory (work item 198); and the link
row read up to 20 percent above its usual level for hours with no change on its path
(work item 358). Every one of those is within a fifth of the level around it.

What needs no quiet host. The allocation tests in `just test`, nine files under
`crates/cavalry-client/tests/`, one under `crates/cavalry-ffi/tests/` and three under
`crates/cavalry-raft/tests/`. The harness itself, which exits non-zero on an allocation
in a measured loop and runs on the hosted runner in `gates (linux)`. And `just lint`:
`scripts/lint-workspace.py` for `async`, `.await`, the dependency allowlist and the
padded slots, and clippy's ban of `std::sync::Mutex`, `RwLock`, `parking_lot` and
`block_on` in data-plane crates. Nothing lints a system call or a panic path; those are
the checklist's and the reviewer's.

## Decision

No pull request and no push takes a bench host. The latency gate runs in a workflow of
its own, `.github/workflows/bench.yml`, at 07:17 UTC each day and on demand, and
compares at a coarse size: a gated row fails when its p99.99 is more than 50 percent,
and more than one step of the clock, above the slowest run recorded for its profile.
The per-message rules of invariant 2 stay enforced in every pull request by the tests
and lints that need no quiet host. This holds until the ADR of the slice after the
beta, in which Cavalry is measured against Aeron, arms the gate again.

1. **The workflow.** `bench.yml` runs the four jobs that `ci.yml` held, under the same
   names: `bench (linux)` and `bench (grip)`, then the pair `bench (udp)` and `bench
   (udp peer)` once both have passed. A first job, `tree`, reads the head of the branch
   to measure, `master` each night, so that the four measure one commit, and each job
   names that commit to the harness in `CAVALRY_BENCH_GIT`, which the result's `git`
   then holds. A night's result is a result of the commit that was `master`'s head when
   the run started. It is no required check and no check of a pull request, and it
   turns no run of `ci` red.
2. **On demand, a branch of this repository, as a report.** `gh workflow run bench.yml
   -f branch=<name>` measures a pushed branch. The input is a branch's name and nothing
   else: the `tree` job takes only the ref that equals the name, in this repository,
   where a stranger can make no branch, so the hosts run no stranger's code. A run
   started by hand prints its comparisons and does not fail on them: the run is
   attached to `master`'s head, and a branch that reads slow is no fault of that
   commit. This is how a change whose decision needs a number takes one measurement,
   which its ADR records.
3. **The size.** `TOLERANCE` in `scripts/bench-compare.py` is 0.50. The rule's other
   parts stand: the median of 21 passes, the slowest run over every process recorded,
   the floor of one clock step, the rows that are reported and never gated, and zero
   allocations. `just bench-compare` says the same on a laptop as on a host.
4. **`master` requires `gates (linux)` alone.** The owner dropped `bench (linux)`,
   `bench (grip)` and `bench (udp)` from the branch protection rule on 2026-11-21, for
   this change. `ci.yml` has no bench job, and those names are the nightly workflow's
   alone. The `changes` job and `scripts/bench-needed.py` are removed with the choice
   they made.
5. **What a red night asks.** One red night asks nothing. Two scheduled nights in a
   row that are red on one row are a task ahead of the features: `/next-task` reads the
   workflow's last scheduled runs. The task finds the change among the commits between
   the last green night and the first red one, by measuring branches at chosen
   commits, and removes the cost. The owner's "no re-records" ends the re-records that
   followed the tree to keep a rule of 5 percent true. Three occasions are left, since
   without them the file is no baseline, and each tells the owner first (ADR 0110): a
   cost that a change added on purpose, with its ADR, which would otherwise turn every
   night red, re-records that profile once; a host that itself changes, in its
   hardware, its kernel or its tuning, has its baselines recorded again (ADR 0033); and
   a row that a change adds to the bench gets its entry.
6. **What a change under a data-plane crate owes.** The per-message checklist of
   `.claude/rules/data-plane.md`, the allocation tests and the lints, as before. When it
   adds work that a message passes through, one sentence in its commit message that
   says what and why. No histogram in a commit message or a pull request's body, and no
   wait for a host. A claim that a change is faster still needs its histogram:
   `CLAUDE.md`'s "A performance claim without a histogram is not a claim" stands.
7. **The words that change with it.** In `CLAUDE.md`: the last sentence of invariant 2,
   the latency row of the Gates table and the bench rule under Commands. With them
   `.claude/rules/data-plane.md`, the `commit`, `pr` and `next-task` skills, the
   reviewer's and the developer's roles, and the how-to pages. The `/next-task` skill
   also takes ADR 0138's three chains as the slice's order.

What the coarse check sees, and what it does not. In `bench/baseline.json` the slowest
run recorded of a claim row, or of a one-way row over shared memory, is 77 to 560 ns in
the two hosts' checked profiles, so 50 percent of it is 38 to 280 ns: the check sees
work that grows with a message, a copy, a scan, a call that blocks. It does not see a
tenth, and it may not see one added system call on a row of 500 ns. The lints and the
allocation tests hold their rules by reading the code and counting, and the checklist
and the reviewer hold the rest. What none of them sees is the price of the coarse
size, and the slice that arms the gate again measures it.

## Alternatives considered

- **Switch the gate off until the performance slice.** Lost: the hosts would measure
  nothing while a change that doubles a row's tail went unseen for weeks, to be found
  among a hundred commits. One run a night finds it among a day's, and costs nobody a
  wait.
- **Measure each landing on `master`, never blocking.** Lost: `master`'s `ci` run would
  still last as long as its bench jobs, the `pr` skill waits on that run, and landings
  would queue behind the hosts again. A night's commits are few enough to search.
- **Keep the gate as it was.** Lost on the evidence in Context, and on the owner's
  ruling.
- **A size of 10 or 25 percent.** Lost: the reds recorded with no cause reach a fifth,
  so 25 percent leaves 5 percent between the hosts' own band and a red night, and a red
  night that means nothing is read by nobody the next time. At 50 percent a red is a
  change's.
- **The schedule inside `ci.yml`.** Lost: a scheduled run would share `master`'s
  concurrency group and take the place of a landing's waiting run, and its result would
  read as `master` red, which `/next-task` puts before everything.
- **Three jobs in `ci.yml` that keep the required names and measure nothing, so that
  the protection rule needs no change.** This ADR's first draft. Lost: the nightly
  workflow's jobs carry the same names, so on one commit a name would be a measurement
  in one workflow and nothing in the other, and a run by hand on a branch's ref would
  put a measurement under a pull request's required name. The owner dropped the names
  from the rule instead.
- **Names of their own for the nightly's jobs.** Lost to the same ruling: the
  baselines' records, the hosts' pages and the partner wait all know the jobs by the
  names they have.
- **Keep `scripts/bench-needed.py` for the day the gate is armed.** Lost: 400 lines and
  678 of tests that choose between two answers when there is one. The repository's
  history has them, and ADR 0071 and ADR 0074 say what they did.
- **Let the workflow measure any ref.** Lost: a pull request's ref or a commit id can
  name a fork's code, and a self-hosted runner on a public repository must not run it.
- **Fail a run by hand on its comparisons, as the night's fails.** Lost: the failure
  would show on `master`'s head for a branch's cost. Whoever started the run reads its
  tables.

## Evidence

None measured for this ADR: it changes when and at what size a measurement is judged.
What informed it: the owner's rulings of 2026-11-15 and 2026-11-21, the second of them
in two parts, the gate's and the protection rule's; ADR 0138's reading of the bench's
rules and cost; the figures in Context, from `git log` and from ADR 0071, ADR 0073, ADR
0110 and ADR 0125 and work items 198 and 358; `ci.yml` and `scripts/bench-compare.py`
as they stood at 6621ddac. The tests hold the size: `scripts/bench-compare-test.py`,
`Coarse`, passes a row 40 percent above the slowest run recorded and fails one 60
percent above it, and holds the rule's other tests at 5 percent, the size the armed
gate returns to. An independent read of the change before it landed found the shared
names, a branch name that was matched as a pattern, a count of nights that a run by
hand would have moved, and a result that named `master`'s commit for a branch's; the
change has each.

## Consequences

Easier: a pull request under `crates/` waits on `gates (linux)` alone, so sessions work
side by side; a pull request's body carries no bench table, so the review loop spends
no cycle on one; the hosts are free in the day for a branch that needs a number.

Harder: a regression under 50 percent lands unseen and is found by the performance
slice among many commits, which is the price the owner chose; a red night is nobody's
pull request, so somebody must read it, and `/next-task` is where; the baselines grow
old, since no re-record follows them, so the comparison is against the tree of
2026-11-15 until the gate is armed; a night's log and result files are kept seven days,
the repository's retention, so the nights are a record of a week and not of the slice;
a red night shows on the commit that was `master`'s head at 07:17 UTC, whoever's it
was.

Revisit: when 0.1.0 is published, with the ADR of the slice that measures Cavalry
against Aeron, which sets the size back, decides whether a change takes a host again
and whether a bench check is required again, and settles what work items 198 and 358
found first; when two nights in a row are red on one row, for the task of Decision 5;
when the nightly run is red more than once a week with no change behind it, for the
size.

## References

- Work item 361 on the Brigade Board, feature 1 of ADR 0138's slice, with the owner's
  ruling on the protection rule; work item 360, where the ruling on the gate is
  recorded; work items 198 and 358, parked
- `CLAUDE.md`: invariant 2; Gates; Commands; Working rules for agents
- ADR 0015, ADR 0016, ADR 0017, ADR 0029, ADR 0033, ADR 0034, ADR 0044, ADR 0050, ADR
  0071, ADR 0073, ADR 0074, ADR 0110, ADR 0125, ADR 0127, ADR 0138
- `.github/workflows/bench.yml`, `.github/workflows/ci.yml`,
  `scripts/bench-compare.py`, `scripts/bench-compare-test.py`,
  `scripts/wait-for-partner-job.py`, `bench/src/main.rs`
- [Run the bench and compare it with the baseline](../how-to/run-the-bench.md);
  `.claude/rules/data-plane.md`; `bench/README.md`
