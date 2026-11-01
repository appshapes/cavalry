---
name: pr
description: Open this branch's pull request and see it through the review loop from the author's side: the four gates one command at a time, the review record as the body, master merged in, the auto-merge label, then the wait each stage needs and the one manual step each stall needs, and just tidy after the merge
argument-hint: "[card number] | watch <pull request number>"
---

Land a branch on `master` through a pull request. `/commit` is a person's path, straight
to `master`; this is the path for a Claude Code session working a card on a branch, which
CLAUDE.md gives to bots and agents (Working rules for agents; ADR 0004). The pull request
is reviewed by the loop in `docs/how-to/ask-the-agent.md`, and that page describes the
loop from the issue's side. This skill is the author's side: what to run, what to wait
for, and the one thing to do at each stall. Everything the loop does by itself is left to
it; a hand on the controls at the wrong moment is what this skill exists to prevent.

The arguments are: $ARGUMENTS. A card number opens the pull request (section 1) and then
watches it (section 2). `watch <number>` watches an open pull request. Empty: on a branch
with commits ahead of `origin/master` and no pull request, open; with an open pull
request, watch; on `master`, stop and say that `/commit` is the path from `master`.

Merges only. Never `rebase`, never `--force`. On a conflict outside the append files
named below, stop and hand it back.

## 1. Open the pull request

