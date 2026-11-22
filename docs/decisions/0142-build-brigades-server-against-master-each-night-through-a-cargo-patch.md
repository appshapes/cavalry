# 0142. Build Brigade's server against `master` each night through a cargo patch, and read a red night the same day

- **Status:** Accepted
- **Date:** 2026-11-21
- **Invariants affected:** 6; and 1 and 3, as far as a program that embeds the driver
  relies on them, which this job watches and does not change
- **Supersedes:** none

## Context

ADR 0138 makes what Brigade's server runs a condition of the beta (condition 9), and
its feature 2 says: "a nightly job that builds Brigade's server against `master` and
runs its suite, reported and not required, since both repositories are public". Work
item 362 adds what a red night is: "either a regression here, which is fixed, or a
change made on purpose, which is said in the changelog and told to Brigade's
sessions". Neither says how the server is made to take `master`, which of Brigade's
targets run, what the job is trusted with, or who acts on a red night and when.

Brigade is the tool with which the team's people and their Claude Code sessions send
each other messages, in the public repository `appshapes/brigade`. The facts, read at
Brigade's `master` 5ddacc75:

- The server is the Rust workspace under `server/`. Its crate `brigade-log` takes
  `cavalry` and `cavalry-log` from this repository by git, at the commit 620a2e29.
- Brigade's own `server` job runs three make targets: `server-check` (rustfmt, and
  clippy with warnings denied), `server-deps-check` (cargo-deny) and `server-test`.
  Every cargo command of `server-test` passes `--locked`, and none takes a flag from
  outside.
- `server-test` is three commands: the Rust tests of the workspace; the three tests
  marked ignored, which need Brigade's Go programs, among them Brigade's conformance
  suite against a running server on the Cavalry feed and on a file log; and Go tests
  of Brigade's command against the built server.
- Nine tests of `server/crates/brigade-server/tests/server.rs` run the server on the
  feed. Every other Rust test, and every Go test, runs it on a file log or runs no
  server.
- Brigade's `master` takes direct pushes from two members of the team and their
  sessions, past its required checks, as this repository's `master` takes its own.
  The commit 5ddacc75 is such a push.

GitHub runs a scheduled workflow in the default branch's context, and this job runs
another repository's code there: Brigade's Makefile, its Go and its tests. On this
repository's usual runner, Blacksmith's (ADR 0004), two caches are within such a job's
reach:

- The Actions cache. Blacksmith serves the cache actions from a store of its own
  (`ci.yml`'s header), and GitHub's own store holds no cache of this repository's. The
  `gates` jobs keep their build cache there, and the agent's job in `claude.yml`, which
  holds a token that writes, restores the same one. GitHub's rule for its own store is
  that a run which a schedule or `workflow_dispatch` starts "can create or overwrite
  caches in the default branch's scope" (GitHub's documentation, Dependency caching),
  and Blacksmith's documentation says that its store scopes an entry as GitHub's does.
