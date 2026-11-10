# 0071. Run the bench jobs only for a change that can move the bench

- **Status:** Accepted
- **Date:** 2026-11-10
- **Invariants affected:** 2, 5, 6
- **Supersedes:** none. Amends the "when" of ADR 0016's Decision, "on pushes to
  `master` and on pull requests from this repository", for `bench (linux)`, and the
  same of ADR 0034's `bench (grip)` and ADR 0044's pair: they run for those pushes and
  pull requests whose change can move the bench. Where they run, what they run and
  what they are compared with stand.

## Context

The latency gate runs on two dedicated hosts, `bench` and `grip`, one self-hosted
runner each, because a shared runner cannot tell a regression from its own noise
(ADR 0016, ADR 0034). Every CI run takes them in turn: `bench (linux)` holds `bench`
for about eleven minutes, `bench (grip)` holds `grip` for about five, and then the
pair `bench (udp)` and `bench (udp peer)` holds both for about five more (ADR 0044).
A pull request runs once for each push, and its merge runs once more on `master`.
The hosts take one job at a time, so runs queue on them.

A change to a page of `docs/`, to an ADR, to `spec/`, to a workflow other than CI's,
to a script or to a client's generated code changes no instruction the harness runs.
Its run on the hosts measures the tree its base already measured, and holds the
hosts from a change that could move them. On 2026-11-10, with five runs in flight,
`master`'s run for one commit took 81 minutes from its push to its last check for
21 minutes of work on the hosts, and a pull request under a data-plane crate queued
among runs that included a script's and two pages'.

ADR 0016's Decision names when the job runs, so running it less often is a decision
of the same weight, and it was put to the person who made that one. Rjae decided it
on 2026-11-10: amend.

## Decision

A first job of `ci.yml`, `changes`, lists the files of the change, a pull request's
against its base and a push's between the push event's before and after, and
`scripts/bench-needed.py` says whether any of them can move the bench. A file cannot
when it is under one of the paths the script lists as known not to matter: `docs/`,
`spec/`, `.claude/`, `.github/` other than `ci.yml`, `scripts/` other than the
scripts the bench jobs and this decision run, `clients/`, `conformance/`, and the
root's markdown and licence. Every other file can: everything under `crates/` and
`bench/`, the workspace's manifest and lock, `counters.toml` and `config.toml`,
which generate sources, the `justfile`, `ci.yml`, `scripts/bench-compare.py`, the
pair's wait, and the script and its test themselves. So a path nobody listed runs
the bench, and a change to the list is judged by running it. The list names no path
that ends a tree as ADR 0033 defines one, so a change that reports without the hosts
is at its base's tree, and that tree's processes are the runs that measured it; a
crate's tests, examples and README are under `crates/` and run the bench for that
reason, not because they can move it. When no file of the change can move the
bench, the four bench jobs keep their names, run on a hosted runner, print which
files were read and why nothing runs, and pass in seconds, so the checks `master`
requires still report and neither host is taken. A push has one more condition: the
commit before it must have a completed, successful run of `ci.yml` on its branch,
by a push or by hand. Runs of one branch go one at a time with one waiting, and a
newer push takes the waiting run's place, so a commit under `crates/` whose run was
replaced that way, or whose bench failed, has no such run, and the push after it
runs the bench over both, pages or not. A push that was itself skipped passed that
test, so the last run on the hosts before any skipped push is green and nothing
since could move the bench: a red or unmeasured `master` is never turned green by a
page. A change the job cannot list, more
files than the API returns, a push with no commit before it, or a failed read, runs
the bench. `workflow_dispatch` always runs it, which is how a baseline's processes
or a look at a host are had at a tree whose last change was a page. The two gates
jobs run for every change, as before.

## Alternatives considered

- **Run the bench for every change, as ADR 0016 has it.** Lost: more than a quarter
  of the commits cannot move it, and each costs the hosts about 21 minutes twice or
  more.
  The queue that results holds back the changes the gate exists for.
- **`paths` or `paths-ignore` on the workflow's triggers.** Lost: a workflow that
  does not start reports no check, and branch protection waits for a required check
  for ever. The filter is also for the whole workflow, and the gates jobs must run
  for every change.
- **A marker in the commit message or a label that skips the bench.** Lost: a person
  or a session decides by hand what a list decides the same way every time, and a
  wrong marker on a data-plane change passes the gate unmeasured.
- **List a crate's `tests/`, `examples/` and `README.md` as unable to move the bench
  too.** True of them, and three more of the 80 commits. Lost: ADR 0033 counts any
  change under `crates/` as the end of a tree, so such a commit would start a tree
  with no process of its own, and a baseline recorded there would have to be asked
  for by hand. The list and that definition agree instead.
