# 0156. Hold in the upgrade gate every directory a server holds: each tree's alone, and the trees in turn

- **Status:** Accepted
- **Date:** 2026-11-25
- **Invariants affected:** 1, 3, 6
- **Supersedes:** ADR 0141 on one point, its rule for when a tree leaves the gate:
  "removed when no server holds a directory that only that tree wrote". This
  decision's rule, under Decision, takes its place. The rest of ADR 0141 stands, its
  rule for when a tree comes among it. ADR 0141's Revisit clause fired on 2026-11-24,
  "a server in production runs a tree other than 620a2e29", and this decision also
  says how the gate holds more than one tree.

## Context

`conformance/upgrade/log-directory.sh` builds one older tree, N, has it write a log
directory as Brigade's server writes its own, and opens that directory in place under
the tree under test (ADR 0141). N was 620a2e29, the pin of Brigade's server 0.9.0. Its
directory was written in server 0.9.0's shape alone.

On 2026-11-24 both servers in production moved to server 0.10.0, whose pin is d265af82.
Each stopped cleanly on 0.9.0 and started 0.10.0 on the same directory, in place. One
of them has since stopped and started again on what 0.10.0 itself wrote (work item 387,
its comments of that day). So the directories that servers hold now are these:

- Each server's own: written by 620a2e29 in server 0.9.0's shape, terms of 16 MiB,
  then by d265af82 in server 0.10.0's shape, `ssc=true` with `term-length=1m`. The
  catalog was created by version 2 of the durable log's format and written on by
  version 5.
- A copy of it that each server took under 0.9.0, as its way back: 620a2e29's alone.
- What a server first installed at 0.10.0 would hold: d265af82's alone.

The gate held only the second. A run by hand at 4d604048 with
`CAVALRY_DIRECTORY_FROM=d265af82` passed, but its step a writes server 0.9.0's shape
whatever N is. So that run had d265af82 write terms of 16 MiB, which it never writes in
production, and no directory in it was written by two trees in turn.

## Decision

**The gate builds every tree that the servers in production have run.** Today those
are 620a2e29 and d265af82, two constants of the script. A tree comes when a server in
production runs it, as ADR 0141 says. When a tree leaves is this decision's rule, and
it replaces ADR 0141's: a tree stays while a server holds a directory that the tree
wrote any part of, alone or before another tree. ADR 0141 removed a tree once no
server held a directory that only that tree wrote. Under that rule 620a2e29 would
leave once the copies taken under 0.9.0 are gone, while each server's own directory
still holds what 620a2e29 wrote. Under this rule 620a2e29 stays while a server's own
directory, or a copy taken under 0.9.0, exists.

**The gate writes every directory that a server holds or may hold.** That is each
tree's directory alone, in its server's shape, and one directory written by the trees
in turn, in the order the servers ran them, each in its own shape. Today that makes
three:

- 620a2e29 alone;
- 620a2e29 then d265af82 in place;
- d265af82 alone.

Each is written with its last run stopped, and again killed. The directory written in
turn is written a third time with the first tree's last run killed, so that the second
tree recovers a recording the first left live: a server that went back to its copy
under 0.9.0, crashed, and then moved to 0.10.0 again would do that. A tree that
opens a directory another wrote begins where that one ended. It reads back every
record left, appends, and releases the oldest recording, so that its writes and purges
reach across the two trees, as a server's do.

**The checks follow what was written, not fixed numbers.** The script keeps each run's
recording and each roll, and checks the recordings that each release purged against
the feed's own note. Then the tree under test is held to that record:

- `cavalry replay` plays each recording that is left, to where its writer stopped;
- the feed finds the longest term among the recordings left;
- the feed reads back from the oldest recording left, and a read from position 0 is
  refused.

When servers move to a third tree, the chain of trees in turn grows by it, and that
tree's alone is added.

**By hand.** `CAVALRY_DIRECTORY_FROM=<commit>` still names one tree, which writes one
directory alone, in server 0.9.0's shape, or in server 0.10.0's when
`CAVALRY_DIRECTORY_SHAPE=moved-to` is set. It is for a run by hand against a commit
that no server runs, such as this tree's own last commit.

## Alternatives considered

- **Replace 620a2e29 with d265af82 as the one N.** Lost: the servers' directories hold
  620a2e29's writes, a catalog that version 2 created among them, and each server
  keeps a directory that 620a2e29 alone wrote as its way back. A gate on d265af82's
  directory alone holds none of those.
