# 0074. Do not run the bench on master for a tree its pull request measured

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 2, 6
- **Supersedes:** none. Amends ADR 0071's Decision for a push: a push whose files can
  move the bench also reports without the hosts when the pushed commit's tree was
  measured on them by the run of the pull request it merged. ADR 0071's list, its
  rule for a push of files that cannot move the bench, and what the bench jobs run
  stand.

## Context

Every landing runs the bench twice. A pull request's run takes the two gate hosts,
`bench (linux)` for 11.3 minutes, `bench (grip)` for 5.2 beside it and the udp pair
for 4.9 after both, 16 minutes at the least (the ten finished runs of 2026-11-11).
The merge is a squash, and `master`'s run at the squash takes the hosts for the same
16 minutes. When the pull request's branch held everything `master` had at the merge,
the squash's tree is the tree of the branch's head, bit for bit: the second run
measures what the first measured.

The hosts take one job at a time, so runs queue. On the night of 2026-11-10 two
sessions landed pull requests in turn, and the runs took 26 to 39 minutes from
push to last check for their 16 minutes of work. Each `master` run stood in that
queue ahead of the next pull request's.

ADR 0071 sends a change that cannot move the bench past the hosts. It left out "what
changed since the last green bench", which needs a rule for merges; this is a
narrower rule with nothing to compare but two tree ids.

## Decision

`scripts/bench-needed.py`, for a push whose files can move the bench, answers
`bench=false` when all of this holds, and `bench=true` as before when any of it does
not or cannot be read:

1. The pushed commit is the merge commit of exactly one merged pull request, from a
   branch of this repository into the pushed branch.
2. The commit before the push is in the pull request's head. A pull request's run is
   of the head merged with its base as the base stood when the run began; with the
   base's last commit before the merge in the head, every such base was in the head,
   so that merge's tree was the head's own.
3. The pull request's head commit has the same tree as the pushed commit.
4. No pull request from the same branch has another base. A run of a head does not
   say which pull request it was for once that pull request has merged, and a run
   for another base would have been of another tree.
5. `ci.yml` has a run for that head, started by a pull request event from that
   branch, in which `bench (linux)`, `bench (grip)` and `bench (udp)` each passed on
   a self-hosted runner, not on the hosted runner a skipped bench job reports from.
   The five newest such runs are read, newest first, and the first that qualifies is
   taken. The run need not have ended: GitHub merges the moment the last required
   check passes, and `master`'s run asks seconds later, while the pull request's run
   may still be finishing `bench (udp peer)`, which is not required.

The `changes` job's log then names the pull request, its head and the run. The next
change's "before" figures are that run's: a pull request body that compares against
`master` reads them from the run named in `master`'s `changes` job, since `master`'s
own bench jobs printed none.

The run of the commit before the push is not asked for: the tree was measured itself,
whatever `master`'s previous run did. A later push of pages still asks, as ADR 0071
has it, and finds this run completed with success.

## Alternatives considered

- **Compare the files changed since the last green bench.** ADR 0071's lost
  alternative, wider than this: it would also spare a pull request's later pushes of
  pages. Still lost: it needs a rule for merges of `master` into a branch. Tree
  identity needs none.
- **Trust the pull request's run when the squash only "looks" up to date**, by the
  branch's merge base. Lost: a tree id is exact and costs two reads, and anything
  less would let an unmeasured tree read green on `master`.
- **Stop running CI on `master` for a squash.** Lost: `gates (macos)` runs only there
  (ADR 0073), and `master`'s run is what every later push's rule reads.
- **GitHub's merge queue**, which runs CI once on the merged result. Lost for now: it
  replaces the loop's arming of a merge (ADR 0004, ADR 0005) and the labels the
  sessions use, for the same saving on the hosts; it stays the answer if conflicts
  between unrelated pull requests come back after the changelog's entries are files
  of their own (card 255).
- **Take a run whose bench jobs were skipped as proof.** Lost: a skipped bench job
  passes in seconds on a hosted runner. The rule asks for the three jobs on
  self-hosted runners.

## Evidence

- `scripts/bench-needed-test.py`, against a fake API: the merge of a pull request
  whose run measured the same tree reports without the hosts and names the pull
  request and the run, with that run still in progress and with it ended; of several
  runs the newest that passed on the hosts is named. Each of these runs the bench: a
  tree that differs, a head that does not hold the commit before the push, no run by
  a pull request event from the pull request's branch, a pull request from the same
  branch into another base, a pull request from another repository, each of the
  three bench jobs reported from a hosted runner, failed, not ended, skipped or
  missing, a push that is not one pull request's merge into this branch, every read
  refused, and every listing of another shape. A push of pages asks for no pull
  request.
- The script, read-only against the repository on 2026-11-11 with a push event
  written for each, answers `bench=false` for the five squashes of that night whose
  files could move the bench and names each pull request's run: ac50dfc (pull request
  185, run 42942288857), 57ee0b9 (187, 42949195714), 62c24f2 (189, 42953414211),
  ae19d99 (188, 42957811545) and 13296f1 (190, 42962653935). `master`'s runs at those
  five held the hosts for 16 minutes each, 80 minutes in all.
- The jobs API gives a job's runner labels: `self-hosted,bench-linux` for
  `bench (linux)` of run 42957811545, and `blacksmith-4vcpu-ubuntu-2404` for the same
  job of run 42947637145, whose bench was skipped under ADR 0071. The squash ae19d99
  and the head 9915b9f of pull request 188 have one tree, 9379a80716ec.

## Consequences

Easier: a landing takes the hosts once, so the queue behind it is half as long.
Harder: `master`'s newest commit may have no bench result of its own, and a reader
must follow the `changes` job's line to the pull request's run. A baseline's
processes at a tree (ADR 0033) can be the pull request's run for that tree. A pull
request that was behind `master` at its merge still runs the bench on `master`, since
its tree is then a new one.

The rule makes seven reads of the API, or eleven when the newest runs of a head did
not pass on the hosts, before the bench jobs of a merge can start; a push that is no
pull request's merge makes one. If any of them fails or changes shape, the answer is
"the bench runs", and the cost is the old one, not an unmeasured tree. The bench
jobs' note, when they report without their hosts, names both reasons and sends the
reader to the `changes` job for which.

Revisit if a regression is found on `master` at a commit this rule passed: then the
tree was not the one measured, and the rule is wrong, not slow.

## References

- `scripts/bench-needed.py`, `measured_as`; `scripts/bench-needed-test.py`, `Merged`.
- `.github/workflows/ci.yml`, the `changes` job.
- `docs/how-to/run-the-bench.md`, "When the bench jobs run".
- ADR 0016, ADR 0033, ADR 0071, ADR 0073.
- Card 254.