- **Decide over what changed since the pull request's last head with a green bench.**
  It would also spare a data-plane pull request's later pushes that change only a
  page. Lost for now: it needs a second rule for merges of `master` into the branch
  and a record of which head was measured. It can follow this one.
- **A third bench host.** It takes the queue from the other side, and a host for the
  single-host job would need its own tuning, profile and baselines (ADR 0033). It
  does not make a page's run measure anything.
- **Leave the jobs on the hosts and let their steps do nothing.** Lost: the job
  still waits in the host's queue for its turn, behind eleven-minute jobs, to do
  nothing.
- **Decide a push from its own files alone.** The first draft of this decision. Lost
  in its review: a push's run can be replaced while it waits, and a page pushed
  after it would then report green for a commit under `crates/` that no run of
  `master` ever measured; and a page pushed onto a commit whose bench failed would
  turn `master`'s newest checks green. The question is whether the tree was
  measured, so the push also asks it of the commit it stands on.

## Evidence

- Of `master`'s 80 commits up to cafffc7, the script answers `bench=false` for 22:
  the commits that propose or accept seven ADRs with their spec pages (0042, 0044,
  0046, 0051, 0052, 0058, 0061), workflow, skill and script changes, pages of
  `docs/`, and the clients' generated code. Not one of the 22 changes a path that
  ends a tree by ADR 0033's definition. Counted by running `decide` over
  `git show --name-only` of each commit of `git log -80 cafffc7`.
- What a run holds, from the jobs of run 42864121833 on 2026-11-10: `bench (linux)`
  17:47:51 to 17:59:12 on `bench`, `bench (grip)` 17:42:54 to 17:48:04 on `grip`, the
  pair 18:38:11 to 18:43:07 on both. The run was created at 17:21:31 and ended at
  18:43:08.
- The queue that day, five runs in flight (`master` at cafffc7 and pull requests 174,
  176, 177 and 178): pull request 177's run lasted from 17:29:57 to 18:38:12, and its
  `bench (udp peer)` held `grip` from 18:03:46 until its partner got `bench` at
  18:33:16, behind two other runs' `bench (linux)`; pull request 176's lasted from
  17:42:47 to 18:58:36; pull request 174, under `cavalry-driver`'s `src/`, queued
  among them. Of the five, this decision takes pull request 178 off the hosts, a
  script and two pages; the other four change a path that runs the bench.
- `scripts/bench-needed-test.py` holds the list: each path named above on its side,
  an unlisted path on the side that runs, the cases the job cannot list, a push onto
  a commit whose run was replaced, failed, is still running or is not there, and
  that every script the bench jobs, the bench recipes and the host's check name is
  on the side that runs.

## Consequences

Easier: a page, an ADR or a workflow change reports its checks in the time the gates
take, about seven minutes, and takes no host; the hosts' queue is shorter by the
runs that measured nothing, so a data-plane change waits less. A baseline's tree and
the bench's runs keep one definition between them.

Harder, and what it costs: every `master` run was also a sample of the hosts'
health between baselines, and cards 168, 169, 198 and 219 read drift from them. With
more than a quarter of the commits skipped the samples are fewer, never wrong. The
bench jobs now wait for a job on a hosted runner to answer first, a minute or so,
and for as long as hosted runners are unavailable; a failed answer runs the bench,
and one that never starts holds the run as an unavailable gates runner already does.
The list is one more thing to keep. A new top-level directory is on the side that
runs until someone lists it, which is the safe side. `scripts/` is the one directory
listed whole with its exceptions named, so the script's tests, which `just test`
runs, fail when the bench jobs, the bench recipes or the host's scripts name a
script that is not among the exceptions.

It does not shorten a pull request whose change is under a crate's `src/`: the
decision is over the whole pull request against its base, so each push of it runs
the bench again.

Revisit when a regression reaches `master` through a change the list called unable
to move the bench, for the path that let it through and whether a list can be
trusted at all; when the hosts' drift is read less well for the fewer samples, for a
scheduled run; and when the queue is long again with the bench skipped for pages,
for the alternative over what changed since the last measured head.

## References

- `CLAUDE.md`: invariant 2 and its gate; Working rules for agents, "a weakened gate
  says why"
- ADR 0015, ADR 0016, ADR 0033, ADR 0034, ADR 0044
- `.github/workflows/ci.yml`; `scripts/bench-needed.py`; `scripts/bench-needed-test.py`;
  `docs/how-to/run-the-bench.md`
- AppShapes Trello card 246; Brigade decision d-20261110-34f67d, answered by Rjae on
  2026-11-10
