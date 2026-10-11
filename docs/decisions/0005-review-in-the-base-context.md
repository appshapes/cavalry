# 0005. Review pull requests in the base repository's context

- **Status:** Accepted
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

The obvious fix, running the reviewer itself on `pull_request_target`, does not work.
That trigger takes the workflow file from the base, and the action's code maps the
event onto its `pull_request` handling, but the exchange refuses the run's OIDC
token outright: pull request 3's run (38114955728) failed three times with "Invalid
OIDC token", and the action's tracker has the same refusal open since 2025-12-02
(issue 713, confirmed as issue 1886 on 2026-10-07): the exchange allowlists the
token's `event_name` claim and `pull_request_target` is not on it. `workflow_dispatch`
is, in the sense that matters: the action lists it among the automation events it is
built for, and a dispatched run on `master` carries a plain branch token. GitHub
documents that a write made with a job's own token raises no workflow event, with
`workflow_dispatch` and `repository_dispatch` as the two exceptions, so a workflow of
this repository can start another one without the owner's token.

## Decision

Two workflows replace the reviewer's trigger and the hand-off. `dispatch-review.yml`
runs on `pull_request_target` (opened, synchronize, reopened, labeled), only for a
pull request whose head is in this repository, holds no secret, checks out nothing,
executes nothing from the pull request, and does two things with the job token: takes
the `review` label off if it is on, and asks GitHub to run the reviewer on `master`
with the pull request's number and head commit as inputs. That is the shape of the
hand-off ADR 0004 already accepted, generalised to every pull request and without the
owner's token. `review-pull-request.yml` runs on `workflow_dispatch` with those two
inputs: it checks their shape, refuses a pull request from a fork, stops when the
head has moved on, checks out `master` at the root so the working directory,
`CLAUDE.md` and `.claude/` are `master`'s, checks the head commit out under
`pr-head/` with no persisted credentials and hands it to the agent with `--add-dir`,
and reviews as before: no `just`, no `cargo`, no write tool, a job token that cannot
write contents, and a review that is posted with the App token as `claude[bot]`, so
the `pull_request_review` event that arms auto-merge and the fixer is unchanged.
Dependabot's pull requests reach the reviewer through the dispatcher like any other,
and `dependabot-review.yml` is deleted. The guard that compared the workflow file
with `master` could not fire on a dispatched run; it stayed until the first pull
request that edited the file, pull request 4, was reviewed and merged by the loop,
which was the proof of this decision, and that pull request removed it.

## Alternatives considered

- **Keep `pull_request` and have a person merge each bump that touches the file.**
  Lost: at the action's release cadence that is most weeks, and the loop exists so
  that a Dependabot bump needs no human step.
- **Keep `pull_request` and tell Dependabot to ignore the action.** Lost: the one pin
  that runs a model with repository access is the one that most needs moving.
- **Keep `pull_request` and merge `master` into every branch before review.** Lost:
  it does nothing for a pull request that changes the file, which is the case that
  matters, and adds a push by a token to every branch.
- **Run the reviewer itself on `pull_request_target`.** Tried first, as pull request
  3, and lost to the exchange's refusal described above. The checkout pattern it
  needed, base at the root and head in a subdirectory, is kept, because a dispatched
  run reads an untrusted head the same way.
- **`workflow_run` after CI.** Lost: the action's code handles it, but whether the
  exchange accepts its token is as unknown as it was for `pull_request_target`, it
  ties the review to CI finishing, and the `review` label path would need a dispatch
  anyway.
- **The job token as the action's `github_token`, skipping the exchange.** Lost: the
  review would be posted by `github-actions[bot]`, a job-token write raises no
  `pull_request_review` event so auto-merge and the fixer never arm, and the
  repository's Actions settings forbid Actions from approving pull requests.
- **Open the dispatcher to fork pull requests.** Lost: a stranger's pull request
  would reach an agent with the Claude token in its environment. The no-execute
  checkout is a mitigation, not a boundary. Forks keep CI and a human.
- **Keep the hand-off alongside the dispatcher.** Lost: it is the dispatcher, with the
  owner's token where the job token now suffices.

## Evidence

