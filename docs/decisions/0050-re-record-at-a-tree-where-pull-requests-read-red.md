# 0050. Re-record a bench baseline at a tree where pull requests read red while master passes

- **Status:** Accepted
- **Date:** 2026-11-05
- **Invariants affected:** 2, 6
- **Supersedes:** none; adds a second case to the trigger ADR 0033 gives for re-recording
  a bench host's baseline at a new tree, and changes nothing else of ADR 0033

## Context

ADR 0033 records a bench host's baseline from every process the host has run at one
tree, re-records it at a new tree "when master's own run fails or brushes the gate
there with no data-plane change behind it", and judges a pull request's run against
the entry as it stands. A pull request that changes none of the tree's paths has its
head at master's tree, so its run is a process of that tree; a pull request that
changes them is judged at a tree of its own, and a red there is a change.

The case the rule left out: master moves to a new tree, its own runs there pass against
the entry recorded at the older tree, and a pull request at master's tree then reads
red on a row against that entry. No rule of ADR 0033 covers that red: master's own run
did not fail or brush the gate, the pull request's process is of another tree than the
entry's and cannot join it, and it is no change, since the pull request changes none
of the tree's paths. On `grip` it happened at 7248621's tree, which began on 2026-11-04
with 7248621's change to `crates/cavalry-control/src/doctor.rs` and held until
7419b2b. Pull request 111's run 41853288029 at 1545bab, attempt 1, read the unchecked
`claim_commit_32_client` row at 127 ns at p99.99 (runs 119 to 134) against the slowest
run of 119 ns in the entry recorded on 2026-10-31 at the tree of 0782ec5 and 11be900,
+6.7 percent. Its attempt 2 read 115 ns and passed, and master's five runs at the tree
read the row at 110 to 113 ns, their slowest runs at 116 to 118, and passed. Against
that entry it was the tree's one red on `grip`.

Pull request 114's record names three more pull requests that read red on the same
row, 102, 103 and 106, at 120 to 132 ns with 111's. Those three were at trees of their
own, and this rule does not apply to them: 102 changed
`crates/cavalry-client/src/client.rs` and the driver's `conductor.rs`, `sender.rs` and
`spec.rs`, on the client row's own path; 103 changed `crates/cavalry/src/main.rs`; 106
changed `bench/src/main.rs` and `bench/host/check.sh`. All three merged before
7248621, so master's runs at 7248621's tree say nothing about them. A red at a pull
request's own tree stays a change under ADR 0033, and this rule never counts it.

On 2026-11-04 the owner re-recorded `grip`'s two profiles from the thirteen processes
at 7248621's tree, pull request 114 at 61fbfdd, which ADR 0032 allows as a person's
commit; no rule triggered it, and the reviewer of that pull request asked for one
(card 199). The rule below would not have triggered it either: `grip`'s one red at the
tree is the first red, which this rule reruns, and the rerun passed. Nor would a
second red there have re-recorded under it. Eighteen commits on the guard's paths lie
between the entry's tree and 7248621's, pull request 102's change to
`crates/cavalry-client/src/client.rs` among them, so a second red would have been
their question. That re-record stays the owner's call.

Whether a host's band at a new tree sits higher because of a change or because of the
host is card 198's question, and master's own runs passing does not answer it. A
change that moves a row's band up by less than the gate passes at its own tree, judged
by one process, and it merges. Master's runs after it pass too. If two reds at a later
tree then re-recorded the entry, the change's band would be in the entry with no
named cause, and the next change would get its own 5 percent on top. ADR 0033 guards
its own case against this with "no data-plane change behind it". This rule carries
that guard.

## Decision

ADR 0033's trigger gains a second case. A pull request at master's tree that reads red
on a row, against an entry recorded at an older tree, while master's own runs at that
tree pass, has measured the host's band at a tree the entry has not seen, if no change
that the gated rows run lies between the two trees. Three rules decide which reds
count:

- A pull request at master's tree is one that changes none of the tree's paths. A red
  at a pull request's own tree is a change under ADR 0033 and never counts.
