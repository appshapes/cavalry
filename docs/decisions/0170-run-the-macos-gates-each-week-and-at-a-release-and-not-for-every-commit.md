# 0170. Run the macOS gates each week and at a release, and not for every commit

- **Status:** Accepted
- **Date:** 2026-11-26
- **Invariants affected:** 5, 6; the gates of 1 to 4 run as before on Linux
- **Supersedes:** 0073. With it goes the "when" that ADR 0073 gave the macOS half of
  ADR 0007's acceptance test and ADR 0016's "the macOS runner keeps the correctness
  gates and the laptop half of invariant 5": that half now runs each week and at a
  release. What it runs stands. ADR 0160 stands whole.

## Context

Since ADR 0073, `ci.yml`'s `gates` job has run on Linux for every pull request, and on
Linux and macOS for every push to `master`. `gates (macos)` has not been a required
check, and `master`'s run has held the only macOS result of a change. Whoever landed
a change watched that run and fixed a red one forward before anything else.

On 2026-11-26 the owner was asked what to do about work item 456, the macOS runner
that is lost partway through `just conformance`. The owner wrote: "I would say leave
it parked, or unless we need to keep the macOS runner I would prefer to remove it.
Given the way Cavalry works, I don't expect organizations to choose macOS to host
Cavalry." The manager session answered that nothing which hosts Cavalry needs that
runner, that what it holds is the README's promise that the ten-minute path works on
a Mac laptop, and offered one run each night. The owner wrote: "Instead of your
nightly Option 1 recommendation, I am leaning towards a weekly run." Asked again the
same day, the owner confirmed the cadence in one word: "weekly".

Three facts bear on it, each counted under Evidence. Every session works on a Mac and
runs the four gates there before each pull request, so CI's macOS job is a second
net: a machine with nothing installed, the quickstart, and a slower clock. In the
four days from 2026-11-22, the job ran 63 times and took 16.2 hours of a macOS
runner, 4.0 hours a day. And three of those 63 jobs lost their runner for no fault
of a change, about one in twenty, each turning `master` red until someone ran it
again.

## Decision

`gates (macos)` no longer runs for a push to `master`. The macOS gates run once a
week at `master`'s head, from a schedule at 04:23 UTC each Monday, by hand at any
ref, and at a release at the release's commit. They are the same job with the same
steps as `gates (linux)`. `ci.yml`'s `gates` job runs on one runner a run: Linux for
every event of that file's own, a run by hand included, and macOS when another
workflow calls the file with `macos: true`. A new workflow, `macos.yml`, is that
caller and has no steps of its own. Its run is no run of `ci` for a commit and no
required check, as the latency gate's and Brigade's server's nights are not (ADRs
0139 and 0142), so a red week is not a red `master`. The runs it calls have a
concurrency group of their own, so a week's run neither waits behind a landing's run
nor takes the place of one that waits.

A red week has a reader. `/next-task` reads the newest run of `macos.yml` on
`master` and gives a red one out as a task, ahead of the next feature.
`docs/how-to/read-the-weekly-run-of-the-macos-gates.md` says what the reader does.
The first move is to run the failed job again, because a lost runner is the likeliest
cause and keeps no log. A failure that is there again is ours, and is found on a
Mac by bisecting the week's commits between the last green week and the red one. Its
work item's title begins `macOS gates on master:`.

At a release, the release's workflow calls `ci.yml` with `macos: true` at the
release's commit, whatever the weekday. That workflow does not exist yet: it is
work item 491, the last part of work item 369, and it carries the step. Until it
lands, a release starts `macos.yml` by hand at the release's tag. Whether the beta
ships packages for macOS at all is the release ADR's line (work item 490), not this
decision's.

The rules that named both runners change with it. `CLAUDE.md`'s gate for invariant 5
says that `just quickstart` runs on Linux for every pull request and every commit on
`master`, and on macOS each week and at a release. Its working rule keeps "whoever
lands a change watches `master`'s run and fixes a red one forward", and no longer
says that the run holds a macOS result. The `/pr` skill's last step, the reviewer's
brief, `docs/how-to/ask-the-agent.md`, `docs/how-to/implement-a-feature.md` and the
README's two sentences say the same.

A test that waits on a clock, a socket or another process is still held to twenty
runs in a row on both of CI's runners before its pull request opens
(`docs/how-to/run-a-test-many-times.md`; `test-times.yml`, by hand). Linux, because
`gates (linux)` runs the test for every change after that and a failure there stops
every pull request. macOS, because those twenty runs are now the only ones on the
macOS runner that name the change: the week's run names a week of commits.

ADR 0160 stands: on CI's macOS runner the log directory's upgrade gate keeps its
one-tree form. That form's rate of lost runners, about one in twenty, is two or
three red weeks a year at this cadence. The seven-write form's rate seen, one in
four, would be about thirteen. Work item 456 stays parked, by the owner's word, and
work item 457 stays as it is: what `just test` holds in memory matters on a laptop
too.

## Alternatives considered

- **Keep `gates (macos)` on every push to `master`** (ADR 0073). Lost by the
  owner's ruling, and on the counts: 4.0 hours of a macOS runner a day for a net
  under which every change has already passed the same gates on a Mac, and a red
  `master` about every twentieth landing that no change caused.
- **Run it each night.** Offered by the manager session, and not chosen by the owner.
  At one lost runner in twenty jobs, a nightly run is red for no fault of ours about
  every three weeks; a weekly one, two or three times a year. A night costs seven
  times the runner time of a week. What a night buys is a shorter search, a day's
  commits where a week has about a hundred, and the one finding in the window
  (Evidence) named its own work item without a search.