- **The two trees in turn alone.** Lost: the copies taken under 0.9.0, and a server
  first installed at 0.10.0, are directories a server may open under the next tree too.
- **Each tree alone, with no directory written in turn.** Lost: it never has a tree
  open and write on a directory an older tree wrote, which is what every server in
  production did on 2026-11-24. A recovery or a catalog change that only shows when
  version 5 writes on version 2's files would pass.
- **Keep ADR 0141's rule for when a tree leaves:** removed when no server holds a
  directory that only that tree wrote. Lost: it removes 620a2e29 once the copies taken
  under 0.9.0 are gone, while each server's own directory still holds what 620a2e29
  wrote, a catalog that version 2 created among it. Without 620a2e29 the gate cannot
  write the directory in turn, so a change that cannot follow 620a2e29 and then
  d265af82 would pass.
- **Keep step a in server 0.9.0's shape for every tree,** as the run by hand of
  2026-11-24 did. Lost: d265af82 writes terms of 1 MiB under `ssc=true` in production,
  and a gate that never has it write those does not hold what the servers hold.

## Evidence

On the laptop that wrote this ADR, an Apple M5 Pro running macOS 26.6.1, at `master`
4b07404e with this change:

- `conformance/upgrade/log-directory.sh` printed `ok` in 35 s. 620a2e29 built in 6 s
  and d265af82 in 7 s, each into a new target directory, and the six directories took
  the rest.
- In the directory written in turn, 620a2e29 wrote 20 records to position 189,568 and
  purged recordings 0 and 1. d265af82 began there, read back the 9 records left from
  position 94,848, appended 9 to position 243,392 and purged recording 2. This tree
  replayed recordings 3, 4 and 5 to where each writer stopped. It found a longest term
  of 16,777,216 bytes, read back 13 records from position 142,240, and after its own
  10, read back 23. In d265af82's directory alone, it found 1,048,576 bytes.
- The gate fails when this tree cannot read what d265af82 left. With one digest of a
  record d265af82 appended changed by hand before this tree read, step c of the
  directory written in turn failed and printed both lists, which differed in that line
  alone.
- `CAVALRY_DIRECTORY_FROM=d265af82` printed `ok` in 6 s with both builds kept.

What the added build costs on CI's runners, from `ci` run by hand on this change's
branch, runs 45536355928 and 45536715972, beside `master`'s run 45533266831 at
4b07404e:

| Runner | 620a2e29's build | d265af82's build | The whole gate | The gate on `master` |
| --- | --- | --- | --- | --- |
| Linux, `gates (linux)` | 6 s | 9 s and 10 s | 29 s and 30 s | 12 s |
| macOS, `gates (macos)` | 5 s and 6 s | 8 s and 9 s | 30 s and 32 s | 10 s |

So `just conformance` takes about 18 to 20 s more on each runner. Half of that is
d265af82's build, which the cache never holds, as it never holds 620a2e29's. The
other half is the added directories. The jobs of the first run took 14.5 minutes on
Linux and 15.5 on macOS, as `master`'s do, well inside their 30-minute bound. The
second run's macOS job lost its runner partway through `just conformance`; run again,
its gate passed. A run by hand checks `lint-docs` over the last commit alone, so both
second runs failed there on this ADR, which the branch's first commit had added.
Over the pull request's range, which CI checks, the ADR is new.

## Consequences

- Every pull request holds the three directories the servers hold or may hold. A
  change to the durable log's format that d265af82 cannot be followed by, or that
  cannot follow 620a2e29 and then d265af82, fails there.
- `just conformance` builds one more older tree, and the gate writes four more
  directories.
- The feed's files must compile against each older tree. When they cannot, ADR 0141's
  way applies to that tree: keep the files as they last compiled against it, under
  `conformance/upgrade/feed-at-<its first 8 hex digits>/`.
- Revisit when a server in production runs a third tree, or when no server holds a
  directory that 620a2e29 wrote any part of.

## References

- Work item 441 on the Brigade Board. It carries ADR 0141's Revisit; work item 387
  recorded the servers' move.
- ADR 0141; `conformance/upgrade/log-directory.sh`; `conformance/README.md`, "The log
  directory's upgrade gate";
  [What does Brigade's server rely on?](../explanation/what-brigades-server-relies-on.md).
