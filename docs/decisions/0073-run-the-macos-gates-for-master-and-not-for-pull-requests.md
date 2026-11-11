# 0073. Run the macOS gates for master and not for pull requests

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 5, 6; the gates of 1 to 4 run as before on Linux
- **Supersedes:** none. Amends ADR 0004's Decision, "the checks `gates (linux)` and
  `gates (macos)` are required", and the "when" of the macOS half of ADR 0007's
  acceptance test and of ADR 0016's "the macOS runner keeps the correctness gates and
  the laptop half of invariant 5": `gates (macos)` runs for pushes to `master` and
  for runs by hand, and is not a required check. What it runs stands.

## Context

`ci.yml`'s `gates` job runs the same steps on two runners, Linux and macOS: `just
lint`, `just test`, `just conformance`, `just loom`, `just miri`, `just docs`, `just
lint-docs`, `just quickstart`, `just replay-check` and a report of the bench. Both
were required checks, so no pull request merged before the macOS job had passed.

The owner asked on 2026-11-11 what the macOS job costs and what it finds, having
dropped the same job from Brigade for giving too little for its time. The ten runs of
that night that finished read: `gates (linux)` 5.0 to 5.7 minutes, `gates (macos)`
7.1 to 7.8 minutes, `bench (linux)` 11.3 minutes, `bench (grip)` 5.2 minutes and the
udp pair 4.9 minutes after both. A run that takes the bench hosts is 16 minutes at
the least, so the macOS job is not what it waits for. A run that takes no host (ADR
0071) ends when the macOS job does: 7.7 minutes, where Linux alone is 5.3.

Over the last 92 finished runs of `ci.yml`, 2026-11-07 to 2026-11-11, `gates (macos)`
failed where `gates (linux)` passed twice. Both were one failure, on `master`: the Go
client's conformance, `a handler that panicked on message 3 did not make Poll return
a *HandlerError`, on a commit that changed an ADR and on a bump of actions' pins. No
change caused it. In those 92 runs the macOS job stopped no change that the Linux job
let through.

## Decision

`gates (macos)` runs for a push to `master` and for a run by hand, and not for a pull
request. It is no longer a check that branch protection requires. The job keeps its
name and its steps: the `gates` job's matrix is chosen by the event, Linux alone for
a pull request and both otherwise. `CLAUDE.md`'s gate for invariant 5 says where
`just quickstart` runs: on Linux for every pull request, and on Linux and macOS for
every commit on `master`. A failure the macOS job finds on `master` is fixed forward
by the session whose landing it was, before anything else: `CLAUDE.md`'s rule that
`master` is green after every commit says so, the `pr` skill's last step waits for
`master`'s run before a card is closed, and that run now holds the only macOS
result. The reviewer's brief says that a pull request has no macOS check, and to
read the macOS side of a change to code that differs by platform.

## Alternatives considered

- **Drop the macOS job, as Brigade did.** Lost by a little. The laptop is half of
  invariant 5, "`cavalry` with no arguments works on a laptop", and macOS is the one
  platform where a process is found with `ps` and not `/proc`, where there is no
  `/dev/shm`, and where the clock's tick is 41.67 ns, and the tree holds code that
  differs by platform for each of the three. On `master` the job costs no pull request a minute, so
  the question is only what a run of it costs, which is one runner for eight minutes
  a landing.
- **Keep it required.** Lost: it is the slowest check of every change that takes no
  bench host, and in 92 runs its two reds were not findings.
- **Run it nightly.** Lost: a failure would then name a day's commits and not one. On
  each push to `master` it names the commit.
- **Keep it on pull requests, not required.** Lost: a check that is red on a pull
  request and does not block it teaches authors and reviewers to read past a red
  check, and the reviewer's brief treats a failed check as a blocker.

## Evidence

- The durations and the count above are from `gh run view` over the runs of
  `ci.yml`: ten finished runs of 2026-11-11 for the durations, and the 92 finished
  runs from 42326458905 to 42957811545 for the failures. The two macOS-only failures
  are runs 42339837022 (fc81ec1) and 42423265916 (a1705d0).
- `scripts/bench-needed-test.py` reads the `gates` job's matrix out of `ci.yml` and
  holds that a pull request has Linux alone and every other event both, under the
  names `gates (linux)` and `gates (macos)`.
- The pull request of this decision is the first to merge with no `gates (macos)`
  check, and `master`'s run at its squash the first to run macOS for a commit no pull
  request ran it for; both are in its body.

## Consequences

Easier: a change that takes no bench host is checked in about five minutes, not
eight. Harder: a change that breaks macOS alone lands, and `master` is red until it
is fixed forward; the session that landed it sees it in the run it already waits for.
`master`'s protection lists `gates (linux)`, `bench (linux)`, `bench (grip)` and
`bench (udp)`; the rule is changed by hand at this decision's landing, as every
change to it is. The intermittent Go conformance failure on macOS is still there and
now shows on `master` alone.

Revisit when a macOS-only failure reaches `master` twice in a month: then the job
returns to pull requests, as a required check. Revisit also if it goes a quarter
without a finding of its own: then Brigade's choice, to drop it, is the cheaper one.

## References

- `.github/workflows/ci.yml`, the `gates` job and the header's paragraph on the
  required checks.
- `CLAUDE.md`, "Gates", invariant 5.
- ADR 0004, ADR 0007, ADR 0016, ADR 0071.
- Card 254.
