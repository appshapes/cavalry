# 0119. Keep a decision that waits on the Brigade Board, and never as a Proposed file

- **Status:** Accepted
- **Date:** 2026-11-13
- **Invariants affected:** 6
- **Supersedes:** none. Records for Cavalry the owner's rulings of 2026-11-13 on the
  Brigade Board's work items 302 and 307. ADR 0001 stands as it is: a decision does
  not exist until an ADR records it. This says where a decision lives before that.

## Context

ADR 0001 has every decision recorded as an ADR in `docs/decisions/`, and the
template gave a file four statuses: Proposed, Accepted, Superseded by NNNN and
Rejected. A Proposed file was a decision still waiting, on a person or on evidence.

Cavalry used it. Sixteen ADR files carried the Status Proposed at some commit on
`master`: 0004, 0005, 0006, 0010, 0011, 0021, 0024, 0025, 0031, 0033, 0034, 0039,
0040, 0041, 0046 and 0051 (`git log -S'**Status:** Proposed' -- docs/decisions/`).
Each landed as "Propose ADR ..." and was accepted by a later commit, the first on
2026-10-10 and the last, ADR 0051, proposed on 2026-11-05. None has been Proposed
since. Cavalry's working rules now say that an ADR lands Accepted on its evidence
and names no person to accept it, and that a question goes to a person only for what
needs their own hand (`CLAUDE.md`, Working rules for agents; ADR 0004 for the
question, `brigade decision`). So the status had fallen out of use while five places
still named it: `.claude/rules/docs.md`, the template, `scripts/lint-docs.py`, the
`next-task` skill, which read the Proposed files as tasks, and step 2 of
`docs/how-to/implement-a-feature.md`, which said to leave an ADR Proposed while a
prototype was pending.

Since 2026-11-13 Cavalry's work is on the Brigade Board (ADR 0113). A decision that
waits is work that waits: it has a question, an owner and a state, and people and
sessions in five repositories look for it on the board, not in one repository's
files. The owner ruled on work item 302 that ADRs stay, written in the repository in
the same change as the code they explain, and that the board carries a decision
until it is accepted. Asked on work item 307, with the history above in hand,
whether Proposed stays a status a file may carry, the owner ruled: "It goes."

## Decision

A decision that waits on someone, the owner's own hand, another session, or evidence
not yet measured, is a work item on the Brigade Board tagged `decision` beside
`cavalry`. It holds the question, the options and the evidence in the template's
sections, and its ruling is a comment on it. The ask for the owner's ruling is
`brigade decision --to <the owner's roster label> --card <work item>`, with the
question on stdin in a quoted heredoc (`<<'EOF'`), so that the shell expands nothing
in it. Brigade refuses an ask without `--to` and sends nothing. The owner answers
where they choose, on Slack, by mail or in a conversation, and Brigade copies the
answer nowhere. So the session that receives the answer records it on the work item
with `brigade work-item comment <work item>`, the owner's words on stdin. The asking
session, or another session of the member who asked, then closes the ask with
`brigade decision --close <the ask's id>`, the ruling as its outcome on stdin in a
quoted heredoc: Brigade refuses a close with no outcome, and a close by a session
of another member. A session of another member that receives the answer records it
and tells the asker. Once it is ruled, the ADR lands Accepted with the change it
explains, its References name the work item, and the work item's landing comment
names the ADR.

A file is never Proposed. The statuses of an ADR file are Accepted, Superseded by
NNNN and Rejected, and `just lint-docs` refuses a file whose Status is Proposed,
with what to do instead.

A decision a session makes on its own inside a work item needs no work item of its
own: its ADR lands in that work item's change and names it. In Cavalry that is most
decisions, and nothing here changes it: no change waits on a person's approval, and
an ADR is accepted on the evidence that informed it. A `decision` work item is for
the minority that really wait.

`next-task` reads the open work items tagged `decision` and `cavalry` where it read
the Proposed files. A Revisit clause that has fired becomes such a work item.

No existing ADR is rewritten, and no citation changes: the sixteen files above are
Accepted or Superseded today and stay as they are. Nothing is exported from the
board. Invariant 6 and the gate on `spec/` stand: a change to `spec/` still needs an
Accepted ADR in the same change.

## Alternatives considered

- **Keep Proposed as a file's status, beside the board.** Lost: one decision would
  wait in two places, and each reader would have to know which to trust. A Proposed
  file on `master` is also a file that `lint-docs` must treat as not yet binding,
  and that a reader of the repository alone cannot tell from a decision made.
- **Keep Proposed, unused, in the template and the lint.** The state before this
  change. Lost: five places told a session to do what no session had done for 68
  ADRs, and the skill that orders the work read a list that was always empty.
- **A work item for every decision.** Lost: most of Cavalry's decisions are made by
  the session that makes the change, on evidence it measures, and land with it. A
  work item for each would put a wait where the rules say there is none.
- **Export the board's decision into the repository when it is ruled.** Lost by the
  owner's ruling on work item 302: the ADR is written in the repository, in the
  change, by the session that makes it; the board is not a source a file is
  generated from.
- **Rewrite the sixteen ADRs' history, or the pages that say an ADR was proposed.**
  Lost: an Accepted ADR is not edited, and the history is what happened.

## Evidence

- The history above, read from `master` at d3482b24: sixteen files, the commits
  that proposed and accepted each, and none Proposed today among 117 ADRs.
- `scripts/lint-docs.py` on a file whose Status is Proposed, added for the check and
  not committed: "adrs: docs/decisions/9999-a-scratch-decision.md has the Status
  Proposed, which a file does not carry: keep a decision that waits as a work item
  on the Brigade Board tagged `decision` and `cavalry`, and land its ADR Accepted
  with the change it explains (.claude/rules/docs.md, Decisions; ADR 0119)", and
  exit 1. Without the file, `lint-docs: ... ok`.
- `brigade work-item list --tag decision --all`, the command `next-task` now runs,
  answers "no open work items match" on 2026-11-13: no decision of Cavalry's waits.
- The owner's rulings, on the Brigade Board's work items 302 and 307.

## Consequences

Easier: a decision that waits is found where the work is, by every session and
person on the team, with its state and its owner; a file in `docs/decisions/` is a
decision made, with no status to check first; `next-task` reads one place for what
waits.

Harder: a reader of the repository alone does not see a decision that is still
waiting, only that none is recorded, so the board must be read for that; a session
without the board, when `brigade` cannot reach it, can still write an ADR for a
decision it makes but cannot carry one that waits. The convention is the same in the
five repositories of the team, so a change to it is a change in five places.

Revisit: when the board cannot carry a decision's evidence, a table of measurements
or a histogram too long for a work item, for where that evidence waits; when a
decision must be visible as pending to someone who has the repository and not the
board.

## References

- The Brigade Board's work items 302, the rulings, and 307, Cavalry's part
- ADR 0001, ADR 0004, ADR 0113
- Brigade's `docs/developer/reference/commands.md`, for the usage of `brigade
  decision`, `--to` required; Brigade's `docs/team-member/use/notes-and-decisions.md`,
  for the answer, which Brigade does not carry back
- `CLAUDE.md`, Working rules for agents and Documentation; `.claude/rules/docs.md`,
  "Decisions (ADRs)"; `docs/decisions/0000-template.md`;
  `docs/how-to/implement-a-feature.md`, step 2; `.claude/skills/next-task/SKILL.md`;
  `scripts/lint-docs.py`