- A disturbed run never counts. That is a run whose row's p50 and p99 moved with its
  p99.99 and whose rate fell against every other process at the tree. It is rerun as
  ADR 0033 says, and no entry records it, this rule's re-record included.
- Reds count on one row of one profile, and so of one host. A red on another row, or
  in the host's other profile, starts a count of its own.

The first red that counts on a row at a tree is a rerun of the job, as a process of
the baseline's tree reading high is under ADR 0033; a person's session starts the
rerun, since an agent's job cannot (below). The second on the same row of the same
profile, on the same pull request or another, is not rerun. Where the guard below
holds, it re-records that profile's entry at that tree in ADR 0033's form, from every
process the host has run at the tree whose artifact is still kept, master's runs and
the pull requests' included, and from every red's record, so that the red runs are
among them, and no disturbed run. The commit names the reds by run, attempt and row,
every gated row's slowest run before and after, and the cause of every row that
loosens by more than 5 percent or the card that will find it. Once the entry is at the
tree, a later red there is ADR 0033's join. Until then a third red on the row at the
tree, or any later one, is the second's: it is recorded as the second was and links
the record before it, it is not rerun, and its pull request waits on the same
re-record or the same card, which nothing opens or makes twice. The common case is a
pull request at the tree while the card is open, since every pull request there reads
the row against the same entry.

The guard is ADR 0033's "no data-plane change behind it", read as any such change
since the entry's tree, not only the change that began master's tree. A change behind
the band is a commit on master after the entry's tree, up to master's commit at the
second red, on any of the tree's paths except `crates/cavalry-control` and
`crates/cavalry`. Those two crates are the ones CLAUDE.md does not mark data plane and
the harness does not build. Every other path on the tree is a crate the gated rows
run, the harness, its host check, or the manifests, lockfile and recipes that build
them. The session that holds the second red lists those commits with one `git log`
(`docs/how-to/run-the-bench.md`, step 4), and its record of the red carries the list.

- **The list is empty.** The guard holds, and the second red re-records as above.
- **The list names a commit.** The second red re-records nothing under this rule. The
  band may have risen with the listed changes rather than with the host, and one
  process at each change's own tree could not tell the two apart. So the red is the
  listed changes' question. A person's session opens a card that names the two reds
  and the listed commits on the row's path. The pull request that holds the second
  red waits until the card's answer is on master. The answer is either a fix, which
  begins a new tree, or a re-record that a person makes under ADR 0032, with a commit
  message that says why each row loosened, by any amount. That is how the how-to
  reads a `REGRESSION` on a p99.99: the change is slower at the tail until it is not,
  or the baseline is raised in a commit whose message says why.

ADR 0033's own case, master's red, keeps its words. This ADR reads the guard for its
own case alone.

Each red that counts is recorded before anything else is done about it: the first
before its rerun, the second before the re-record or the card. The second red can come
days later, on another pull request and in another session, and the re-record needs
the first red's process. So the session that holds a red
downloads the attempt's artifact first. A rerun's artifact takes the same name, so a
download by name gives the newest attempt's files alone, and every artifact expires
seven days after its attempt began (Evidence). The session then comments on its pull
request, in a comment of its own. The comment's first line is the record's key:
`ADR 0050 red:`, the profile, the row, and `at the tree of` with the full hash of the
tree's first commit. That commit is the newest commit to change the tree's paths in
the history of the master commit the run merged. The comment then says whether the
red is the first or the second, names the run, the attempt, the job, the pull request
and the row's numbers, and carries both of the attempt's result files whole. The
comment is where the process is kept. A file downloaded into a CI workspace, where the
writer and the fixer run, is gone when the job ends, and a file on a person's disk is
not where the next session looks. A session that holds a red finds the earlier reds on
its row at the tree by listing the repository's comments since the tree's first commit
and keeping those whose body starts with the key. If it finds none, its red is the
first. If it finds one, its red is the second. If it finds two or more, its red is a
later one. The re-record takes every red's result files from their comments.
`docs/how-to/run-the-bench.md`, step 4, has the commands.

