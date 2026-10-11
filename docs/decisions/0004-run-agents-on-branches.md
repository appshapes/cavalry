# 0004. Run agents on branches, with the owner's token out of every agent's job

- **Status:** Proposed
- **Date:** 2026-10-10
- **Invariants affected:** 1, 5, 6
- **Supersedes:** the bot pull request merge path of ADR 0003 (a merge commit by a
  human); the rest of ADR 0003 stands

## Context

Developers commit straight to `master` (ADR 0003). Bots and agents cannot: a change
nobody has read must be reviewable and revertable as one unit, so they work on
branches and open pull requests. Brigade, the sibling repository, has run such a loop
since 2026-09-10: a writer agent implements a labelled issue on a branch, a read-only
reviewer agent approves or requests changes, a fixer applies the blockers, and a merge
is armed when the reviewer approves. Cavalry ports that loop. Its two secrets exist
on the repository since 2026-10-10: `CLAUDE_CODE_OAUTH_TOKEN`, a Claude Code token
minted for Cavalry, and `GH_ACTIONS_TOKEN`, a personal access token of the owner's
account. Cavalry is public, so every run log is world readable.

Four facts shape the port. First, `claude-code-action` scrubs secrets from an agent's
subprocesses only when `allowed_non_write_users` is set, and setting it lets anyone's
comment start an agent on a public repository; so the scrub is off, and every shell
command an agent runs can read the agent job's environment. Second, a personal access
token reaches every repository its account can reach and does not expire with the
job. Third, a write made with the job's own `GITHUB_TOKEN` raises no workflow event,
while a write made with the personal access token does; the chain issue, pull request,
review, merge only moves on events. Fourth, a workflow run that Dependabot triggers
receives no repository secrets and a read-only token, so the reviewer cannot run
directly on a Dependabot pull request.

`master` has classic branch protection, set by the owner on 2026-10-10: the two CI
checks `gates (linux)` and `gates (macos)` are required, branches need not be up to
date, force pushes and deletions are blocked, and repository admins bypass the rule,
which is what lets a developer's direct commit through. No approval is required by
the rule; the approval requirement lives in the auto-merge workflow. The Claude GitHub
App and the Blacksmith runner app are installed for every repository of the
organisation.

## Decision

Three workflows and one hand-off run the loop, all on Blacksmith runners, with the
`claude-code-action` pinned to a full commit SHA. `claude.yml` is the writer: an issue
labelled `claude`, an `@claude` comment, a `claude` label on a pull request, or a
changes-requested review from the reviewer starts an agent in a job that holds only
`CLAUDE_CODE_OAUTH_TOKEN` and a job token scoped to this repository for the length of
the run. A second job, which runs no agent, holds `GH_ACTIONS_TOKEN` and opens the
pull request from the branch the agent pushed, so the `opened` event fires. A third
job, also without an agent, approves the CI runs GitHub holds on a fixer's push.
`review-pull-request.yml` runs the reviewer on every same-repository pull request
with a job token that cannot write contents, denies the edit tools outright, and
fails the job when no review lands at the pull request's head. `dependabot-review.yml`
is the hand-off: on a Dependabot pull request it uses `GH_ACTIONS_TOKEN`, with nothing
of the pull request checked out, to add the `review` label as the owner, which raises
the event the reviewer can run on. `auto-merge.yml` arms GitHub's own auto-merge, as a
squash, when the reviewer approved the current head of a pull request labelled
`auto-merge`; GitHub then merges once the two required checks pass. The squash commit
takes its subject from the pull request title and its body from the pull request
body, so the pull request body is the review record CLAUDE.md asks of every commit.
Dependabot labels its pull requests `auto-merge`. Fork pull requests get CI and a
human. Every agent runs with `show_full_output` off, an explicit short `allowed_bots`
list, no web tools, and a deny list covering `.github/workflows/`, `.claude/`,
`spec/` and `bench/baseline.json`. The agents' briefs live in `.claude/agents/`, and
`docs/how-to/ask-the-agent.md` is the page for a person who wants work done.

## Alternatives considered

- **One job holding both tokens.** Lost: an agent's shell children inherit the job's
  environment, and the personal access token reaches every repository of the
  account. Keeping it in a job that runs no agent is the one structural control; the
  rest are mitigations.