- **Remove the macOS runner**, the owner's first preference "unless we need to keep"
  it. Lost by a little, as in ADR 0073: the laptop is half of invariant 5, and CI's
  runner is the one Mac that has none of a developer's tools, where `just
  quickstart` and `just package` prove that the README's path needs nothing that the
  README does not name. One job a week keeps that for about 15 minutes of a runner.
- **Add a `schedule` trigger to `ci.yml` itself.** Lost: a scheduled run of `ci` at
  `master`'s head would be listed beside the commit's own run wherever `ci`'s runs
  on `master` are read, which is the `/pr` skill's last step and `/next-task`'s
  first, and it would share the concurrency group of `master`'s runs, where a run
  that starts while another waits takes the waiting run's place.
- **Give `macos.yml` its own copy of the job's steps.** Lost: 23 steps kept the same
  in two files by hand come to differ, and the macOS gates would then pass or fail
  something that the Linux gates do not run.
- **Run the macOS job for a pull request that changes code which differs by
  platform.** Lost: such code is not in a fixed set of paths, and a list of paths
  goes stale without a failure. An author who wants the clean machine before a pull
  request starts `macos.yml` at the branch, which the how-to names.
- **Run `just quickstart` alone each week.** Lost: the whole job is one runner for a
  quarter of an hour, and the tests and the conformance runners are where the code
  that differs on macOS runs.

## Evidence

Counted on 2026-11-26 from GitHub's record of `ci.yml`'s runs for pushes to `master`
created from 2026-11-22 to 2026-11-25 UTC, four days, and each run's jobs:

- 62 commits had a run, 15.5 a day, and `gates (macos)` ran 63 times.
- The 59 jobs that passed took 12.3 to 17.8 minutes, median 15.1. All 63 took 16.2
  hours of a macOS runner, 4.0 hours a day. The owner's brief on work item 479
  reckoned about 4.5 hours, at 17 minutes a job; the count is the measured one. One
  job a week is about a quarter of an hour.
- Three jobs lost their runner, with no failed step and no log: runs 44989472096
  (654037c6), 45091169007 (dddd87a9) and 45549149085 (2387b4be). That is 3 of 63,
  about one in twenty-one, and agrees with ADR 0160's 2 of 39 for the one-tree form.
- One job failed for a cause of ours: run 45598311008 (f4785a97) failed `just test`
  at a test of `cavalry-raft` that work item 282 had added in the commit before,
  b0c7407a, whose own macOS job passed. A helper of the test raced the follower's
  own subscription. The laptop had passed the test 20 of 20; the runner showed the
  race. It was fixed forward as ae417f8d, tests only. `gates (linux)` passed on the
  same commit. That is the window's one
  finding of the macOS job's own, and it is of the kind that twenty runs on the macOS
  runner before a pull request are kept for.
- In the same window `gates (linux)` failed once in 63 jobs, at `just test` (run
  45487811627, 0c68d788).

The changed workflows were tried from a throwaway branch, `trial-479-macos`, which
held this change's two workflow files and a temporary push trigger in each:

- A push started `ci` run 45759398269 with one job, `gates (linux)`, and `macos` run
  45759398361 with one job, `macos / gates (macos)`. The two ran at the same time,
  so the called run is in no group that `ci`'s own runs wait in.
- A run of `macos.yml` by hand at that branch, and a run of `ci.yml` by hand, are in
  this change's pull request body with their jobs, as is the first run of `macos.yml`
  at `master`'s head after the landing.

An independent read of the workflows and the rules by a read-only reviewer came
before the push; what it found is in the pull request's body.

## Consequences

Easier: a landing waits on one job, and `master` is no longer red for a runner that
was lost. A macOS runner works a quarter of an hour a week where it worked four
hours a day. A run of `ci` by hand is Linux alone, which is what a trial of a
workflow most often wants.

Harder: a change that breaks macOS alone is seen up to a week after it lands, among
about a hundred commits, and only if the laptops' gates let it through. The search
is a bisect on a Mac, and on the runner when the laptop passes. The laptop is now
the first net for macOS, so a hang or a failure of the gates there is worth more
than it was (work items 338 and 457). A session that started `ci.yml` by hand to
get a macOS result gets none: the command is `gh workflow run macos.yml --ref
<branch>`.

To maintain: `macos.yml`, the how-to, and `/next-task`'s rule. GitHub stops the
schedules of a public repository with no activity for 60 days, and the how-to says
how to start one again.

Revisit when two weeks in one quarter are red for a failure of ours that the
laptops' gates passed: then the laptop is not the net this decision takes it for,
and the run moves to each night. Revisit when work item 456 finds why the runner is
lost: a run that is never red for nothing can be read each night at no cost in
attention. Revisit if the release ADR ships packages for macOS to users who host
with them: then macOS is a platform that hosts Cavalry, which the owner's ruling
takes it not to be.

## References

- Work item 479, with the owner's ruling and its confirmation. Work items 456, 457,
  338, 490 and 491.
- `.github/workflows/ci.yml` and `.github/workflows/macos.yml`.
- `docs/how-to/read-the-weekly-run-of-the-macos-gates.md`.
- `CLAUDE.md`, "Gates", invariant 5, and "Working rules for agents".
- ADR 0004, ADR 0007, ADR 0016, ADR 0073, ADR 0139, ADR 0142, ADR 0160.
