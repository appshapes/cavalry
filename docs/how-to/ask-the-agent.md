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
     branch, one commit per blocker, pushed as each lands; when it ends, the run
     approves the `ci` run GitHub held on its last push, deletes the runs held on its
     earlier pushes, and adds the `review` label itself, because a push by the agent
     starts no review on its own; a blocker
     in the pull request body alone it fixes by editing the body, and asks for the
     review of the unchanged head the same way; the label starts one, up to three
     times;
   - with `auto-merge`, an `auto merge` run arms the merge on the approval, and the
     pull request merges as one squash commit when `gates (linux)`, `gates (macos)`,
     `bench (linux)` and `bench (grip)` pass, and the issue closes. For a change no
     file of which can move the bench, a page or an ADR, the bench checks pass in
     seconds without their hosts
     ([ADR 0071](../decisions/0071-run-the-bench-jobs-only-for-a-change-that-can-move-the-bench.md)). A push after the
     approval, yours or the agent's, or a review that requests changes on the approved
     head, disarms the merge; the reviewer's approval of the new head arms it again.

   The two `gates` jobs run `just lint`, `just test`, `just conformance`, `just loom`,
   `just miri`, `just docs` and `just lint-docs`; `bench (linux)` and `bench (grip)`
   run `just bench` and `just bench-compare` on the two gate hosts, `bench` and `grip`.
   The pull request body still carries what CI does not check: the bench figures, from
   the hosts' jobs, for a change under a data-plane crate, and the `just fuzz` result
   after a parser change.

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
- **"Fix session ended without a push landing ... and without a change to the pull
  request body."** The fixer's run ended with nothing it could show: no commit reached
  the branch and the body is as the review found it. Its comment on the pull request
  says what it did. A blocker it may not apply (`.github/workflows/`, `.claude/`,
  `spec/`, `bench/baseline.json`) is yours: apply it and push, and the push starts the
  review. A fixer run that ended early with some blockers done is not this case: it
  pushed those as commits, its comment says which gates ran on the head it left, and
  the review of that head lists what is left.
- **The fixer's push has no gates.** The `claude` run that pushed a fix ends in a
  failure, in its job `approve-fixer-runs`, step "Approve the runs GitHub held on the
  fixer's push", and no review of the push starts. GitHub holds the `ci` run of every
  push the fixer makes until it is approved, and the step approves it; the failure is
  one of two things, and the step's last line says which and names the commands.
  `run <id>, held on the fixer's push <commit>, refused the approval`: the step found
  the held run and GitHub refused the call, with its reason on the line above.
  `<commit>, the head of #<number>, has no ci run listed after two minutes`: the step
  found no `ci` run at the branch's tip, neither in the runs listed under that commit
  nor among the newest hundred runs of `ci.yml`, on a pull request that does not
  conflict with `master`. Run `gh run list --workflow ci.yml --branch <branch>`. A
  run that reads `action_required` is the held one: approve it with
  `gh api -X POST repos/<owner>/<repository>/actions/runs/<id>/approve`. When there is
  none, push to the branch, which raises a run and its review. After an approval by
  hand, add the `review` label to the pull request, since the failed step skipped the
  one that asks for the review. Two endings of the step are warnings and not this
  case, and the run goes on to ask for the review: a pull request that conflicts with
  `master` has no run to hold, and the entry on a conflict below is the one to read;
  and when GitHub did not say within the two minutes whether the pull request
  conflicts, the warning names the two commands that tell (card 215).
- **No review arrived.** A review lands within about thirty minutes of the push, most
  inside fifteen. A reviewer session that ends without submitting one is run again
  once, on its own, and a comment on the pull request says so. When the review is more
  than thirty minutes late, run `gh run list --workflow "review pr"`: each run is named
  with its pull request number and attempt, a run `in_progress` is still working, a
  `failure` is the second miss, with what the session did in its job summary and in
  its log: `gh run view <run id> --log` prints the result, the denied tool calls and
  the session's last text after the line `## Reviewer session, attempt 2 of 2`. The
  first miss is the `attempt 1` run for the same pull request. It is listed below the
  `failure` run, because the list is newest first. Its log holds the same lines after
  the line `## Reviewer session, attempt 1 of 2` among the `review` job's lines: each
  line of `gh run view --log` starts with its job's name, and the `retry` job's lines
  hold that line again, in the script the runner prints before it runs it. That run
  reads `success` or `cancelled` in the list, as its last job ended or was replaced by
  the second attempt, and `gh run view <run id>` shows its `review` job as passed
  either way. A
  run whose `review` job itself reads `cancelled` held no finished session: the review
  of a newer head took its place, or after sixty minutes it hit the job's timeout,
  which runs nothing again (card 213). Add the `review` label to the pull request to run it again. Pull requests from forks are not reviewed by the agent, only by a person.
- **The pull request is approved but not merged.** Either it has no `auto-merge`
  label, or a required check failed, or the merge was disarmed since the approval: a
  push, yours or the agent's, or a review that requested changes on the approved head,
  and the reviewer's approval of the new head arms it again, or of the same head asked
  for with the `review` label (`gh pr view <number> --json autoMergeRequest` reads
  null until then); or a request for changes from a collaborator is open on the head,
  which the `auto merge` run says, and the approval that follows its answer arms the
  merge; or GitHub never saw the push, the next entry. A disarm run that says the
  approval crossed it names the same label. `gh pr checks <number>` shows a failed check; the `auto merge` run on the
  Actions tab shows a moved head.
- **The `auto merge` run failed: "nothing in the loop reviews the branch's tip".** The
  branch moved after the reviewer's approval, and GitHub raised no run for the new
  head: its pull request object still names the approved commit, and the loop reads
  that object. Push a commit to the branch, `git pull --no-edit origin master` and
  push when `master` has moved, or the next change: the push moves the object and
  starts the review of the new head. The same run ending with a notice and a green
  result is the other case: GitHub has the new head's runs, the loop reviews it, and
  there is nothing to do.
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
- **The fixer answered an old review.** Fix runs queue one at a time per pull request,
  and GitHub keeps one pending run per queue: a second changes-requested review while a
  fix run is working queues its run, and a third cancels the queued one. A push of your
  own while a fix run is working is what produces those extra reviews, so push nothing
  while a `claude` run is in progress or queued on the pull request; wait for it to end
  and for the review of its push, which it asks for itself. One writer per cycle.
