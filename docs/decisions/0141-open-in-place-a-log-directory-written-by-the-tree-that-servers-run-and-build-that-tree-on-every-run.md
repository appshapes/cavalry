# 0141. Open in place a log directory written by the tree that servers run, and build that tree on every run

- **Status:** Accepted
- **Date:** 2026-11-21
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. Builds the log directory's half of `conformance/upgrade/`,
  which ADR 0138 names as part of its feature 2, work item 362. Changes no page of
  `spec/`, no format and no public item.

## Context

Brigade's server embeds the driver from a git dependency pinned at commit 620a2e29,
and two teams' servers run that build in production, each on a log directory it keeps
(ADR 0138, Context and condition 9). The tree at 620a2e29 writes version 2 of the
durable log. `master` writes version 4: versions 3 and 4 each added a file that an
older reader ignores (`spec/durable-log.md`, its opening). Feature 3 of ADR 0138
changes the format again, in work item 287, and ADR 0138 gives the rule that change
must keep: "a driver opens a directory of the format before it in place, with no step
by an operator, and feature 2's gate holds that."

No test opened a directory that an older tree wrote. `conformance/upgrade/run.sh`
(ADR 0136) replays each cluster member's log under the newer build, but its older
build is the last tree of the cluster protocol's version before, which is a later
commit than 620a2e29, its directories are a cluster member's, and nothing in them was
purged.

What a server's directory holds is known from the server's code
(`docs/explanation/what-brigades-server-relies-on.md`): one recording for each run of
the server and for each roll, recordings that a snapshot covers purged, and, after a
kill or a power cut, a last recording that the catalog still lists as live.

## Decision

`conformance/upgrade/log-directory.sh` is the log directory's upgrade gate, and `just
conformance` runs it after `conformance/upgrade/run.sh`. It builds an older tree, N,
beside this tree, N+1. N writes a log directory as Brigade's server writes its own:
two runs of the feed in the shape the server is pinned to, four recordings, and two
releases that purge the first two. It does this twice, once with the last run stopped
and once with it killed. N+1 then opens each directory as N left it, and the gate
holds five things: `cavalry run` starts on it; `cavalry replay` plays each recording
that is left, to the position N answered; the feed begins at the last position N
answered and reads back every record N appended, each at its position, with what N
purged refused; the feed takes a new recording in the pinned shape and another in the
shape the server moves to, `ssc=true` with `term-length=1m`; and one read then crosses
what the two builds wrote. No program of the gate converts, repairs or copies the
directory between N and N+1: that is "in place, with no step by an operator".

N is the tree that servers in production run. Today that is one commit, 620a2e29, and
the script holds it as a constant. The constant changes when the servers change: a
tree is added when a server in production runs it, and removed when no server holds a
directory that only that tree wrote. `CAVALRY_DIRECTORY_FROM` names another N for a
run by hand.

N is built on every run, from `git archive` into `target/upgrade/`, as ADR 0136's
gate builds its own. The program that writes the directory is this tree's
`crates/cavalry/examples/feed-directory.rs` with `crates/cavalry/tests/feed/`, copied
into N's exported tree and compiled there. They are the files `just test` runs, the
server's own use of Cavalry call for call, and they use no call that 620a2e29 lacks.
So the gate holds one more thing: the feed's files compile against the tree the server
is pinned to. When a change must end that, on purpose, the files as they last compiled
against N are kept under `conformance/upgrade/feed-at-<N's first 8 hex digits>/`, the
script copies those into N's tree in place of this tree's, and the change says so
(`docs/how-to/change-a-call-that-brigades-server-makes.md`, "When it does not work").

A change to the durable log's format passes this gate in the pull request that makes
it. Where the new format cannot read the old one as it stands, the driver converts
the directory at its start, and the gate is what shows that it does.

## Alternatives considered

- **Commit a directory that 620a2e29 wrote, as a fixture.** No build of N, so the gate
  is faster by that build. Lost: a recording's first segment file is 128 MiB long,
  sparse on a disk and whole in an archive, so the fixture is binary files that git
  holds for ever or an archive the gate unpacks; nobody can read what a fixture holds
  or write it again without the tree anyway; and a fixture cannot be killed, so the
  directory a power cut leaves would be a second fixture. The build costs 6 s warm on
  the laptop that measured it (Evidence), and ADR 0136 measured 10 s for its own on
  CI's Linux runner.