- **A GitHub App token minted per run instead of the personal access token.** Lost
  for now: it is one more application to register and rotate, Brigade runs on the
  token with the job split above, and the split is the same either way. Revisited
  if the token's reach ever includes a repository that is not the owner's own.
- **`allowed_non_write_users` for the action's secret scrub.** Lost: it admits
  prompts from strangers on a public repository to buy a best-effort scrub.
- **Merge commits for bot pull requests, as ADR 0003 said.** Lost: a squash leaves
  one commit per pull request on `master`, and that commit's message is the review
  record, which is the rule for every commit. Nothing already on `master` is
  rewritten, so the no-rebase rule is untouched.
- **Merging from the workflow when CI is green.** Lost: the personal access token is
  an admin's and bypasses the protection rule, so a direct merge would be a bypass by
  construction. Arming GitHub's auto-merge leaves the waiting to GitHub.
- **A ruleset instead of classic protection.** Lost: the owner chose classic; both
  express what this decision needs.
- **The two secrets copied into the Dependabot secrets store.** Lost: two secrets
  kept by hand in two places, and a Dependabot-triggered run still has a read-only
  job token. The hand-off re-raises the event as the owner and checks out nothing.
- **A reviewer that can push small fixes.** Lost: a reviewer that can push is not a
  reviewer. The fixer is the writer, on the writer's token.
- **A human approval for auto-merge.** Lost: the loop would wait for a person on
  every Dependabot bump. A human can still merge any pull request by hand.
- **GitHub-hosted runners.** Lost: the owner's choice is Blacksmith, which is already
  installed for the organisation, runs macOS as well as Linux, and ran CI in 16 s and
  19 s on the empty workspace (run 38101583751). Blacksmith sits in the runner path
  and sees whatever secrets a job holds; Brigade accepts the same boundary.
- **Web tools for the agents.** Lost: nothing in this repository's work needs the
  web, and a fetch tool is the cheapest way for an injected prompt to send a token
  out. `cargo` and `just` still execute code, so this closes a path, not the class.

## Evidence

- Brigade's reviewer sessions, fourteen that reported a turn count on pull requests
  3, 7 and 8 (2026-09-10 to 2026-09-13), ran 16 to 42 turns, the longest 5 min 41 s.
  Its writer sessions ran 13 to 124 turns. Cavalry starts from Brigade's turn and
  time bounds and re-measures.
- Brigade measured on 2026-09-17 that GitHub holds the runs a fixer's push raises at
  `action_required` whatever the fork approval policy says (Brigade pull request 22),
  which is why the writer workflow carries the approving job.
- Cavalry CI run 38101583751 passed on both Blacksmith labels on 2026-10-11.
- The first runs of each workflow on Cavalry are added here when this decision is
  accepted.

## Consequences

Easier: a person opens an issue, labels it, and reads a pull request; a Dependabot
bump merges with no human step; every agent change is one commit on `master` with a
review record; a stranger can read why each token is where it is from the workflow
files and this page.

Harder: a workflow change needs a developer, since agents may not edit
`.github/workflows/`; a fork contribution waits for a human reviewer; the owner's
token is rotated on any odd run, and so is the Claude token, because the agent's
environment is reachable from anything it executes.

Revisit: when the action supports scrubbing without admitting strangers; when the
personal access token's reach grows beyond the owner's own repositories; when the
first measured Cavalry runs justify tighter turn or time bounds; when `spec/` exists
and the reviewer needs a deterministic guard for it, as Brigade has for its release
pins.

## References

- `CLAUDE.md`, "Working rules for agents"; ADR 0001; ADR 0003
- `.github/workflows/ci.yml`, `claude.yml`, `review-pull-request.yml`,
  `dependabot-review.yml`, `auto-merge.yml`; `.github/dependabot.yml`
- `.claude/agents/reviewer.md`, `.claude/agents/developer.md`;
  `docs/how-to/ask-the-agent.md`
- Brigade: `.github/workflows/claude.yml`, `review-pull-request.yml`,
  `auto-merge.yml` and `.context/plans/agentic-workflows.md` section 1.1.1
- `claude-code-action`'s `docs/security.md` on the conditional scrub
- AppShapes Trello card 75, "Setup repo and SDLC"
