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
tree is the first red, which this rule reruns, and the rerun passed. That re-record
stays the owner's call. Whether a host's band at a new tree sits higher because of the
change or because of the host is card 198's question; the rule here needs no answer to
it, since master's own runs at the tree pass either way.

## Decision

ADR 0033's trigger gains a second case. A pull request at master's tree that reads red
on a row while master's own runs at that tree pass has measured the host's band at a
tree the entry has not seen. The first such red at a tree is a rerun of the job, as a
process of the baseline's tree reading high is under ADR 0033. The second red at the
same tree, on the same pull request or another, re-records the entry at that tree in
ADR 0033's form, from every process the host has run at the tree, master's runs and
the pull requests' included, the red runs among them, as a person's commit (ADR 0032)
by the session that holds the second red, on its own branch through the review loop,
with the commit naming the two reds by run and row, every gated row's slowest run
before and after, and the cause of every row that loosens by more than 5 percent or
the card that will find it. A pull request's run is still judged against the entry
alone: the gate stays one number per row in `bench/baseline.json`.

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

## Consequences

Easier: a red at a new tree has a rule, and the second red re-records with no call to
wait for; the entry follows the host's band at each tree master reaches.

Harder: a second red costs the re-record's commit, every process's artifact read and
every loosened row explained or carded; the entry loosens to the band at the new tree,
as a re-record under ADR 0033's first case does.

Revisit: when `scripts/bench-compare.py` reads several trees' entries; when card 198
names the host effect behind `grip`'s band, which may tighten what a re-record admits.

## References

- ADR 0029, ADR 0032, ADR 0033; `docs/how-to/run-the-bench.md`, step 4
- `bench/baseline.json`; `scripts/bench-compare.py`
- Pull requests 102, 103, 106, 111 and 114; CI runs 41853288029 (attempts 1 and 2),
  41852170961, 41868019184, 41870700774, 41874887478 and 41884701705; AppShapes
  Trello cards 198 and 199
