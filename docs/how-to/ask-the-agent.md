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
     branch and pushes, which starts a new review, up to three times;
   - with `auto-merge`, an `auto merge` run arms the merge on the approval, and the
     pull request merges as one squash commit when `gates (linux)` and `gates
     (macos)` pass. The issue closes.

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
- **No review arrived.** The `review pr` run on the Actions tab is red with the reason
  in its last step. Pull requests from forks are not reviewed by the agent, only by a
  person.
- **The pull request is approved but not merged.** Either it has no `auto-merge`
  label, or a required check failed. `gh pr checks <number>` shows which.
