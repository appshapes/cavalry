# Read the weekly run of the macOS gates

A how-to page: one task, reading the run of the macOS gates that starts each Monday,
and acting on one that is red. Why the gates run on macOS once a week, and not for
every commit, is
[ADR 0170](../decisions/0170-run-the-macos-gates-each-week-and-at-a-release-and-not-for-every-commit.md).

The run is the workflow `macos` (`.github/workflows/macos.yml`). It starts at 04:23
UTC each Monday at the head of `master`, and whenever a person starts it. It has one
job, `macos / gates (macos)`, which runs every step that `gates (linux)` runs for
each commit, on a macOS runner: `just lint`, `just test`, `just conformance`, `just
loom`, `just miri`, `just docs`, `just lint-docs`, `just quickstart`, `just package`,
`just replay-check` and a report of the bench. It took 12 to 18 minutes when it ran
for every commit; a week apart it may find no cache and take longer, within the
job's bound of 30 minutes. It is no check of a pull request and no run of `ci`, so a
red week is not a red `master`.

## Prerequisites

- `gh`, signed in. Reading a run needs no more. Starting a run, or running one again,
  needs write access to this repository.
- `brigade`, for the commands of step 3 and step 6. They read and write the team's
  Brigade Board, where a work item is the unit of work
  ([Implement a feature](implement-a-feature.md), Prerequisites).
- For step 5, a Mac with the tools that [Implement a feature](implement-a-feature.md)
  lists under Prerequisites, and a checkout of this repository.

## Steps

1. List the newest runs on `master`, the newest first:

   ```sh
   # ignore: needs gh signed in
   gh run list --workflow macos.yml --branch master --limit 3
   ```

   You will see one line for each Monday, and one for each run that a person started
   at the head of `master`. In a terminal the first column is a mark: `✓` for a run
   that passed and `X` for one that did not. A run that passed needs nothing more.
   `gh run view <run id>` says whether an `X` failed or was cancelled. Either is a
   red week, with one exception: a run that waited behind another and was cancelled
   with no job, because a run started by hand took its place. The run that took its
   place, the line above it, is then the week's result. The `/next-task` skill reads
   the same lines before it names a task.

2. For a red week, read which step failed:

   ```sh
   # ignore: needs gh signed in; the run id is from step 1
   gh run view <run id>
   gh run view <run id> --log-failed
   ```

   The first command prints the job, a mark beside each step, and the run's
   annotations. The second prints the failed step's log. There are two kinds of red:

   - **The runner was lost.** The annotations hold `The self-hosted runner lost
     communication with the server`, no step has a log, and the second command prints
     `log not found: <job id>`. No change caused it: about one macOS job in twenty ends this way (work
     item 456;
     [ADR 0160](../decisions/0160-keep-the-log-directorys-gate-on-one-tree-on-cis-macos-runner-and-track-the-lost-runner-as-a-fault-of-its-own.md)).
     Go to step 4.
   - **A step failed.** The log names the command, and for `just test` or `just
     conformance` the test and its message. Write down the step and the test: steps
     3 and 6 use them.

3. Find out whether somebody has read this failure already:

   ```sh
   # ignore: needs brigade, in a checkout of the team's
   brigade work-item list --all | grep "macOS gates on master:"
   ```

   A work item for a red week has a title that begins `macOS gates on master:` and
   then names the step or the test. `brigade work-item show <number>` prints one.

   - One in Wanting or Doing names this failure: it is known. Comment on it with this
     run's id (`brigade work-item comment <number>`, the text on stdin) and stop.
   - One in Done names it, and this run began after it moved to Done: the fix did not
     hold. Move it back with `brigade work-item move <number> doing`, comment with
     this run's id, and go to step 5.
   - None names it: go on.

4. Run the failed job again, once:

   ```sh
   # ignore: needs gh signed in, with write access
   gh run rerun <run id> --failed
   gh run watch <run id>
   ```

   The second attempt runs the same commit. When it passes after a lost runner, the
   week is green: comment the run's id on work item 456, which counts the losses, and
   stop. When it passes after a step failed, the failure comes and goes: add a work
   item for it as step 6 says, and hold the test to many runs on the macOS runner
   with [Run a test many times in a row](run-a-test-many-times.md). When the same
   step fails again, the failure is ours: go on.