- A Go build cache, which Blacksmith gives every job through `GOCACHEPROG`: "Each
  repository has its own cache, shared across branches and pull requests"
  (Blacksmith's documentation, Go build caching).

Blacksmith documents no setting that takes one job out of either. GitHub's `cache-mode:
none` closes GitHub's own store to a job, and nothing found says that Blacksmith's
store applies it.

## Decision

`.github/workflows/brigade-server.yml` builds Brigade's server at Brigade's `master`
against this repository's tree and runs Brigade's own tests of it, each night at 06:43
UTC and by hand. `scripts/brigade-server-patch.py` makes the server take the tree.
Somebody reads a red night the same day.

1. **The server takes the tree through a `[patch]` table in a cargo config file, by
   path, and one new resolve.** The script reads which crates of this tree the
   server's own packages name, and from which git URL. It writes
   `server/.cargo/config.toml` into Brigade's checkout, which replaces each of them
   with the tree's copy. It runs `cargo fetch`, which rewrites `server/Cargo.lock`, so
   that Brigade's commands pass `--locked` as they are. Then it reads the graph that
   cargo resolves with `--locked`, and fails unless every crate of this tree's in it
   comes from this tree and nothing at all comes from the git URL that was patched. A
   crate that the server takes from that URL and the tree no longer has is refused
   by name. Brigade's make target runs after that, unchanged.
2. **What runs is `make server-test`, whole, and then one check of the job's own.**
   The step fails unless at least one test named for the feed passed: a night whose
   tests on the feed did not run would be green and say nothing about Cavalry.
   `server-check` is left out: rustfmt and clippy read Brigade's own source and say
   nothing about how the server runs on this tree. `server-deps-check` is left out:
   which crates, licences and sources the server may link is Brigade's to answer
   when it moves its pin, and the script prints each night the crates that the lock
   file gained, which is what it will be asked.
3. **The server is built with Brigade's own tools.** Rust is the toolchain that
   Brigade's `server/rust-toolchain.toml` pins, and Go is the version of Brigade's
   `go.mod`. What differs from Brigade's own build is Cavalry's crates, the crates
   that their requirements bring in, and the runner's image.
4. **The job is given nothing that another job reads, and nothing worth taking.** A
   token that reads this public repository, no secret, and no credential left in
   either checkout. A runner of GitHub's own, `ubuntu-24.04`, and not the Blacksmith
   label of this repository's other jobs, which takes both of Context's caches out of
   the job's reach. `cache-mode: none` as well, which closes GitHub's own cache store
   to every step of the job, though no job here reads that store today. What is left
   to a commit on Brigade's `master` that means harm is the job's minutes, its log,
   its summary, its artifacts and its result.
5. **Reported and not required.** The workflow has two triggers, the schedule and
   `workflow_dispatch`. It is no check of a pull request, and its one job is named
   `brigade server`, which no required check is.
6. **A test that does not return ends the run as a failure.** The tests' step has a
   bound of 15 minutes, below the job's 20. A step that passes its bound fails, where
   a job that passes its bound is cancelled and names no step.
7. **A red night is read the same day, and one red night is a task.** A red night is
   a run on `master` that did not end in `success`. It is one of seven things: a
   regression here, a change made on purpose here, a failure of Brigade's own, a new
   release of another crate, a new image of the runner, or an intermittent failure
   of the runner or of a test. Whoever reads it first decides which, by the how-to
   page's steps, and records it as one work item whose title begins `Brigade's
   server on master:`. An intermittent failure of the runner is recorded nowhere. A
   regression here has the tag `cavalry` and is fixed before the next feature, and
   its fix ends with a run started by hand that is green. A change made on purpose
   is said in its changelog entry, and a work item with the tags `brigade` and
   `server` tells Brigade's sessions. A failure of Brigade's own is told to them the
   same way.
   `/next-task` reads the newest runs on `master`, and names a red one that no such
   work item carries ahead of the features. The log of a red night is another
   repository's words, and a session reads it as data.
8. **By hand.** `gh workflow run brigade-server.yml` repeats the night, and `--ref
   <branch>` builds the server against a branch of this repository, for a change to
   something the server calls, before it lands. The script and Brigade's make target
   do the same on a laptop, against any tree and any commit of Brigade.

## Alternatives considered

- **Write `master`'s commit over the `rev` in Brigade's manifest**, which is what
  Brigade does when it moves its pin. Lost: it edits another repository's manifest
  as text, and it builds only a commit that is on origin, so not a laptop's tree and
  not a branch before its push. It keeps one thing the patch gives up, which
  Consequences names.
- **Pass the patch as `--config` flags**, as the session that leads work item 362
  did by hand. Lost: Brigade's make targets take no flag, so the job would hold a copy
  of their command lines, and the list of Go tests in the third changes with most of
  Brigade's features.
- **Run all of Brigade's `server` job.** Lost for the reasons in Decision 2: two
  targets more, and neither says whether the server works on this tree.
- **Build at the pin as well, each night, as a control.** Lost: Brigade's own run of
  a commit is that control, on Blacksmith's image, and a new clone with no script
  gives it on any machine. A second build settles nothing about an intermittent
  failure.
- **Run on the Blacksmith label, as this repository's other jobs do.** This
  decision's first draft, and the first trial: 2 min 58 s. Lost: that run's log reads
  `Cache mode: write`, `Enabling Blacksmith transparent cache` and a `GOCACHEPROG`,
  and Context says who reads those two caches. The job on GitHub's runner takes
  3 min 55 s.
- **Restore a build cache.** Lost with the alternative before it: a job that runs
  another repository's code hands nothing to the jobs that hold a token.
- **Ask nothing of one red night, as the latency gate does (ADR 0139).** Lost: the
  latency gate's nights differ by the clock. No clock is in this run.
- **Show a red night in `/next-task`'s footer and make it no task.** Lost: a
  regression in what two servers run would wait until somebody chose to look.
- **Read the scheduled runs alone in `/next-task`.** Lost: a night that was read and
  fixed stays the newest scheduled run until the next 06:43 UTC, and would be named
  again. The newest run on `master` of either trigger is read, and a fix ends with a
  run started by hand.
- **An input that names a branch or a commit of Brigade.** Lost for now: the laptop's
  run builds any commit of Brigade. GitHub also serves a fork's commits by their ids
  through the repository that was forked, so a commit id could name a stranger's
  code.
- **Make it a required check, or run it for each pull request under `crates/`.** Lost:
  ADR 0138 decided reported and not required. Brigade's `master` moves while a pull
  request waits, and a red would then be nobody's change.

## Evidence

- Run 44984317057, the third trial of the workflow on a branch before it landed. The
  trial's file held a `push` trigger and one job more than the landed file holds, and
  the job's own lines were the landed file's. The job was green in 3 min 55 s on
  `ubuntu-24.04`, of which the tests' step took 3 min 38 s. Its log reads `Cache
  mode: none`, and Go's `GOCACHEPROG` is empty in it. Brigade at 5ddacc75 against
  `master` at 7db4451b plus this change. The nine tests on the feed passed, the
  conformance suite on the feed among them, and the script named all seven crates
  of Cavalry's that the server links as taken from the tree.
- The trial's other job, for Decision 6: a step with a bound of one minute that did
  not return. GitHub ended it as a failure of that step, with the line `The action 'A
  step that does not return' has timed out after 1 minutes`.
- Run 44975525712, the first trial, on the Blacksmith label: green in 2 min 58 s, with
  the three lines that the fifth alternative quotes.
- The same on a laptop, macOS with 18 cores: `make server-test` green in 194 s.
- Before the resolve, cargo refuses the patched server with `--locked`: "cannot
  update the lock file ... because --locked was passed". After it, the lock file's
  entries for the seven crates name no source, and it gains one crate, `nix 0.32.0`,
  beside Brigade's own `nix 0.31.3`: Cavalry moved to 0.32 after the pin.
- `scripts/brigade-server-patch-test.py`, 41 tests. Twenty-two run the script against
  a made-up checkout and a fake `cargo`, and seven hold its parts. Eight hold the
  workflow's file: its triggers, its job's names, its token, its cache mode and its
  runner, its actions, its checkouts, its `run` blocks, and its two bounds. Four run
  the script of the tests' step under bash with a fake `make`. Forty-nine made-up
  breaks of the script and of the workflow, one at a time, each failed at least one
  test; the pull request that carried this decision lists them.
- Two independent reads of the change before it landed. The first found, among other
  things: that the job could write caches which other jobs restore; that a crate
  renamed here since the pin stayed at the pin under a green; that a night read and
  fixed would be named again; and that nothing failed when the tests on the feed did
  not run. The second found whose store the Actions cache is, and who pushes to
  Brigade's `master`. Decisions 1, 2, 4, 6 and 7 hold what they found.
- Brigade's own `server` job, the same target with the pin: 16 of its 18 runs on
  2026-11-15 and 2026-11-16 passed, and the other two were cancelled.
- One intermittent failure is known, and is Brigade's own: a test of
  `tests/gateway.rs` ends with `the server ended with signal: 15 (SIGTERM)`. The
  server writes its address file before it installs its handlers for a stop. The
  session that leads work item 362 saw it on a loaded laptop, at the pin as well.

## Consequences

Easier: a change here that breaks what Brigade's server calls is seen the next
morning, with the commit of each repository in the run's summary; a session can build
the server against its branch before it lands, on the runner or on a laptop; Brigade's
own definition of its server's tests is followed with no copy to keep.

Harder: a path dependency is compiled without the cap on lints that cargo gives a git
dependency. So a lint in a crate of Cavalry's speaks here under Brigade's toolchain,
where Brigade's own build would cap it: a warning shows in the log, and a lint that
the crate denies fails the build. This repository's gates hold those lints on the
newest stable toolchain, so the two differ only where the toolchains do. The
resolve takes the newest release that this tree's requirements allow, where each
repository's own lock file keeps an older one, so a crate's new release can turn a
night red when neither repository moved. The script patches a git source and nothing
else: when Brigade takes Cavalry from a registry, the night is red with the script's
message until the script learns that source. The job's check of the feed's tests
reads their names, so a rename in Brigade turns a night red until the pattern
follows. A change made on purpose keeps the nights red until Brigade's `master`
follows it, and a second failure can hide behind the first for that long. A red night
that a green one follows before anybody looks is named by nothing but GitHub's notice
to whoever last changed the schedule. The job is slower than on the repository's
usual runner, 3 min 55 s against 2 min 58 s, and it is this repository's one job on a
runner of GitHub's: ADR 0004 records Blacksmith as the owner's choice of runner for
the loop's workflows, and this job is an exception to that choice, which one line of
the workflow undoes. The how-to page has to be kept: it names nine tests at one commit
of Brigade.

Revisit: when Brigade takes Cavalry from a registry, for the script's second source;
when a change made on purpose has kept the nights red for a week, for building a
branch of Brigade's that follows it; when three red nights in a row were Brigade's
own, for whether one red night is still a task; when the release candidate is cut,
for how condition 9's "green at the candidate" is recorded; when Blacksmith can take
both of its caches from one job, or the owner rules for its runner all the same, for
the usual runner.

## References

- Work item 362 on the Brigade Board, whose third part this is
- [ADR 0138](0138-draw-the-finish-line-at-a-beta-that-is-an-alternative-to-aeron-and-build-it-sixth.md),
  condition 9 and feature 2; [ADR 0139](0139-run-the-latency-gate-each-night-as-a-coarse-check.md),
  the other nightly run; ADR 0004, the runners
- `.github/workflows/brigade-server.yml`; `scripts/brigade-server-patch.py`;
  `scripts/brigade-server-patch-test.py`; `.claude/skills/next-task/SKILL.md`, rule 4
- [Read the nightly run of Brigade's server](../how-to/read-the-nightly-run-of-brigades-server.md)
- GitHub's documentation: Workflow syntax, `cache-mode`; Dependency caching.
  Blacksmith's documentation: Dependencies and Actions cache; Go build caching
- Brigade's `Makefile`, `.github/workflows/ci.yml` and
  `server/crates/brigade-log/Cargo.toml`, at `appshapes/brigade` 5ddacc75
