# 0005. Review pull requests in the base repository's context

- **Status:** Proposed
- **Date:** 2026-10-11
- **Invariants affected:** 6; the reviewer's enforcement of 1 to 5 is unchanged
- **Supersedes:** the reviewer's trigger and the Dependabot hand-off of ADR 0004; the
  rest of ADR 0004 stands

## Context

The reviewer of ADR 0004 ran on the `pull_request` event. The `claude-code-action`
mints its GitHub App token through an exchange that validates the workflow file: the
copy at the run's commit must be identical to the copy on `master`, or the exchange
refuses and the action skips itself. For `pull_request` the run's commit is the merge
commit GitHub computes for the pull request, and GitHub recomputes it lazily. On pull
request 2 the refused run (38102414890) checked out merge commit 1c9b251, whose parents
were 8375c12 and 921b808, while `master` was two commits ahead at a55d36b. A pull
request that changes the file itself is refused by design, whatever the state of its
merge commit. The guard added in 13694fb says so on the pull request and leaves the
merge to a person.

That exception is not rare. The action released eleven versions between 2026-09-30
and 2026-10-09 (v1.0.238 to v1.0.248), so Dependabot's weekly grouped pull request
would move the reviewer's own pin most weeks, and every one of those would need a
human merge. ADR 0004's hand-off, `dependabot-review.yml`, exists for a second gap of
the same trigger: a run that Dependabot's own events raise gets no repository
secrets, so the hand-off re-raised the event as the owner by adding the `review`
label, using `pull_request_target` to do it.

`pull_request_target` runs a workflow in the context of the pull request's base: the
workflow file is `master`'s, the declared job permissions apply, and the repository
secrets are present, on Dependabot's pull requests too, as long as the base branch is
not one of Dependabot's. The objection to it is well known: a fork's code running
with the base's secrets, when the workflow checks the fork's head out and executes
it. The action's own documentation (`docs/security.md` at the pinned commit) covers
this trigger and prescribes the checkout: the base at the workspace root, the head in
a subdirectory handed to the agent with `--add-dir`. Its source maps
`pull_request_target` onto its `pull_request` handling (`src/github/context.ts`),
checks out the pull request branch itself only in its tag mode, which the reviewer
does not use, and restores `.claude/`, `CLAUDE.md` and similar paths from the base
branch before starting Claude on any pull request
(`src/github/operations/restore-config.ts`).

## Decision

`review-pull-request.yml` runs on `pull_request_target` (opened, synchronize,
reopened, labeled) and only for a pull request whose head is in this repository. That
condition is now a security boundary, not a convenience: a fork's run would hold the
secrets. Removing it is a new ADR. The root checkout is `master`, so the working
directory, `CLAUDE.md` and `.claude/` are `master`'s; the pull request's head commit
is checked out under `pr-head/` and given to the agent with `--add-dir`, and nothing
in it is executed: the reviewer keeps no `just`, no `cargo`, no write tool, and a job
token that cannot write contents. Dependabot's own events run the reviewer directly,
`dependabot` joins `allowed_bots`, and `dependabot-review.yml` is deleted. The guard
that compared the workflow file with `master` cannot fire under this trigger; it
stays until the first pull request that edits the file is reviewed and merged by the
loop, which is the proof of this decision, and that pull request removes it. This
ADR is Proposed until that happens, and Rejected, with the trigger reverted, if the
exchange refuses the run.

## Alternatives considered

- **Keep `pull_request` and have a person merge each bump that touches the file.**
  Lost: at the action's release cadence that is most weeks, and the loop exists so
  that a Dependabot bump needs no human step.
- **Keep `pull_request` and tell Dependabot to ignore the action.** Lost: the one pin
  that runs a model with repository access is the one that most needs moving.
- **Keep `pull_request` and merge `master` into every branch before review.** Lost:
  it does nothing for a pull request that changes the file, which is the case that
  matters, and adds a push by a token to every branch.
- **`workflow_run` downstream of CI.** Lost: the same base-context model with one more
  hop and the same checkout rules; nothing gained.
- **Open the trigger to fork pull requests.** Lost: a stranger's pull request would
  run with the Claude token in the agent's environment. The no-execute checkout is a
  mitigation, not a boundary. Forks keep CI and a human.
- **Keep the hand-off alongside the new trigger.** Lost: two reviewer runs per
  Dependabot pull request, one cancelling the other, to guard against a semantics
  that GitHub documents and that the hand-off itself already relied on.
- **Check the head out at the root, as before.** Lost: the action's documentation says
  not to, and the reviewer's own configuration would then come from the pull request.
  The action restores those paths from the base anyway, but the workspace shape should
  not depend on it.

## Evidence

- Run 38102414890 on pull request 2 was refused by the exchange; its checkout was
  merge commit 1c9b251 with parents 8375c12 and 921b808 while `master` stood at
  a55d36b, so the run's commit carried the previous copy of the workflow file.
- The hand-off's own `pull_request_target` runs on pull request 2 (38102254538 and
  38102517185, both skipped by their `if`) record the pull request's head commit as
  the run's head; the workflow file those runs loaded was `master`'s.
- `claude-code-action` at 1d6de8cb: `src/github/context.ts` lines 191 to 200 map
  `pull_request_target` to the `pull_request` case; `setupBranch` in
  `src/github/operations/branch.ts` is called from `src/modes/tag/index.ts` only;
  `docs/security.md`, "Using this action with `pull_request_target` or
  `workflow_run`", gives the base-at-root, head-in-subdirectory pattern;
  `src/github/validation/permissions.ts` strips a trailing `[bot]` when matching
  `allowed_bots`.
- `gh release list` for the action on 2026-10-11: v1.0.238 on 2026-09-30 through
  v1.0.248 on 2026-10-09.
- Not yet proven, and added here before acceptance: the first pull request that edits
  `review-pull-request.yml` under this trigger passes the exchange, is reviewed, and
  merges through the loop. The Dependabot path cannot be proven today: every pinned
  action was at its latest release on 2026-10-11.

## Consequences

Easier: a Dependabot bump of any pin, the reviewer's own included, merges with no
human step; the loop is three workflows, not four; a branch cut before a change to
the reviewer file needs nothing merged in.

Harder: the same-repository condition in `review-pull-request.yml` must be read as a
boundary, and a reader who knows the trigger's reputation has to be told why it is
safe here, which the file's header does; the reviewer reads the pull request's tree
under `pr-head/` rather than at the root, so its brief says where to look.

Revisit: when a fork contribution should be agent-reviewed (new ADR, with the
secrets question answered first); when the action's validation or its treatment of
this event changes; when the first Dependabot pull request under this trigger runs,
and, if it is not reviewed, a new ADR.

## References

- ADR 0004; `CLAUDE.md`, "Working rules for agents"
- `.github/workflows/review-pull-request.yml`, `.github/dependabot.yml`,
  `.claude/agents/reviewer.md`
- `claude-code-action` at commit 1d6de8cb0c237e7c15e9e1bdf973826ebae490cc:
  `docs/security.md`, `src/github/context.ts`,
  `src/github/operations/restore-config.ts`, `src/github/validation/permissions.ts`
- GitHub Docs, "Automating Dependabot with GitHub Actions", on secrets and
  `pull_request_target`; GitHub Security Lab, "Preventing pwn requests"
- AppShapes Trello card 75, "Setup repo and SDLC"
