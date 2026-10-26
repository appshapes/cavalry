# Ask the agent to make a change

A how-to page: one task, from an idea to a merged commit, with no human step after the
first one. Why the loop is built this way is in ADR 0004
(`docs/decisions/0004-run-agents-on-branches.md`).

## Prerequisites

- Write access to the repository. The agent runs only for a person with write access;
  a comment from anyone else is ignored by design.
- A Trello card for the work, when there is one. Its number becomes the prefix of
  the pull request title and so of the commit on `master`.

## Steps

1. Open an issue. The title is the pull request title the agent will use, so write
   it as a commit subject: `75: Add a timeout to every CI job`, or without the card
   prefix when there is no card. The body is the agent's whole brief: what to change,
   where, what done looks like, and which invariant in `CLAUDE.md` it serves. The
   agent reads nothing else from you.
2. Add the `claude` label. Add `auto-merge` as well if the pull request should merge
   without you once the reviewer approves it and CI is green. Leave `auto-merge` off
   when you want to read the pull request yourself first.
3. Wait. You will see, in this order, on the Actions tab and on the issue:
   - a `claude` workflow run starts within a minute and comments on the issue as it
     works;
   - a pull request opens, titled with your issue title, its body built from the
     agent's commit messages and ending with `Closes #<issue>`;
   - a `dispatch review` run on the pull request starts a `review pr` run, listed on
     the Actions tab rather than among the pull request's checks because it runs on
     `master`, which either approves or requests changes;
   - on requested changes, a second `claude` run applies the blockers on the same
     branch and pushes, then adds the `review` label itself, because a push by the
     agent starts no review on its own; the label starts one, up to three times;
   - with `auto-merge`, an `auto merge` run arms the merge on the approval, and the
     pull request merges as one squash commit when `gates (linux)`, `gates (macos)`
     and `bench (linux)` pass. The issue closes.

   The two `gates` jobs run `just lint`, `just test`, `just conformance`, `just loom`,
   `just miri`, `just docs` and `just lint-docs`; `bench (linux)` runs `just bench`
   and `just bench-compare` on the bench host. The pull request body still carries
   what CI does not check: the bench figures, from the host's job, for a change under
   a data-plane crate, and the `just fuzz` result after a parser change.

## How to verify it worked

`git log --oneline -1 origin/master` after `git fetch` shows one commit whose subject
is your issue title and whose body is the pull request body. The issue is closed with
a link to the pull request. Without `auto-merge`, the pull request is open and
approved, waiting for you to merge it. After the merge, `just tidy` retires the
branch locally; GitHub has already deleted it on origin.

## Other ways in

- Comment `@claude <what you want>` on any issue or pull request. The agent answers
  on that issue, or works on that pull request's branch.
- Add the `claude` label to an existing pull request to start the agent on it.
- Add the `review` label to a pull request to run the reviewer again. The run takes
  the label off when it starts, so you can add it again later.

## When it does not work

- **The agent had nothing to change.** The issue is closed with a comment saying so
  and no pull request. Reopen it with a clearer brief if that is wrong.
- **"The agent run ended in failure without pushing a branch."** The issue stays open
  with that comment and a link to the run. Read the run, then add the `claude` label
  again to retry.
- **"Autonomous fix cap reached."** The reviewer requested changes three times. The
  pull request is yours now: fix it by hand, or write `@claude` with instructions in
  a review to start the loop again.
- **No review arrived.** A review lands within about ten minutes of the push. A
  reviewer session that ends without submitting one is run again once, on its own,
  and a comment on the pull request says so. When the review is more than fifteen
  minutes late, run `gh run list --workflow "review pr"`: each run is named with its
  pull request number and attempt, and a `failure` is the second miss, with what the
  session did in its job summary. Add the `review` label to the pull request to run
  it again. Pull requests from forks are not reviewed by the agent, only by a person.
- **The pull request is approved but not merged.** Either it has no `auto-merge`
  label, or a required check failed. `gh pr checks <number>` shows which.
- **The pull request conflicts with `master`.** `gh pr view <number>` says
  `CONFLICTING`. Such a pull request has no merge ref, so GitHub starts no CI run and
  no fix run on it, whatever its labels, and the reviewer blocks on the missing CI. The
  fixer merges `master` into the branch before each fix cycle, so this happens when
  `master` moves after the fixer's last push, when the fixer could not start, or when
  `master` changed `.github/workflows/`, which the fixer's token cannot push, so it
  leaves that merge to a person and says so in a comment. Merge `master` yourself with
  `git pull --no-edit origin master`, keep both sides with `master`'s lines first in the
  files everyone appends to (listed in `CLAUDE.md`, "Working rules for agents"), run the
  gates and push; the next review starts on the push.
