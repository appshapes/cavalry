# 0110. Let a person's session re-record a bench baseline, after telling the person

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 2, 6
- **Supersedes:** none. Settles who makes the re-record that ADR 0032 calls "a
  person's commit" and that `docs/how-to/run-the-bench.md`, step 4, describes. When a
  baseline is re-recorded and from which processes stay ADR 0033's and ADR 0050's.

## Context

`bench/baseline.json` is what the latency gate judges every change against. ADR 0033
says when an entry is re-recorded and from what: every process a host has run at one
tree. ADR 0050 adds a second case. Neither leaves a choice to whoever makes the
re-record: every run that reached the profile is taken, and the commit lists every
gated row's slowest run before and after and names the cause of every row that
loosens by more than 5 percent.

The pages did not agree on who makes one. ADR 0032 says "a later re-record of a
host's baseline is a person's commit". `docs/how-to/run-the-bench.md` said "The
re-record is a person's commit on a branch of its own" and "A person decides whether
to re-record the baseline from that run", and a few lines on that "a person's
session", a Claude Code session working for a person, "makes the re-record or opens
the card" after a second red. What every page agrees on is who does not: the loop's
writer and fixer, whose runs are denied writes to `bench/baseline.json`
(`.github/workflows/claude.yml`).

On 2026-11-12 a session made one. The bench harness had changed how a subscriber
keeps its samples (ADR 0090), which asks for both hosts' profiles to be re-recorded.
The session told the repository's owner, got no answer, and went on: six runs
started by hand, two hours of both hosts, pull request 243. It said in the pull
request's body that it had gone on without an answer, and asked again afterwards.

## Decision

A re-record of `bench/baseline.json` is made by a person or by a person's session: a
Claude Code session that works a card for a person, on a branch of its own, through
the review loop. It is made only in the cases ADR 0033 and ADR 0050 name, from the
processes they name, with the commit message they ask for. A session tells the
person before it starts: which case, which tree, and how long it holds the hosts. It
does not wait for an answer. The loop's writer and fixer still make none.

## Alternatives considered

- **A person's own hand only**, the reading of ADR 0032 that left the question
  open. A re-record is hours of starting runs one at a time and reading rows, and
  the rules decide each step of it. It needs no credential, account or host access
  that a session lacks, and its pull request is reviewed as any other is. On
  2026-11-12 two other sessions held their pushes for the two hours the re-record
  took; a wait for a person on top would have held them longer for nothing the
  person's answer could change.
- **Ask, and wait for the answer.** The owner's answer to the first one was to keep
  it and not to be asked the next time. A question the rules already answer costs
  the person a turn and the hosts the wait.
- **Any agent, the loop's fixer too.** The fixer holds a red on its own pull
  request. A fixer that could re-record would loosen the gate it had failed. ADR
  0050's rule stays: it records the red, re-runs nothing and changes no baseline.

## Evidence

- Pull request 243 (1989bd1): four profiles written whole from eight processes at
  one tree; the message lists every gated row's slowest run before and after and
  names, for each of the six rows that loosened by more than 5 percent, the process
  and the passes behind it. The reviewer approved it at its head. `master`'s run at
  it, 43207607484, was green in all seven jobs.
- The owner's answer, the same day, to the session that made it: the re-record
  stays, and a session makes a future one on the rules after telling them, without
  waiting. The working rules say the same of every change ("Decide, do, and show the
  evidence; no change waits on a person's approval", `CLAUDE.md`).

## Consequences

- A baseline no longer waits for a person when ADR 0033's or ADR 0050's case has
  come. The session that makes one still agrees the time with the other sessions
  that land changes, since no change under the tree's paths may land while the
  processes are taken.
- The gate's strength rests on the rules and the review, not on who types the
  commit: a row that loosens is named with its cause in the message, and the
  reviewer reads the table.
- To undo a re-record, revert its commit: the entries that stood come back, and the
  next process at the tree is judged against them.
- **Revisit** if a session's re-record admits a row that no process at the tree
  explains, or is made outside the two ADRs' cases: then a re-record goes back to
  a person's own commit.

## References

- [ADR 0032](0032-load-the-producer-position-every-two-milliseconds.md): "a later
  re-record of a host's baseline is a person's commit".
- [ADR 0033](0033-record-a-bench-baseline-from-every-process-at-one-tree.md) and
  [ADR 0050](0050-re-record-at-a-tree-where-pull-requests-read-red.md): when, and
  from which processes.
- [ADR 0090](0090-keep-a-subscribers-samples-out-of-the-histogram-until-its-loop-is-over.md):
  the harness change behind pull request 243.
- [Run the bench](../how-to/run-the-bench.md), step 4. Card 198.
