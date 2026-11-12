# Run a test many times in a row

A how-to page: one task, running one test program many times in a row, on a laptop or
on both of CI's runners, to hold a test to more than one pass or to find a failure
that comes once in many runs. Why the tests of three drivers on the loopback are held
to twenty runs on both runners is
[ADR 0077](../decisions/0077-elect-the-leader-by-vote-and-close-a-ballot-on-the-members-heard.md),
"Evidence": what the implementation proves goes in its commit messages, the loopback
tests' twenty runs on both runners among it.

## Prerequisites

- `cargo`, as for every gate.
- A machine that runs nothing else that is timed. Two test runs at once on one machine
  slow each other's clocks, and a test that waits on a clock may then fail for the load
  and not for itself.
- For the runs on CI's runners: `gh`, signed in with write access to the repository.

## Steps

1. Choose the test by the arguments `cargo test` takes: the crate, the test program,
   and a filter when you want one test of it. For the tests of three cluster members
   over three drivers:

   ```sh
   cargo test -p cavalry-raft --test cluster
   ```

   You will see the program's tests run once and `test result: ok. N passed`, with N
   the number of tests you meant to run. A filter that matches no test gives `running
   0 tests` and `0 passed`, and passes: correct the filter before step 2.

2. Run it twenty times in a row on this machine:

   ```sh
   scripts/test-times.sh 20 -p cavalry-raft --test cluster
   ```

   The script builds once, then runs the command twenty times. You will see each run's
   own output, one line after each run, and one line at the end:

   ```text
   test-times: run 1 of 20 passed in 58 s
   ...
   test-times: 20 of 20 runs passed
   ```

   At the first run that fails the script stops, with that run's output above its last
   line and the run's exit code as its own:

   ```text
   test-times: run 7 of 20 failed with exit code 101 after 61 s; its output is above, and the 13 runs after it were not made
   ```

   To see what a test prints, give the test program `--nocapture` after `--`, as
   `cargo test` takes it: `scripts/test-times.sh 20 -p cavalry-raft --test cluster --
   --nocapture`.

3. Run it twenty times in a row on both of CI's runners, at a branch that is pushed
   and holds both `scripts/test-times.sh` and `.github/workflows/test-times.yml`. A
   branch made from `master` after card 228's change landed holds both; a branch made
   before it holds neither until it merges `master` ("When it does not work", below):

   ```sh
   gh workflow run test-times.yml --ref <branch> -f arguments='-p cavalry-raft --test cluster'
   ```

   `-f times=50` asks for another count, from 1 to 1000, as many as fit in the job's
   90 minutes ("When it does not work", below). The workflow runs the script
   of step 2 once on the Linux runner and once on the macOS runner, as the jobs `test
   times (linux)` and `test times (macos)`. No push and no pull request starts it, and
   it is no check of a pull request.

4. Find the run and wait for it:

   ```sh
   gh run list --workflow test-times.yml --limit 3
   gh run watch <run id>
   ```

   You will see both jobs end with success when all twenty runs passed on both.

5. Put the run's link where the evidence belongs: in the pull request body of the change
   the test is for, with the count and both jobs' results, or on the card of the
   failure you were looking for.

## How to verify it worked

- `gh run view <run id> --log | grep 'test-times:'` prints the lines of step 2 for
  both runners. The last line of each job is `test-times: 20 of 20 runs passed`.
- `gh run view <run id> --log | grep 'test result:'` prints one line for each test
  program each run ran, `test result: ok. N passed; 0 failed; ...`. Check that N is
  the number of tests you meant to run. The script counts a run that ran no test as a
  pass, so twenty runs of a filter that matches nothing also end `20 of 20 runs
  passed`, each with `0 passed`; that is no evidence.

## When it does not work

- `test-times: run 7 of 20 failed with exit code 101 ...`: the test failed. The
  failed run's output is in the job's log, above that line. A failure that comes once
  in many runs is a finding, not a reason to run again: read the test's own message,
  look for what the test waited on, and record the run's link and the message on the
  card before changing anything. A test that fails only when another test run shares
  the machine is a test that depends on the machine's load; say so on the card, with
  both runs.
- A job ends without success and with no `test-times: N of N runs passed` line, and
  its log ends with GitHub's notice that the job has exceeded the maximum execution
  time of 90 minutes: the runs did not fit in the job's bound. Each job stops at 90
  minutes, the workflow's `timeout-minutes`, and the checkout, the toolchain and a
  build from a cold cache take part of them before the first run. The count times the
  time of one run, the `passed in N s` of step 2, has to fit in the rest, and a
  runner can be slower than your laptop. At about a minute a run, as `cluster` takes,
  about 60 runs fit and 1000 do not. The runs whose `passed` lines the log holds did
  pass. Ask for fewer, or start the workflow more than once and put each workflow
  run's link and count on the card.
- `gh workflow run` stops with `HTTP 422` and `Workflow does not have
  'workflow_dispatch' trigger`: the branch does not hold
  `.github/workflows/test-times.yml`, because it was made from `master` before card
  228's change landed. Merge `master` into the branch and push it, then start the
  workflow again:

  ```sh
  git pull --no-rebase --no-edit origin master
  git push
  ```

  `--no-rebase` makes the pull a merge, as this repository's branches take `master`.
  Without it, a git that has no `pull.rebase` setting refuses a branch that has moved
  apart from `master`, with `Need to specify how to reconcile divergent branches`.

- `gh workflow run` stops with `HTTP 422` and `No ref found for: <branch>`: the
  branch is not on origin. Push it, then start the workflow again.