5. Find the change, on a Mac. The run's commit is the bad one, and the commit of the
   last week that passed is the good one:

   ```sh
   # ignore: needs gh signed in, and changes what the checkout holds
   gh run view <run id> --json headSha --jq .headSha
   gh run list --workflow macos.yml --branch master --status success --limit 1 --json headSha --jq '.[0].headSha'
   git bisect start <bad commit> <good commit>
   git bisect run just test
   git bisect reset
   ```

   Give `git bisect run` the smallest command that fails: `cargo test -p <crate>
   --test <name>` for one test program, or the gate that failed, `just conformance`
   or `just quickstart`. A week of `master` is about a hundred commits, so the search
   takes about seven runs of that command. You will see `<commit> is the first bad
   commit` and the commit's subject, which begins with its work item's number.

   When the command passes on the Mac at the bad commit, the failure needs the
   runner: a machine with nothing installed, six virtual processors and a slower
   clock. Search on the runner then. Make a branch at a chosen commit, push it, and
   run the failing test twenty times there with `gh workflow run test-times.yml
   --ref <branch> -f arguments='...'`
   ([Run a test many times in a row](run-a-test-many-times.md)). For a gate that is
   no test, start the gates there with `gh workflow run macos.yml --ref <branch>`. A
   branch at a commit from before `macos.yml` landed does not hold that workflow.
   Give it `master`'s copy of the two files in a commit of the branch's own, which
   leaves the rest of the tree as the chosen commit has it: `git checkout
   origin/master -- .github/workflows/ci.yml .github/workflows/macos.yml`, then `git
   commit`. The job then runs `master`'s steps on the older tree, so a step whose
   recipe that tree lacks fails for that reason alone.

6. Act on it, and record it. Whoever reads the red week first acts: a session that
   `/next-task` gave the task to, or a person whom GitHub notified of the failed run.
   Add the work item, with the title's first words as below, so that step 3 finds it:

   ```sh
   # ignore: needs brigade, in a checkout of the team's
   brigade work-item add --tag cavalry <<'EOF'
   macOS gates on master: <the step, and the test when there is one>

   What failed, the run's id, the first bad commit and its work item. Whether the
   laptop's gates pass at the first bad commit: ADR 0170 counts the weeks where they
   did.

   ## Done when

   - A run of `macos` at the head of `master` is green.
   EOF
   ```

   Then fix it forward in a pull request, as any change
   ([Implement a feature](implement-a-feature.md)). Tell the session whose commit it
   was. After the fix lands, start the run by hand, below. Its green run is the
   week's result, and the work item moves to Done with that run's id in its landing
   comment.

## Run it by hand

```sh
# ignore: needs gh signed in, with write access
gh workflow run macos.yml
gh run list --workflow macos.yml --limit 1
gh run watch <run id>
```

The first command starts the gates at the head of `master`. `--ref <branch>` starts
them at a branch that holds the workflow file, for a change you want to see on a
clean macOS machine before its pull request opens. At a branch, the job's `just
lint-docs` reads the branch's whole change against `master`, as a pull request's run
does, and not its last commit alone. Nothing requires such a run: a pull
request's required check is `gates (linux)`, and a Mac laptop has run the gates.

## How to verify it worked

- Step 1 lists this week's run, and it passed.
- The run held the macOS job and no other:

  ```sh
  # ignore: needs gh signed in; the run id is from step 1
  gh run view <run id> --json jobs --jq '.jobs[] | "\(.name) \(.conclusion)"'
  ```

  You will see one line, `macos / gates (macos) success`.
- After a fix, the run you started by hand is the newest line of step 1, and it
  passed.

## When it does not work

- **No run is listed for Monday.** GitHub starts a schedule late when it is busy, and
  can skip one. It also stops the schedules of a public repository that has had no
  activity for 60 days. `gh workflow list --all` prints each workflow's state, which
  is `active` while its schedule runs, and `gh workflow enable macos.yml` starts it
  again. Start the week's run by hand.
- **`gh workflow run` stops with `HTTP 422` and `Workflow does not have
  'workflow_dispatch' trigger`, or with `could not find any workflows named
  macos.yml`.** The branch does not hold the workflow file, because it was made before
  the file landed on `master`. Merge `master` into the branch, push it, and start the
  workflow again.
- **A run by hand stays `queued`.** One run of the macOS gates goes at a time for a
  branch, and one more waits behind it. A run that starts while another already
  waits takes the waiting run's place, and GitHub cancels the one that waited.
- **You started `ci` by hand and it ran no macOS job.** `ci` runs the gates on Linux
  alone, for every event. The macOS gates are the workflow `macos`.
- **The run is red at a step that a Mac laptop passes, every time.** The step may
  need a tool that the laptop has and the runner does not. The job's steps in
  `.github/workflows/ci.yml` list what the runner installs, each with its reason.
