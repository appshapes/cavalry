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

The case the rule left out: master moves to a new tree by a data-plane change whose
own runs pass, and pull requests at that tree then read red on a row they do not touch,
against an entry recorded at the older tree. On `grip`'s unchecked
`claim_commit_32_client` row it happened four times in a week: pull requests 102, 103,
106 and 111 read 120 to 132 ns at p99.99 against the slowest run of 119 ns recorded on
2026-10-31, while master's own runs at the tree read 116 to 118 ns. Each time the
remedy was a rerun and a judgement call, and on 2026-11-04 the owner called the
re-record, pull request 114 at 61fbfdd, from the thirteen processes at 7248621's tree,
which ADR 0032 allows as a person's commit; no rule triggered it, and the reviewer of
that pull request asked for one (card 199). Whether a host's band at the new tree sits
higher because of the change or because of the host is card 198's question; the rule
here needs no answer to it, since master's own runs at the tree pass either way.

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

Pull requests 102, 103, 106 and 111 on `grip`'s unchecked `claim_commit_32_client`
row, 120 to 132 ns against 119 ns, with master at 116 to 118 ns at the same tree;
pull request 114's re-record at 61fbfdd from thirteen processes, with every loosened
row's cause or card in its message; card 198 for the host effects behind the band.

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
- Pull requests 102, 103, 106, 111 and 114; AppShapes Trello cards 198 and 199
