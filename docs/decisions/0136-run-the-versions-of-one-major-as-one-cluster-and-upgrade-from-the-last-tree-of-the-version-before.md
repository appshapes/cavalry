# 0136. Run the versions of one major as one cluster, and upgrade from the last tree of the version before

- **Status:** Accepted
- **Date:** 2026-11-15
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. Decides ADR 0052's feature 7, work item 231, and what that
  ADR left to it: how the upgrade gate names the version before. Moves
  `spec/cluster.md` to version 1: a paragraph of its opening; a paragraph of "Roles
  and the election", "Versions and the upgrade"; the header's `version` row and the
  sentence of "Encoding" on a version field; the `protocol version` rows of canvass
  position and request vote, and the `version` rows of session connect and session
  event; two rows of "Values"; a sentence of "Vectors"; and eight vectors. No
  message's layout changes, no file's format and no function of the C ABI.

## Context

Invariant 3 says that mixed versions run during an upgrade, and its gate,
`conformance/upgrade/`, did not exist: no feature had needed it (`CLAUDE.md`,
"Gates"). ADR 0052's feature 7 builds it: "the cluster protocol's version in every
message, a member at N+1 joining a cluster at N and the election crossing the
two", with a gate that "builds the checkout at the last commit that moved
`spec/cluster.md`'s version and at `HEAD`".

Three facts of the page at version 0 shape the decision.

- The page's version had never moved, in 33 commits that changed the page. Two
  builds of any two of those commits are one version, so no cluster of them is a
  cluster of two versions, and nothing can name a version that differs.
- Every message's header carries `version`, a u16, and a reader rejects every
  message whose header's version is not 0. A member at the version before would
  drop every message of a member that wrote another value there. ADR 0052 found
  the same of `spec/udp.md`'s frame byte and kept it at 0.
- Two consensus messages carry `protocol version`, an i32 in semantic form: canvass
  position and request vote. A reader rejects a major that is not 0 and does
  nothing with the rest. A session connect and a session event carry a `version`
  whose major the leader and the client check.

So the tree before this decision already reads a later minor of major 0. What the
page lacked is the rule that says so, what a minor may never change, how an
upgrade goes, and a version that differs.

## Decision

The versions of this page that share a major form one cluster. Within a major no
version changes a layout, a template's id, an enumeration's values, a check of
"What a reader rejects", or what an entry does to the state computed from the log;
a version that must is another major, which writes another header version and
forms no cluster with major 0. The page moves to version 1, 0.1.0 in semantic
form, the page's version being the minor while the major is 0. A member and a
client of version 1 write 256 in the four fields that carry a version and 0 in
every header. Version 1 adds no message, field or entry: its content is this rule.

A cluster whose every member runs is upgraded one member at a time on the log
directories it has. A member is stopped with its service and both are started at
the new version, and the next is stopped only once that one leads, or follows with
its election closed. With three voters or more a quorum runs at every step. A
member may be stepped back, since no file's format differs between the two
versions.

The gate, `conformance/upgrade/`, runs that upgrade on the loopback under `just
conformance`. It builds two trees: the tree under test, and N, the last tree of
the version before. N is derived from the page's own history: the first parent of
the newest first-parent commit that changed the page's first line to the one the
tree under test has. Three members start at N and commit; one follower moves to
the new version; the leader is stopped, so that a ballot crosses the two
versions, and is started at the new version; then the last member moves. The gate
holds every acknowledged entry to every member's log, and every member's log to
the same bytes.

## Alternatives considered

- **No move of the version.** The gate would run two builds of one version. Lost.
  "A cluster of two versions elects and commits" would be untrue of it, and
  `doctor` would have no version to name. A rule that nothing exercises is not a
  gate.
- **Move the header's version.** It is the field every message has, which is what
  "the version in every message" reads as. Lost. A member of version 0 rejects
  every message whose header is not 0, so a cluster of the two versions would
  elect nothing: the opposite of the feature, as ADR 0052 found for
  `spec/udp.md`. The header's version is now the layouts' version, and a major's.
- **A major.** Lost for the same reason: two majors form no cluster.
- **Negotiate.** Each member would learn its peers' versions and write the lowest
  common layout. Lost for now. No layout differs between the two versions, so
  there is nothing to negotiate, and a member learns a peer's version only from
  the two messages of an election. The first version that adds an entry or a
  message says when a leader may use it.
- **Name N in a file or a tag.** A file, `conformance/upgrade/FROM`, or a tag would
  hold N's commit. Lost. Each move of the version would need the name moved by
  hand, in a change that cannot name its own squash; a tag is mutable and outside
  the pull request that needs it. The page's history holds the same fact and
  cannot be out of step with the page.
- **N as the commit that moved the version**, ADR 0052's words read literally.
  Lost. On the commit that moves it, that is the tree under test against itself.
  Its first parent is the last tree of the version before, which is what the
  words meant.
