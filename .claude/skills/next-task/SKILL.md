---
name: next-task
description: Say what the next task is, read from the repository and its process alone (master's CI, open pull requests and issues, the Cavalry list, the slice ADR's feature order, the gates that print not built yet, Proposed ADRs, fired Revisit clauses, active sessions); an optional count lists that many, in order
argument-hint: "[count]"
---

Answer "What is the next task?" from the repository and its process, not from memory or
preference. Print the top task, or the top N in order, each as a title and a short
description. Every task names the file, card, run or command it was read from, so a
stranger can check it (CLAUDE.md, invariant 6). A task with no source is not listed.

The count is: $ARGUMENTS. Empty means 1. Not a positive integer means 1, and say so.
More than 10 means 10, and say so.

Read everything yourself, with no subagent sweeps: the session that cites a line is the
session that read it. Create nothing, change nothing, start nothing.

## Why this needs no backlog

The process fixes the order of work, so the next task is a reading, not a choice.
`master` is green after every commit (CLAUDE.md, Working rules for agents). A feature is
a card and follows `docs/how-to/implement-a-feature.md`. The slice ADR fixes the order
of the slice's features and which gates each one builds: ADR 0007 for the first slice,
then the ADR its Revisit clause leads to, once that one is Accepted. A gate that prints
`not built yet` is built by the first feature that needs it (CLAUDE.md, Gates). A
Proposed ADR is accepted when the evidence it names exists. An accepted ADR's Revisit
clause says when a new ADR is due.

## 1. Read the state

Run these in parallel. Read `origin/master`, not the local branch: other sessions commit
to `master` while this one runs.

- `git fetch --quiet origin`, then `git log --oneline -30 origin/master`.
- `gh run list --branch master --workflow ci.yml --limit 1`: CI at `master`'s head.
- `gh pr list --state open` and `gh issue list --state open`: work in flight through
  `docs/how-to/ask-the-agent.md`.
- `trello card:list --board "AppShapes" --list "Cavalry" --format json`: the open cards.
  The number is the path segment before the slug in `url`; `description` is the brief,
  with its "Done when" section. On `no such table` or `not found on board`, run
  `trello sync` once and retry. On a missing key or token, say the list was not read and
  continue: the slice ADR still gives the order.
- `brigade sessions`, when `brigade` is installed: the line under an active session is
  what it says it is doing, in its owner's words, unverified and possibly stale.
- `grep -n 'not built yet' justfile`: the gates that do not exist.
- `grep -H -m1 'Status' docs/decisions/*.md`: the Proposed ADRs.
- `grep -n -A4 '^Revisit' docs/decisions/*.md`: the Revisit clauses.
- The Decision section of the slice ADR, `docs/decisions/0007-build-ipc-pub-sub-first.md`:
  the features in order, the card number where it names one, and the gates each feature
  builds. When its Revisit clause has fired (`just quickstart` passes on both runners),
  the slice is done, and until the next slice's ADR is Accepted, writing it is the task.
- For every feature of the slice and every open card:
  `git log --oneline origin/master --grep '^<n>:'`.

## 2. Decide what is done, in flight or free

- A feature of the slice is **done** when its card's "Done when" holds or, with no card,
  when the gates the slice ADR says it builds no longer print `not built yet` and its
  crate's README example is no longer "Not yet". `master` is the only evidence: a
  branch, a pull request or a session's line is not done.
- A task is **in flight** when it has an open pull request or issue, has commits on
  `origin/master` with its card prefix but is not done, or an active session's line
  names it.
- Everything else is **free**.

## 3. Order the candidates

Highest first. Within one rule, the slice ADR's order, then the card's position on the
list.

1. **A red `master`.** CI failed at `origin/master`'s head: the task is to make it
   green, titled with the failing run's subject. A run that is queued or in progress is
   not red; name it in the footer and go on.
2. **Work stuck on a person.** A pull request that is approved without `auto-merge`,
   conflicting, or at the fix cap; an issue labelled `claude` whose run failed. The
   "When it does not work" section of `docs/how-to/ask-the-agent.md` says what each
   needs.
3. **The first feature of the slice that is not done.** With a card: the card, titled
   `<n>: <card title>`, and when commits with its prefix exist, the description says
   which of the card's commits is next. Without a card: the feature, titled in the
   imperative from the slice ADR's words, and the description says that step 1 of
   `docs/how-to/implement-a-feature.md`, writing the card, is the first move. The
   features after it follow in the slice's order, each waiting on the one before.
4. **A Proposed ADR whose evidence now exists.** Its Evidence or Revisit section says
   what acceptance waits on; when that is on `master`, accepting it is a task. When it
   is not, the ADR is not a task.
5. **A fired Revisit clause.** The trigger has happened on `master`, the action it names
   has not, and no open card lists it. Cite the trigger and the evidence that it fired.
6. **Every other open card**, by position on the list.

A task in flight in another session is reported in one line as taken, with the
session's name, and does not count toward N. A task in flight on a pull request or an
issue counts: finishing it is the task.

## 4. Report

For each task, in order:

```
1. <Title>
   <Description>
```

- **Title**: imperative, capitalized, under 72 characters, usable verbatim as the card
  title and the commit subject. A card's own title when one exists, prefixed `<n>: `.
- **Description**: two to four sentences, under 80 words. What it is. Why it is next,
  with the source: a file and section, a card number, a run id or a command's output.
  Which invariant, by number. What the first move is. What it waits on, when N > 1. No
  code block, and at most one path per sentence.

After the list, one footer line beginning `Read:` with what was consulted: `origin/master`'s
head and its CI state, the counts of open pull requests, issues and cards, the slice's
done count out of its total, the Proposed ADRs, and the active sessions. A source that
could not be read is named here.

End with the one command that starts the top task: `/trello-read <n>` when it has a
card, `/trello-create "<title>"` when it does not, or `gh pr view <n>` for a pull
request. Do not run it.