1. **Read the state.** `git fetch --quiet origin`, then `git status` (never `-uall`),
   `git log --oneline origin/master..HEAD`, `git diff` and `git diff --cached`. The branch
   is `<card>-<slug>`; a change sitting on `master` moves to one first with
   `git switch -c <card>-<slug>`. Check the diff against CLAUDE.md as `/commit` does
   (one crate per commit, docs in the same commit, histograms for a data-plane change, a
   dependency's justification, an ADR for `spec/`), and scan every added line for a
   secret. A diff over about a thousand changed lines, generated and recorded files
   aside, is more than one pull request: split it by crate and by plane as
   `docs/how-to/implement-a-feature.md`, step 3.8, says, so that the first review's
   blockers fit one fix run, and open the rest from their own branches after this one
   merges.

2. **Run the gates, one command each.** Read each exit code before the next command:

   ```
   just lint;      echo "lint exit $?"
   just test;      echo "test exit $?"
   just docs;      echo "docs exit $?"
   just lint-docs; echo "lint-docs exit $?"
   ```

   Never chain a gate through a pipe or behind `&&` of a pipe: `just lint 2>&1 | tail -1
   && git commit` reports `tail`'s exit code, and the failed gate goes through. Two
   commits reached pull request 57's branch that way on 2026-10-26, one that failed
   `just lint` and one that did not build, and each cost a review cycle. A non-zero exit
   stops the sequence; fix, then start this step again.

   The laptop's gates compile the laptop's platform alone. A change that adds or
   touches a `cfg(target_os = "linux")` block, or its `not` twin, is type-checked for
   Linux before the push, because the Linux gates and both bench hosts build it:

   ```
   cargo check --workspace --all-targets --target x86_64-unknown-linux-gnu; echo "check exit $?"
   ```

   `rustup target add x86_64-unknown-linux-gnu`, once, installs the target; a check
   needs no linker. Pull request 94's first review on 2026-11-01 was CI's three Linux
   errors in a file every macOS gate had passed.
   A `cfg(target_os)` ladder with more than two arms is checked on a target that is
   neither, `--target x86_64-unknown-freebsd` after `rustup target add` once: on the two
   targets CI runs exactly one arm is live, so a third arm that is dead there, or an arm
   that overlaps another, shows only elsewhere. Pull request 95's second review on
   2026-11-01 was two constants defined twice on every target but Linux and macOS.

3. **Commit.** The message is the review record, as `/commit` writes it: subject
   `<card>: <Imperative subject>` under 72 characters; body with what changed, which
   invariant by number, and the evidence (the gate results, the ADR number, both
   histograms for a change under a data-plane crate, the `just conformance` and `just
   replay-check` results when the change claims correctness). Several commits on the
   branch are fine; the squash merge takes the pull request's title and body as the
   commit on `master` (`auto-merge.yml`), so the branch's messages are drafts of that.

4. **Merge `master` in.** `git merge --no-edit origin/master`. A conflict in a file
   everyone appends to (`CHANGELOG.md`, `docs/SUMMARY.md`, `docs/glossary.md`,
   `spec/README.md`, `counters.toml`, a crate's `lib.rs`) is resolved by keeping both
   sides with `master`'s lines first (CLAUDE.md, Working rules for agents); any other
   conflict is handed back. After a merge commit, run step 2 again.

5. **Check what CI will check.** CI's `lint-docs` runs over `origin/master...HEAD`, the
   whole branch, while the laptop's checks the working tree or the last commit alone
   (`scripts/lint-docs.py`, header). Run CI's range before pushing:

   ```
   LINT_DOCS_RANGE=origin/master...HEAD just lint-docs; echo "lint-docs exit $?"
   ```

   This is also why an ADR Accepted in one commit of the branch and edited in a later
   one passes CI but fails the laptop's `HEAD~1..HEAD` check as "Accepted at HEAD and
   changed": over the branch's range the ADR is new. Trust the range CI runs.

6. **Push and open.** `git push -u origin <branch>`. Write the body to a file, never
   inline: it is the commit message on `master`, so it is the review record in full,
   built from the branch's commit messages the way the loop's `open-pr` job builds it
   from the agent's: what changed, which invariant by number, the evidence, and what CI
   does not check (the bench figures from the host's job for a data-plane change, the
   `just fuzz` result after a parser change). End it with `Closes #<issue>` when an
   issue exists. Then:

   ```
   gh pr create --title "<card>: <Imperative subject>" --body-file <file> --label auto-merge
   ```

   `auto-merge` goes on at creation: the reviewer's approval of the current head then
   arms a squash merge that lands when the required checks pass, with no person in the
   path (`auto-merge.yml`). Leave it off only when the user asked to read the pull
   request first.

7. **Confirm the review started.** Within a minute, `gh run list --workflow "dispatch
   review" --limit 3` shows a `pull_request_target` run titled with the pull request,
   and a minute later `gh run list --workflow "review pr" --limit 3` shows `review pr
   <number>, attempt 1`. The reviewer runs on `master` (ADR 0005), so it is on the
   Actions tab, not among the pull request's checks. Nothing after two minutes:
   `gh pr edit <number> --add-label review` and look again.

## 2. Watch the pull request

Read the state in one go, then wait. A review lands about ten minutes after a push; a
fix run takes up to thirty minutes; the `bench (linux)` and `bench (grip)` jobs each
have their host, `bench` and `grip`, to themselves. Poll no more often than every five
minutes, and touch nothing a rule below does not name. Every wait here is the loop's own work in progress; the stalls are the rules.

```
gh pr view <number> --json state,mergeable,headRefOid,labels,reviewDecision
gh api repos/{owner}/{repo}/pulls/<number>/reviews --jq '.[] | "\(.submitted_at) \(.state) \(.commit_id[0:7]) \(.user.login)"'
gh run list --workflow claude --limit 5
gh run list --workflow "dispatch review" --limit 5
gh run list --workflow "review pr" --limit 5
gh pr checks <number>
```

- **Changes requested by `claude[bot]`.** A `claude` run (event `pull_request_review`)
  starts within a minute and applies the blockers on the branch. Push nothing while it
  runs or waits: one writer per cycle. Its runs queue one at a time per pull request
  and GitHub keeps one pending run per queue, so a push of yours now gets its own
  review, and the fix run that review starts cancels the one already waiting; on pull
  request 57 two reviews lost their fix run that way and the run that did work answered
  a review two heads old. The fixer commits each blocker as its test passes and pushes
  as it goes, so the head may move more than once during one run; the review is of the
  head it leaves. When the run completes, its last job approves the `ci` run GitHub
  held on the agent's push and adds the `review` label if the head has no `dispatch
  review` run yet, or if the fix was an edit to the pull request body alone
  (`claude.yml`, "Ask for the review of the agent's push"), so a `dispatch review` run
  appears within a minute of the run's end. Add `review` yourself only when a minute
  has passed and none has.

- **"Autonomous fix cap reached".** The fourth changes-requested review ends the fixer's
  part (`claude.yml`, `MAX_ATTEMPTS: 3`). The pull request is the author's now: fix,
  step 2, step 4, push. A person's push raises its own `dispatch review` run.

- **A blocker the fixer cannot apply.** The fixer's run denies writes to
  `.github/workflows/**`, `.claude/**`, `spec/**` and `bench/baseline.json`
  (`claude.yml`, `settings.deny`), so a review whose blockers are all there ends its run
  with "Fix session ended without a push landing ... and without a change to the pull
  request body". These are the author's from the first review: apply them, push. A
  blocker on the pull request body alone the fixer applies itself with `gh pr edit`,
  and its run asks for the review of the unchanged head (issue 11); when you edit the
  body yourself, `gh pr edit <number> --body-file <file>` and then `gh pr edit <number>
  --add-label review`, because an edit to the body raises no review.

- **No review fifteen minutes after a head.** `gh run list --workflow "review pr"`: a
  run whose reviewer session ended without a verdict is retried as `attempt 2` by the
  loop itself; a failed `attempt 2` is the second miss, and `gh pr edit <number>
  --add-label review` starts a third. A head with no `dispatch review` run at all, after
  a push of yours: the same label.

- **`CONFLICTING`.** `master` moved under the branch; GitHub starts no run on a
  conflicting pull request. Step 4, step 2, push.

- **A red check.** `gh run view <run id> --log-failed`. `bench (linux)` runs on the host
  `bench` and `bench (grip)` on `grip`: never run a bench on either while its job may be
  running (`gh run list` shows it `in_progress` or `queued`; `pgrep -a cavalry-bench` on
  the host), and a gate failure there is read from `bench-compare`'s own lines before
  anything is changed.

- **Approved.** `auto-merge.yml` arms the merge on an approval whose `commit_id` is the
  current head; an approval of an older head arms nothing, and the newer head gets its
  own review. The merge lands when the required checks pass: `gh pr checks <number>
  --required`.

- **Merged.** `git fetch --quiet origin` and `git log --oneline -1 origin/master` show
  the squash commit with the pull request's title; `gh run list --branch master
  --workflow ci --limit 1` is its CI run, and the pull request is not done until that
  run is green. Then comment the commit hash on the card and archive it, and run
  `just tidy`: it retires the branch locally, with its worktree, and GitHub has already
  deleted it on origin.

## 3. Report

Give the pull request number and URL; each review's verdict, who answered it and with
which push; the squash commit on `master` and its CI run; and that `just tidy` ran. If a
rule above was applied, say which and why. If the pull request waits on a fix run or a
review, say so and what the next check is; do not add a label to hurry it.