The re-record is a person's commit (ADR 0032), on a branch of its own, through the
review loop, and the rerun of a first red is a person's too. An agent's job reads runs
and starts none: it has `actions: read` (`.github/workflows/claude.yml`), and the one
token of the loop that acts on runs is kept out of it on purpose. The agent's session
that can hold a red is the fixer. The writer's session ends before its pull request's
first run, which starts when the pull request opens, and a fixer's own push starts a
run that stays held until its job has ended; so the fixer holds a red when the
reviewer blocked on a red bench job at the head before it. That fixer records the red
as any session does, reruns nothing, and may never change `bench/baseline.json`
(`.claude/agents/developer.md`), so it changes nothing there and opens no card. Its
run then ends with no push, which fails as `developer.md` designs it, and the red is a
person's: after a first red a person's session reruns the job, and after a second it
makes the re-record, or opens the card when the list names a commit. In every case
the pull request that holds the second red waits for the re-record or for the card's
answer, and its job is not rerun: its next run is the one that a merge of master into
its branch starts once that is on master, judged against the entry master then holds.
A pull request's run is still judged against the entry alone: the gate stays one
number per row in `bench/baseline.json`.

## Alternatives considered

- **Judge a pull request against the entry plus master's runs at its tree.** Lost: the
  gate would be two sources, the file and another run's artifacts, which a pull
  request's job would have to fetch with a token and a second compare path, a workflow
  change an agent's branch cannot make (ADR 0004); the entry is the record, and a
  stranger reads the gate from one file.
- **Re-record on the first red.** Lost: one high run at a tree is the band, which
  ADR 0033 reruns, and a re-record from every process is an hour of a session; two
  reds at one tree are the band twice.
- **Let the pull request's run join the older entry.** Lost: ADR 0033 says a process
  of another tree never joins, and an entry that mixed trees would say nothing about
  either.
- **A wider gate.** Lost: ADR 0029's 5 percent and one clock step are the rule, and a
  wider gate hides a regression the same size.
- **A person's call each time, as on 2026-11-04.** Lost: card 199's point; the next
  stranger needs a rule to follow, not a call to wait for.
- **The session that holds the second red re-records, whoever it is.** The first
  version of this decision. Lost: the second red can land on a writer's `claude/...`
  branch, or on the run a fixer's push starts, as on pull request 114. That session
  may never change `bench/baseline.json`, and the reviewer's checklist blocks the
  change, since a baseline an agent rewrote hides a regression. The rule then gave the
  work to a session that may not do it, and fell back to a call each time.
- **Rerun the job after the second red until a run passes, and merge.** Lost: that is
  the coin flip ADR 0033's first alternative lost, and the entry stays where it was,
  so the next pull request at the tree reads red against it again.
- **Count a disturbed run as a red.** Lost: a host disturbance, such as the
  `sysstat-collect` run in ADR 0033's Evidence, plus one ordinary high run would
  re-record, and the re-record would loosen the gate with the disturbance inside it.
  ADR 0033 already reruns such a run and records it in no entry.
- **Count two reds on any rows of a profile.** Lost: two reds on two rows are one high
  run on each row, and ADR 0033 reruns a high run. A re-record rewrites every row of
  the profile, so it needs one row's band measured twice, not two rows measured once.
- **Leave the first red in its job's log and its run's artifact.** Lost: the log holds
  the comparison, not the result file, and only a session that already knows which run
  to open finds it. This ADR's own Evidence came from reading every `bench (grip)` job
  log at the tree. After the rerun, the artifact's name gives the newest attempt's
  files, and every artifact expires after seven days.
- **Keep the first red's result file on the pull request's branch, or on the bench
  host.** Lost: `bench/results/` is ignored, and a squash merge would put a run's file
  on master. A file on the host is reachable only from the host, and keeping it there
  is a workflow change that an agent's branch cannot make (ADR 0004).