- **N as the commit before the tree under test.** Lost. Two versions would meet
  only in the change that moves the version, N would be built again for every
  commit, and no later commit would be held to the version before.
- **A released version as N.** No release exists. ADR 0061's fourth condition
  extends the gate to released versions at the first release (ADR 0003, Revisit).

## Evidence

- `spec/vectors/cluster.json`, 278 vectors, written by `spec/vectors/cluster.py`:
  the four messages that carry a version, each as a writer of version 0 wrote it
  and as a writer of 0.7.3 would. `crates/cavalry-proto/tests/cluster_conformance.rs`
  reads every one with the crate at version 1, and still rejects a header's version
  of 1, a `protocol version` of major 1 and a negative one.
- `crates/cavalry-raft/tests/ballot.rs`,
  `a_member_writes_its_version_and_reads_every_version_of_its_major`: a member
  writes 256 in its canvass position and its request vote; it becomes a candidate
  on canvass positions of version 0; and it votes for a candidate of version 0 and
  for one of 0.7.3. The consensus tests that were written at version 0 feed a
  member canvass positions and request votes of version 0, and pass unchanged
  with the member at version 1.
- The reader's rule was the tree's before the page said it:
  `crates/cavalry-proto/src/cluster.rs` checks a `protocol version`'s major alone,
  and `crates/cavalry-raft/src/consensus.rs` drops the field on receipt. So the
  tree before this decision, which is N, reads version 1.
- `conformance/upgrade/run.sh` on the loopback: the upgrade above, from the tree
  before this decision, b82fb555, to this one, on a laptop, which is macOS. The two
  members that remained when the leader was stopped, one of each version, elected
  the one of version 1 in term 1. The three services reached the client's total,
  124, from the same 18 entries, and the three logs replayed to the same bytes, to
  position 1856. The upgrade took 18 s and the run 21 s, with N's build kept. The
  pull request's body has that run and the run of CI's Linux runner; `master`'s
  own run is the first on CI's macOS runner (ADR 0073).

## Consequences

- A cluster is upgraded without stopping it, and the page says how. A member of
  a later version can lead members of an earlier one, and the reverse.
- Every later change to the page is held to the rule: it adds what a member of an
  earlier minor may ignore, or it is a major. A change that adds an entry or a
  message must say when a leader may use it, which this version leaves to it.
- The header's version no longer follows the page's. It stays 0 until a major.
- The gate builds a second tree. N changes only when the page's version does, so
  its build is kept between runs. `just conformance` takes longer by the upgrade,
  and by that build when it is not kept.
- A member's version is in no counter yet, so `cavalry doctor` cannot name the
  versions of a cluster, and `cavalry --version` says the crate's version, 0.1.0,
  for every build. Work item 231's second change adds the counters and the
  finding; until then the gate knows a member's version by the build it started.
- The gate holds the last tree of the version before, not every tree of it. A
  log directory that "A member's start" refuses, as one of a build before ADR
  0104 or ADR 0108, is refused in an upgrade as at any start.
- `spec/cluster.md` names an app version that a leader announces and a counter,
  `cluster.app_version_mismatches`, that `counters.toml` does not have; the
  module writes 0. That is a service's version and not this page's, and this
  decision leaves it as it found it.
- Revisit: at the first release, for released versions as N (ADR 0061); when a
  version adds an entry or a message, for when a leader may use it; when a major
  is needed, for how a cluster crosses one and what a member of the later major
  does with a log directory that major 0 wrote; when a line that the gate reads
  of both builds is reworded, since N prints the older line until the version
  moves.

## References

- Work item 231 on the Brigade Board, "Run a rolling upgrade across the cluster
  protocol's version, under conformance/upgrade".
- `spec/cluster.md`: the opening, "Encoding", "The consensus messages", "The
  session protocol", "Roles and the election" ("Versions and the upgrade"),
  "Values", "What a reader rejects", "Vectors". `spec/vectors/cluster.py`.
- `crates/cavalry-proto/src/cluster.rs` (`VERSION`, `HEADER_VERSION`,
  `PROTOCOL_VERSION`); `crates/cavalry-raft/tests/ballot.rs`;
  `conformance/upgrade/run.sh`; `conformance/README.md`;
  `docs/how-to/upgrade-a-cluster.md`.
- [ADR 0052](0052-build-raft-on-the-durable-log-fifth.md),
  feature 7 and why a frame's version byte stays 0;
  [ADR 0058](0058-specify-the-cluster-protocol-from-aerons-codecs.md), the
  messages and their header;
  [ADR 0061](0061-draw-the-finish-line-for-early-testers-at-the-first-release.md), the gate over
  released versions;
  [ADR 0003](0003-lay-out-the-workspace.md), its
  Revisit.