- Run 38102414890 on pull request 2 was refused by the exchange's workflow
  validation; its checkout was merge commit 1c9b251 with parents 8375c12 and 921b808
  while `master` stood at a55d36b, so the run's commit carried the previous copy of
  the workflow file.
- Run 38114955728 on pull request 3, the reviewer on `pull_request_target` as commit
  bfe7fb8 had it: the checkouts and the guard's skip worked, then the exchange
  answered 401 "Invalid OIDC token" on each of three attempts. Pull request 3 was
  closed with that as its result.
- `claude-code-action` issues 713 (opened 2025-12-02) and 1886 (2026-10-07, labelled
  bug, duplicate, p2): the same refusal, with the `event_name` allowlist as the
  reporter's diagnosis and no maintainer workaround.
- `claude-code-action` at 1d6de8cb: `src/github/context.ts` lists `workflow_dispatch`,
  `repository_dispatch`, `schedule` and `workflow_run` as automation events and maps
  `pull_request_target` to the `pull_request` case; `setupBranch` in
  `src/github/operations/branch.ts` is called from `src/modes/tag/index.ts` only, so
  the agent mode the reviewer uses leaves the root checkout alone; `docs/security.md`
  gives the base-at-root, head-in-subdirectory pattern; `src/github/validation/
  permissions.ts` strips a trailing `[bot]` when matching `allowed_bots`.
- `gh release list` for the action on 2026-10-11: v1.0.238 on 2026-09-30 through
  v1.0.248 on 2026-10-09.
- The proof, pull request 4, which removed the guard and so edited
  `review-pull-request.yml`: dispatch run 38115409465 on its `opened` event started
  reviewer run 38115417542 on `master` at 05:38:06Z on 2026-10-11 (the `labeled` run
  38115409684 for the `auto-merge` label skipped itself); the exchange issued the App
  token at 05:38:22Z with `github-actions[bot]` as the run's actor; the session took
  36 s and approved head 5962f82 at 05:38:59Z as `claude[bot]`; auto-merge run
  38115466161 merged it at 05:39:15Z as squash commit b9b5e93, whose message is the
  pull request body with no trailer and no footer.
- Not proven: the Dependabot path. Every pinned action was at its latest release on
  2026-10-11, so the first Dependabot pull request is its test.

## Consequences

Easier: a Dependabot bump of any pin, the reviewer's own included, merges with no
human step; no job in the loop's review path holds the owner's token; a branch cut
before a change to the reviewer file needs nothing merged in.

Harder: the reviewer's run is not listed among the pull request's checks, because it
runs on `master`; a person looks for it on the Actions tab under `review pr`, and the
how-to says so. The dispatcher's same-repository condition must be read as a
boundary, and a reader who knows the trigger's reputation has to be told why it is
safe here, which the file's header does. The reviewer reads the pull request's tree
under `pr-head/` rather than at the root, so its brief says where to look.

Revisit: when the exchange accepts `pull_request_target` (issue 713 closed), which
would let the dispatcher go; when a fork contribution should be agent-reviewed (new
ADR, with the secrets question answered first); when the first Dependabot pull
request under the dispatcher runs, and, if it is not reviewed, a new ADR.

## References

- ADR 0004; `CLAUDE.md`, "Working rules for agents"
- `.github/workflows/dispatch-review.yml`, `.github/workflows/review-pull-request.yml`,
  `.github/dependabot.yml`, `.claude/agents/reviewer.md`,
  `docs/how-to/ask-the-agent.md`
- `claude-code-action` at commit 1d6de8cb0c237e7c15e9e1bdf973826ebae490cc:
  `docs/security.md`, `src/github/context.ts`,
  `src/github/operations/restore-config.ts`, `src/github/validation/permissions.ts`;
  issues 713 and 1886
- GitHub Docs, "Automatic token authentication", on events a `GITHUB_TOKEN` write
  does not raise; "Automating Dependabot with GitHub Actions", on
  `pull_request_target`; GitHub Security Lab, "Preventing pwn requests"
- Pull request 3 (closed), run 38114955728
- AppShapes Trello card 75, "Setup repo and SDLC"