- **Drop ADR 0033's guard for this case.** This decision's earlier versions, which
  did so without saying it. Lost: master's runs passing says nothing about a change
  that moved a row's band by less than the gate. Two reds at a later tree would put
  that band in the entry with no named cause, and the next change would get its own
  5 percent on top. Invariant 2 does not trade that away for a re-record with no card.
- **Read the guard as the change that began master's tree.** Lost: it catches two reds
  at the change's own tree, but not two reds at the next tree, once a control-plane
  commit has begun it. The 2026-11-04 case is that shape. 7248621 began its tree with
  a change to `crates/cavalry-control/src/doctor.rs`, and eighteen commits on the
  guard's paths lie between the 2026-10-31 entry's tree and 7248621. Reading back to
  the entry's tree closes the gap.
- **Guard on the data-plane crates alone.** Lost: a change to the harness or a
  dependency bump moves the gated rows as a data-plane change does. The memmap2 bump
  of 07a6332 changed `Cargo.lock` for `cavalry-shm`. The how-to already gives the
  re-record after a harness change to a person.
- **Where the list names a commit, re-record anyway and name the commits.** Lost: the
  commit would name the suspects without measuring them, and the entry would take the
  band all the same. The card is the measurement: whether the band rose with the
  changes or with the host.
- **Let the agent's session rerun the first red.** Lost: its job reads runs and starts
  none, and the token that would let it is kept out of that job so that an agent's
  session cannot rerun a gate until it passes. Giving it one is a workflow change,
  which is not an agent's branch's to make (ADR 0004), and it would reopen the coin
  flip above.
- **Rerun a third red.** Lost: the row's band at the tree is already measured twice,
  and a rerun of the third is the coin flip above on a pull request that waits on the
  same re-record or card as the second.

## Evidence

`grip`'s unchecked `claim_commit_32_client` row at 7248621's tree, judged against the
entry of 2026-10-31, whose slowest run is 119 ns, as `bench-compare` printed it in
each `bench (grip)` job: the median run's p99.99 in ns, then the band of the 21 runs.

```text
run          attempt  commit   whose                p99.99  runs        result
41853288029  1        1545bab  pull request 111     127     119 to 134  REGRESSION, +6.7%
41853288029  2        1545bab  pull request 111     115     113 to 117  ok, -3.4%
41852170961  1        7248621  master               110     105 to 116  ok, -7.6%
41868019184  1        07b6221  master               113     110 to 117  ok, -5.0%
41870700774  1        7cae5b4  master               113     111 to 118  ok, -5.0%
41874887478  1        c4e2489  master               111     104 to 117  ok, -6.7%
41884701705  1        a718d63  master               111     103 to 116  ok, -6.7%
```

Pull request 111's two attempts ran 1545bab merged with master, ab9cf24 in the
harness's first line. Both read the row's p50 at 21 ns and its rate at 14.59 million
claims a second, so attempt 1 is not a disturbed run. Every other `bench (grip)` job
at the tree passed: pull request 109's at 783ace0 and b3b0a95, before it merged as
7248621, 111's at a027b86, 113's at ec9a0ce and f23d419, and 114's at bf5ee43. The
files of pull requests 102, 103 and 106 on the tree's paths, named in the Context, are
from `gh pr view <n> --json files`. Pull request 114's re-record at 61fbfdd is from
thirteen processes, with every loosened row's cause or card in its message. Card 198
holds the host effects behind the band.

The guard at 7248621's tree, against the 2026-10-31 entry at 0782ec5, which is on
master:

- **The guard's list.** `git log --oneline 0782ec5..7248621 -- crates/ bench/src
  bench/host bench/Cargo.toml Cargo.toml Cargo.lock justfile ':!crates/cavalry-control'
  ':!crates/cavalry'` lists eighteen commits. The newest is b6fc3ed (ADR 0045, the
  padding frame), and the oldest is e5943f9. 06e3ccc, pull request 102's squash
  commit, is among them.
- **The tree's paths.** The same range over the tree's paths alone lists twenty-one.
  The three that the guard leaves out are 7248621 (`crates/cavalry-control`), and
  542112a and dd16167 (`crates/cavalry`).
- **What the harness builds.** `bench/Cargo.toml` names `cavalry-shm`,
  `cavalry-client`, `cavalry-driver` and `cavalry-log`, which in turn name
  `cavalry-proto`. None of them names `cavalry` or `cavalry-control`.

Where a first red's process is kept, read on 2026-11-05:

- **Two artifacts, one name.** Run 41853288029's artifacts endpoint lists two
  `bench-results-grip-linux` artifacts: 13336413831 from attempt 1, at 15:07:14Z, and
  13337971965 from attempt 2, at 15:23:04Z. Attempt 2 reran `bench (grip)` alone.
- **The name gives the newest.** `gh run download 41853288029 --name
  bench-results-grip-linux` gives attempt 2's files: the unchecked
  `claim_commit_32_client` row at 115 ns and 14,591,744 claims a second.
- **A rerun of every job.** Run 41196273792, whose attempts 4 and 5 each ran every
  job, lists one artifact per name.
- **Seven days.** Each artifact expires seven days after its attempt began. Attempt 1's
  expires on 2026-11-11 at 15:00:37Z.
- **The size.** A `grip` result file is about 5 KB. Run 41884701705 at a718d63 wrote
  5,077 bytes for `grip-linux` and 5,185 bytes for `grip-linux-unchecked`. A comment
  holds 65,536 characters, so one comment carries both files.
- **The tree.** The commit in pull request 111's results, ab9cf24, has 7248621 as its
  first parent and 1545bab as its second (`gh api
  repos/appshapes/cavalry/commits/ab9cf24…`). The newest commit on the tree's paths in
  7248621's history is 7248621 itself, at 2026-11-04T14:53:53Z.
- **The listing.** Step 4's listing, run with the date of 7419b2b's tree, read the
  repository's twelve comments since that tree began and found no record among them.

## Consequences

Easier: a pull request's red at master's tree has a rule. Where no change the gated
rows run lies between the entry's tree and master's, the second red on a row
re-records with no call to wait for, and the entry follows the host's band to that
tree. A change smaller than the gate cannot get into the entry through this rule.

Harder: every red that counts costs a download and a comment of about 10 KB before its
rerun. A second red costs the re-record's commit, every kept artifact and every red's
record read, and every loosened row explained or carded; the entry loosens to the band
at the new tree, as a re-record under ADR 0033's first case does. A tree older than a
week has lost the artifacts of its first processes, and the re-record is from what is
kept, which can only leave the entry tighter than every process would. When an agent's
session holds a red, its pull request waits for a person's session: the rerun after a
first red, the re-record or the card after a second, with the fixer's run ending in a
failure that says so. It waits for that work, not for a decision, since this rule has
already made the decision. After any change on the guard's paths, until the entry is re-recorded past
it, a second red re-records nothing. It holds its pull request until a card is
answered: whether the band rose with the changes or with the host. Most trees follow
such a change, so the card is the common case and the re-record the rarer one. That
is the gate declining to read a change's band as the host's.

Revisit: when `scripts/bench-compare.py` reads several trees' entries; when card 198
names the host effect behind `grip`'s band, which may tighten what a re-record admits;
when a pull request has waited a week on the card for a second red.

## References

- ADR 0029, ADR 0032, ADR 0033; `docs/how-to/run-the-bench.md`, step 4
- `.claude/agents/developer.md` and `.claude/agents/reviewer.md`, which keep
  `bench/baseline.json` from an agent
- `bench/baseline.json`; `scripts/bench-compare.py`
- Pull requests 102, 103, 106, 111 and 114; CI runs 41853288029 (attempts 1 and 2),
  41852170961, 41868019184, 41870700774, 41874887478 and 41884701705, and run
  41196273792 for its artifacts; AppShapes Trello cards 198 and 199
- GitHub's REST endpoints for a repository's issue comments
  (`repos/{owner}/{repo}/issues/comments`), a run's artifacts and a commit
