# 0126. Run the cluster tutorial's path in just quickstart

- **Status:** Accepted
- **Date:** 2026-11-14
- **Invariants affected:** 5, 6
- **Supersedes:** none. Adds to what the `quickstart` recipe runs: the README's
  ten-minute path as before (ADR 0046), then the cluster tutorial's. Where and when
  CI runs the recipe stands (ADR 0073).

## Context

The fifth slice's fifth feature ends with a page a stranger runs a cluster from,
`docs/tutorials/run-a-cluster.md` (ADR 0052, feature 5; work item 229). The README's
six commands stay six (ADR 0046), and a cluster is three drivers, so the page is a
tutorial of its own. Work item 229 is done when "the tutorial runs as written on both
runners".

A page of commands that nothing runs goes wrong without notice. The first tutorial is
held by `just quickstart`: `scripts/quickstart.sh` types the README's six commands and
fails on the first that does not do what the README says. `just docs` cannot hold this
page: mdBook runs a page's Rust blocks, and this page's blocks are shell commands that
start seven programs in the background, wait for an election and stop a leader.

CI's `gates` job runs `just quickstart` on Linux for every pull request, and on Linux
and macOS for every commit on `master` (ADR 0073). `CLAUDE.md` lists the recipes, and
its gate 5 is `just quickstart`: zero configuration works on a laptop and on a cloud
VM.

## Decision

`just quickstart` runs `scripts/run-a-cluster.sh` after `scripts/quickstart.sh`. The
script types the page's ten steps, with the page's numbers, and fails on the first step
that does not print what the page says the reader will see: three members started with
`cavalry run`, the leader read from `cavalry doctor`, a service beside each member, a
driver for the client, two numbers sent and the total read, the leader stopped and the
next message answered by another member's service with the same total, and the stopped
member and its service started again and reading the log to that total. The page's two
programs are examples of the `cavalry` crate, `adder-service` and `adder-client`. The
page says that `scripts/run-a-cluster.sh` is its path, as the first tutorial says of
`just quickstart`.

The script differs from the page in three ways, each said in its header. Its directory
and its sixteen UDP ports are its own, counted from the process id as the conformance
runners count theirs, so that the gate neither meets nor removes a cluster a reader
left on the page's `/tmp/cavalry-tutorial` and ports 9000 to 9100; the page's members
string is read from the page and held to `cavalry doctor`'s check of it. It stops a
program by its process id where the page uses the terminal's own name for a background
program. And it exports the members string once and takes it out for the client's
driver, where the reader has a second terminal that never had it.

## Alternatives considered

- **A recipe of its own, `just tutorial`, and a step for it in `ci.yml`.** The page's
  claim is gate 5's claim for a cluster: no configuration but the list of members, on
  a laptop and on a cloud VM. A second recipe would give one invariant two gates, a
  thirteenth line in `CLAUDE.md`'s list and a step in the workflow, and the next
  tutorial a third. It is the remedy when the path's time, below, outgrows the gate.
- **Run it in `just conformance`.** Conformance holds a client to `spec/`. A tutorial's
  failure there would read as a client's, and `just conformance` does not run the
  README's path either.
- **Mark the page's blocks for `mdbook test`.** mdBook runs Rust. A shell block that is
  not run is what the page had without a gate.
- **One program that runs the whole cluster in its process**, as
  `examples/cluster-replay-check.rs` does for the replay gate, with the page telling
  what it prints. It would run in about a second. The reader would start no member, no
  service and no client, and would stop no leader: the page would show a result and
  teach none of the commands a cluster is run with.
- **The script on the page's own directory and ports.** Then the gate fails on a
  laptop where a reader's cluster runs, and its cleanup removes that reader's
  directory. What the page's literal string can get wrong, a member's id or an
  endpoint, `cavalry doctor` checks without a port being bound.
- **The client beside a member's driver, or with a driver inside its process.** The
  first makes the reader choose a member that still runs after the leader is stopped.
  The second hides that a client is a program beside a driver, which is what the first
  tutorial taught. A fourth driver, started with the command the reader already knows,
  costs one line of the page.

## Evidence

On the laptop (Apple M5 Pro, macOS 26.6.1), with the binary and the examples built:

- `scripts/run-a-cluster.sh` passed 25 of 25 runs in a row, each on new directories
  and ports. A run took 12 s to 14 s in 16 of them and 17 s or 18 s in 9. Three runs
  timed by step: step 7, the wait for the next leader, took 10.2 s to 10.4 s, which
  is the 10 s a follower waits for a leader it hears nothing from; every other step
  took 0.6 s or less, but step 8 in one run, 5.1 s. That is the session connect
  timeout: the client asks the members in the string's order, and in that run the
  stopped leader was member 0. A member leads about one start in three.
- The next leader's term was 1 in 23 of the runs and 2 in 2, where the two members
  that were left voted twice. The page says so, and the script reads the term from
  `cavalry doctor`.
- `just quickstart` took 1 s before this change and 13 s with it.
- The script can fail. Five changes, each made, built, run and undone. A service
  that keeps a total one above the sum: "the client did not read the total 12". A
  service that adds a number twice: "the client did not read the total 5". The
  page's members string with member 1 named twice: "doctor refused the members
  string the page gives". A client whose first line is worded otherwise: "the client
  did not say that member 1 opened session 0 in term 0". A service that says of
  every reply that it sent it: "the service beside member 1 did not add 5 at
  position 224, or the wrong service replied".

The runners' times are in the pull request that lands this, from its `gates (linux)`
job and from `master`'s `gates (macos)`.

## Consequences

Easier: the page stays true, since a change to what `cavalry run`, `cavalry doctor`,
`adder-service` or `adder-client` prints fails the gate until the page and the script
say the same. The cluster's whole path, an election, sessions, replies, the loss of a
leader and a member's return, runs through the binary and two small programs on every
pull request, where the tests over three drivers run it through the crates.

Harder: `just quickstart` takes 12 s to 18 s longer on the laptop and starts four
drivers. It now depends on the election's timing: every wait in the script is bounded
at 60 s, and a host that cannot elect a leader in that time fails the gate with every
program's output. A change to the page's commands is a change to the script in the
same commit.

Revisit when the cluster tutorial's path takes over 60 s on a runner, or when a third
page needs its path run: then the paths get a recipe of their own, and `just
quickstart` goes back to the README's.

## References

- Work item 229 on the Brigade Board, "Connect a cluster client over the session
  protocol, through the ABI and the five clients".
- [ADR 0052](0052-build-raft-on-the-durable-log-fifth.md),
  feature 5; [ADR 0046](0046-build-the-c-abi-and-the-clients-fourth.md);
  [ADR 0073](0073-run-the-macos-gates-for-master-and-not-for-pull-requests.md).
- [`docs/tutorials/run-a-cluster.md`](../tutorials/run-a-cluster.md);
  `scripts/run-a-cluster.sh`; `crates/cavalry/examples/adder-service.rs` and
  `adder-client.rs`; the `quickstart` recipe of the `justfile`.
