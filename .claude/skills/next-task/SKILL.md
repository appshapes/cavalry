---
name: next-task
description: Say what the next task is, read from the repository and its process alone (master's CI, open pull requests and issues, Cavalry's work items on the Brigade Board, the slice ADR's feature order, the gates that print not built yet, the decisions that wait on the board, fired Revisit clauses, active sessions); an optional count lists that many, in order
argument-hint: "[count]"
---

Answer "What is the next task?" from the repository and its process, not from memory or
preference. Print the top task, or the top N in order, each as a title and a short
description. Every task names the file, work item, run or command it was read from, so a
stranger can check it (CLAUDE.md, invariant 6). A task with no source is not listed.

The count is: $ARGUMENTS. Empty means 1. Not a positive integer means 1, and say so.
More than 10 means 10, and say so.

Read everything yourself, with no subagent sweeps: the session that cites a line is the
session that read it. Create nothing, change nothing, start nothing.

## Why this needs no backlog

The process fixes the order of work, so the next task is a reading, not a choice.
`master` is green after every commit (CLAUDE.md, Working rules for agents). A feature is
a work item on the team's Brigade Board and follows
`docs/how-to/implement-a-feature.md`. The slice ADR fixes the order
of the slice's features and which gates each one builds: ADR 0007 for the first slice,
then the ADR its Revisit clause leads to, once that one is Accepted. A gate that prints
`not built yet` is built by the first feature that needs it (CLAUDE.md, Gates). A
decision that waits, on the owner's own hand, on another session or on evidence not
yet measured, is a work item on the board tagged `decision` beside `cavalry`, and no
file is Proposed: its ADR lands Accepted with the change it explains, once it is
ruled (ADR 0119). An accepted ADR's Revisit clause says when a new decision is due.

## 1. Read the state

Run these in parallel. Read `origin/master`, not the local branch: other sessions commit
to `master` while this one runs.

- `git fetch --quiet origin`, then `git log --oneline -30 origin/master`.
- `gh run list --branch master --workflow ci.yml --limit 1`: CI at `master`'s head.
- `gh pr list --state open` and `gh issue list --state open`: work in flight through
  `docs/how-to/ask-the-agent.md`.
- `brigade work-item list --tag cavalry --all`: Cavalry's open work items on the team's
  Brigade Board, every list included (ADR 0113). The board's lists are Wanting, Doing,
  Done and Hibernating, and the plain listing hides Hibernating. A work item in Done
  was moved there when its last pull request landed and every "Done when" item held,
  so it is finished, though `master` stays the evidence when the two disagree. One in
  Hibernating is parked and is not a candidate. The others are open. For a
  candidate, `brigade work-item show <number>` gives the brief, with its "Done when"
  section, its assignee and its comments. A work item whose description begins "Trello
  card N" is that card, carried under a new number (ADR 0113). When the board cannot be read
  (`config` or `no_board`), say so and continue: the slice ADR still gives the order.
- `brigade sessions`, when `brigade` is installed: the line under an active session is
  what it says it is doing, in its owner's words, unverified and possibly stale.
- `grep -n 'not built yet' justfile`: the gates that do not exist.
- `brigade work-item list --tag decision --all`: the decisions that wait. Cavalry's
  are those also tagged `cavalry`; `brigade work-item show <number>` gives the
  question, the options, the evidence, and the ruling when a comment has it (ADR
  0119). No ADR file is Proposed.
- `grep -n -A4 '^Revisit' docs/decisions/*.md`: the Revisit clauses.
- The Decision section of the slice ADR, `docs/decisions/0007-build-ipc-pub-sub-first.md`:
  the features in order, the card number where it names one (a Trello card's, read
  through ADR 0113's table), and the gates each feature
  builds. When its Revisit clause has fired (`just quickstart` passes on both runners),
  the slice is done, and until the next slice's ADR is Accepted, writing it is the task.
- For every feature of the slice and every open work item:
  `git log --oneline origin/master --grep '^<n>:'`. A commit before 5bbd2ad8 names a
  Trello card by its number, so for the five work items ADR 0113 renumbered, grep the
  Trello number as well.

## 2. Decide what is done, in flight or free

- A feature of the slice is **done** when its work item's "Done when" holds or, with no
  work item,
  when the gates the slice ADR says it builds no longer print `not built yet` and its
  crate's README example is no longer "Not yet". `master` is the only evidence: a
  branch, a pull request or a session's line is not done.
- A task is **in flight** when it has an open pull request or issue, has commits on
  `origin/master` with its work item's prefix but is not done, or an active session's
  line names it.
- Everything else is **free**. A work item in Doing that nothing above holds is free
  too: the session that took it ended, blocked or with a "Done when" item open, and its
  last comment says what is left. Finishing it is the task.

## 3. Order the candidates

Highest first. Within one rule, the slice ADR's order, then the work item's list
(Doing before Wanting), then its number.

1. **A red `master`.** CI failed at `origin/master`'s head: the task is to make it
   green, titled with the failing run's subject. A run that is queued or in progress is
   not red; name it in the footer and go on.
2. **Work stuck on a person.** A pull request that is approved without `auto-merge`,
   conflicting, or at the fix cap; an issue labelled `claude` whose run failed. The
   "When it does not work" section of `docs/how-to/ask-the-agent.md` says what each
   needs.
3. **The first feature of the slice that is not done.** With a work item: the work
   item, titled `<n>: <work item title>`, and when commits with its prefix exist, the
   description says which of the work item's commits is next. Without a work item: the
   feature, titled in the imperative from the slice ADR's words, and the description
   says that step 1 of `docs/how-to/implement-a-feature.md`, writing the work item, is
   the first move. The
   features after it follow in the slice's order, each waiting on the one before.
4. **A decision whose ruling or evidence now exists.** An open work item tagged
   `decision` and `cavalry` says what it waits on. When the ruling is a comment on
   it, or the evidence it names is on `master`, writing its ADR with the change it
   explains is a task. When neither is there, the decision is not a task; when the
   ruling is the owner's and has not been asked for, asking is: `brigade decision
   --to <the owner's roster label> --card <work item>`, with the question on stdin
   in a quoted heredoc. The session that receives the answer records it with
   `brigade work-item comment <work item>`, the owner's words on stdin, so that the
   ruling is the comment this rule reads. The asking session, or another session of
   the member who asked, closes the ask with `brigade decision --close <the ask's
   id>`, the ruling as its outcome on stdin in a quoted heredoc; a session of
   another member that receives the answer records it and tells the asker.
5. **A fired Revisit clause.** The trigger has happened on `master`, the action it names
   has not, and no open work item lists it. Cite the trigger and the evidence that it
   fired. The first move is the work item that carries it, tagged `decision` and
   `cavalry`.
6. **Every other open work item**, Doing before Wanting, then by number.

A task in flight in another session is reported in one line as taken, with the
session's name, and does not count toward N. A task in flight on a pull request or an
issue counts: finishing it is the task.

## 4. Report

For each task, in order:

```
1. <Title>
   <Description>
```

- **Title**: imperative, capitalized, under 72 characters, usable verbatim as the work
  item's title and the commit subject. A work item's own title when one exists,
  prefixed `<n>: `.
- **Description**: two to four sentences, under 80 words. What it is. Why it is next,
  with the source: a file and section, a work item number, a run id or a command's
  output.
  Which invariant, by number. What the first move is. What it waits on, when N > 1. No
  code block, and at most one path per sentence.

After the list, one footer line beginning `Read:` with what was consulted: `origin/master`'s
head and its CI state, the counts of open pull requests, issues and work items, the slice's
done count out of its total, the decisions that wait on the board, and the active sessions. A source that
could not be read is named here.

End with the one command that starts the top task: `/work-item-start <n>` when it has a
work item; `brigade work-item add --tag cavalry` when it does not, with the title, a
blank line and the brief on stdin (what changes, a "Done when" section and the
invariant by number, as `docs/how-to/implement-a-feature.md`, step 1, says); or `gh pr
view <n>` for a pull request. Do not run it.