- **Take N from the history, as ADR 0136's gate does**: the first parent of the commit
  that last moved the first line of `spec/durable-log.md`. Lost for now: that names
  the last tree of version 3, which no server runs and no directory in production was
  written by. The rule is about data that exists. When a release exists, feature 12
  of ADR 0138 runs `conformance/upgrade/` over released versions, and this gate takes
  its trees from the releases then.
- **A writer of its own for N, frozen in `conformance/upgrade/`.** Lost: a second copy
  of the feed, some 900 lines that no gate would compile against this tree, and that would
  be wrong about the server the day the server's file changed. The copy that `just
  test` holds is the one to write the directory with. The frozen copy is what the
  decision falls back to, on the day the two must differ.
- **Compare `cavalry replay` of N with `cavalry replay` of N+1, byte for byte.** Lost
  as the check of the bytes: N's replay needs N's driver on the directory first, which
  for the killed directory is N's recovery and not N+1's, so the gate would need a copy
  of each directory. The feed's process writes one line for each record as it appends
  it, its length, a digest of its bytes and the position after it, and N+1's read is
  held to those lines: to what N was answered, not to what N reads back.
- **Write a recording that spans two segment files.** Lost for its cost: a segment
  file is 128 MiB from the base of the term its recording starts in, so every
  recording that crosses one holds 128 MiB, and the gate would write and read back
  several hundred MiB in every pull request. The format of a segment file is its
  frames and nothing else, and its name is arithmetic that `spec/vectors/` hold. A
  change that touches segment files or their names revisits this.

## Evidence

On the laptop that wrote this ADR, an Apple M5 Pro running macOS 26.6.1, first at
`master` 901aceb2 with this change and again at 7db4451b, with the same positions:

- `conformance/upgrade/log-directory.sh` printed `ok` in 18 s on its first run in a
  new checkout: the two builds took 7 s and 6 s, each into an empty target directory
  with the crates' sources already downloaded, and the two directories 5 s. A second
  run, with both builds kept, took 6 s.
- N, 620a2e29, wrote 20 records to position 189,568 in each directory and purged
  recordings 0 and 1, which held the feed to position 94,848. N's page begins `# The
  durable log, version 2` and this tree's `# The durable log, version 4`.
- N+1 began at 189,568 on both directories, the stopped one and the killed one, read
  back the 9 records N left, appended 5 in each shape, and read back 19 records from
  94,848 to 284,352.
- The gate fails when a record read back differs: with one digest of N's lines changed
  by hand before N+1 read, step c failed and printed both lists.

The first run found nothing to fix: a directory of version 2 opens under version 4.

## Consequences

Easier: work item 287 changes the format against a gate that already holds the
directories in production; a session that changes a call of the feed learns in its own
pull request that the server pinned to 620a2e29 cannot make the new call.

Harder: `just conformance` builds a second older tree on every run, and CI's cache
keeps neither (ADR 0136, Evidence); the gate grows by one more build for each tree a
server in production runs. A recording that spans two segment files, and a directory a
cluster member wrote, are not in it: the second is `conformance/upgrade/run.sh`'s.

The gate does not hold a downgrade. A directory that N+1 has written to is not
promised to open under N, and nothing here tries it.

Revisit when: a server in production runs a tree other than 620a2e29; a release
exists; or a change touches a segment file's layout or name.

## References

- Work item 362 on the Brigade Board, which carried this decision.
- [ADR 0138](0138-draw-the-finish-line-at-a-beta-that-is-an-alternative-to-aeron-and-build-it-sixth.md),
  condition 9 and features 2 and 3;
  [ADR 0136](0136-run-the-versions-of-one-major-as-one-cluster-and-upgrade-from-the-last-tree-of-the-version-before.md),
  the cluster's half of the gate and how it builds an older tree;
  [ADR 0031](0031-truncate-purge-and-delete-a-stopped-recording.md), the purge.
- `conformance/upgrade/log-directory.sh`; `crates/cavalry/examples/feed-directory.rs`;
  `crates/cavalry/tests/feed/`; `spec/durable-log.md`.
- [What does Brigade's server rely on?](../explanation/what-brigades-server-relies-on.md)
- Brigade's server, `server/crates/brigade-log` of `github.com/appshapes/brigade`, and
  its pin on Cavalry at 620a2e29.
