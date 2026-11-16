# 0137. Hold a member's version of the cluster protocol in a counter, and no peer's

- **Status:** Accepted
- **Date:** 2026-11-16
- **Invariants affected:** 3, 4, 6
- **Supersedes:** none. Decides the second change of work item 231, which ADR 0136's
  Consequences left to it: `cavalry doctor` naming the versions in a cluster. It
  narrows what those Consequences forecast for this change, "the counters and the
  finding": it adds one counter and a clause of the member's note, and no finding.
  Changes `spec/cluster.md`: one row of "Counters", which `counters.toml` gains as
  id 179, and a sentence of "Versions and the upgrade". The page's version stays
  1: a counter's row moves no version, as ADR 0132 and ADR 0134 added rows without
  moving it. No message's layout changes, no file's format and no function of the
  C ABI.

## Context

After ADR 0136 a cluster runs members of more than one version of `spec/cluster.md`
while it is upgraded, and the versions of one major form one cluster. Nothing
showed an operator which member was of which version. No counter held a member's
version (ADR 0136, Consequences). `cavalry --version` prints the crate's version,
0.1.0, for every build. `docs/how-to/run-a-cluster.md`, under `WARN
cluster.messages_rejected`, and that counter's `doctor` sentence both sent an
operator to "the same version", the first by way of `cavalry --version`. Work item
231 asks for "doctor naming the versions in a cluster".

Invariant 4 says that `cavalry doctor` explains a cluster from counters alone. So
the question is which versions a member's counters hold: its own, or its peers'
too.

A member hears little of a peer's version. Two consensus messages carry one,
canvass position and request vote (`spec/cluster.md`, "Versions and the upgrade").
A member sends them while it has no leader: in canvass, nominate and candidate
ballot ("Roles and the election"). A leader whose election has closed sends
neither. The reader checks the major and drops the rest
(`crates/cavalry-raft/src/consensus.rs`).

## Decision

Each member holds its own version of `spec/cluster.md`, in semantic form, in a
gauge, `cluster.protocol_version`, id 179: 256 for version 1, 0.1.0. The member
stores it once, when it adds its counters, and holds no peer's version. `cavalry
doctor` names it as the last clause of the member's `note cluster-member` line, `it
runs version 1 of the cluster protocol, 0.1.0`. A member whose counters hold no
such gauge runs a build older than the counter, which is every build of version 0
and the first build of version 1, and `doctor` says that of it and no more: `it has
no cluster.protocol_version, so it runs a build older than that counter: version 0
of the cluster protocol, or the first build of version 1`. The versions of a
cluster are read by running `doctor` on each member, as its leader and its voters
already are. The clause is a part of the note and never a warning. The upgrade
gate, `conformance/upgrade/`, asks the newer build's `doctor` of every running
member after every step, and holds its answer to the build that the member runs:
to the version of that build's page when the build's `counters.toml` has the
gauge, and to the clause of a member with none when it has not.

## Alternatives considered

- **A gauge for each peer, of the version last heard from it.** One `doctor` would
  then name every member. Lost. A member hears a peer's version only in canvass
  position and request vote, which a member sends while it has no leader. A member
  that started after its leader never hears the leader's version: the leader sends
  it new leadership term, which carries none. `doctor` would say "not heard" of
  the member an operator most wants named, for up to 7 more counters a member, of
  the 256 a driver gives its clients.
- **`cavalry --version`.** Lost. It is the crate's version, the same for every
  build until a release, and it says nothing of a member that runs: the binary on
  a host's disk may not be the one its member was started from.
- **A `WARN` when a member's version differs from the version of `doctor`'s own
  build.** Lost. `doctor` of one build reading a member of another is the ordinary
  state of an upgrade, not a fault, and a warning on every step would teach an
  operator to pass over warnings.
- **Carry the version in new leadership term or in commit position**, so that every
  member knows its leader's. Lost. It changes a layout, which ADR 0136 makes a
  major, and two majors form no cluster.

## Evidence

- `crates/cavalry-raft/tests/cluster.rs`,
  `three_members_replicate_a_hundred_thousand_entries_and_their_recordings_are_equal`:
  each of three members over three drivers holds 256 in `cluster.protocol_version`
  before its first duty cycle.
- `crates/cavalry-control/src/doctor.rs`,
  `a_members_note_ends_with_the_version_of_the_cluster_protocol_it_runs`: a member
  whose gauge holds 256 is named `it runs version 1 of the cluster protocol,
  0.1.0`; of one with no such gauge the note says `it has no
  cluster.protocol_version, so it runs a build older than that counter: version 0
  of the cluster protocol, or the first build of version 1`; and one at 1795, a
  later minor, is named `it runs version 7 of the cluster protocol, 0.7.3`. A
  gauge that reads 0, as it does between a member's add and its store, is said as
  `its cluster.protocol_version is not stored yet, as for a moment while a member
  starts`, and never as a version 0, which no build stores. None of the four has
  a finding besides its note. Six tests of the note that were
  written before this decision hold the clause at the note's end, after the
  voters, and `a_member_count_with_a_doctor_sentence_is_reported_with_it` holds
  `cluster.messages_rejected`'s new sentence.
- `crates/cavalry/tests/cli.rs`: three members that `cavalry run` started on the
  loopback. `cavalry doctor` on the leader prints a line that holds `; its members
  string gives a vote to members 0, 1 and 2; it runs version 1 of the cluster
  protocol, 0.1.0; see `.
- `conformance/upgrade/run.sh` on a laptop, an Apple M5 Pro with macOS 26.6.1, from
  N, b82fb555, whose page is version 0, to this tree. It read N's `counters.toml`
  in N's exported tree and printed: "N's counters.toml, in
  target/upgrade/b82fb555/src, has no cluster.protocol_version, so a member at N
  stores no version", and then "doctor of N+1 must say of a member at N+1 \`it runs
  version 1 of the cluster protocol, 0.1.0\` and of a member at N \`it has no
  cluster.protocol_version, so it runs a build older than that counter: version 0
  of the cluster protocol, or the first build of version 1\`". It held both
  clauses after every start, stop and client run. The five steps' lines of one run
  end, in order: "doctor of N+1 names members 0, 1 and 2 with no
  cluster.protocol_version"; "doctor of N+1 names member 0 at version 1 and
  members 1 and 2 with no cluster.protocol_version"; with the leader stopped,
  "doctor of N+1 names member 0 at version 1 and member 1 with no
  cluster.protocol_version"; "doctor of N+1 names members 0 and 2 at version 1 and
  member 1 with no cluster.protocol_version"; and "doctor of N+1 names members 0,
  1 and 2 at version 1". Two runs in a row passed: the upgrade took 19 s and 18 s,
  and the whole runs 22 s, with N+1 built in 3 s, and 18 s. In both the member at
  N+1 won the ballot that crosses the two versions, in term 1. With the clause for
  a member with no gauge worded another way, four runs before them passed, and in
  three the member at N won that ballot: its own upgrade then held one more
  election, which waits the leader heartbeat timeout, 10 s, and those upgrades
  took 28 s to 34 s.

## Consequences

- An operator reads the versions of a cluster from `doctor` on each member, during
  an upgrade and after it, and no longer from which binary a host holds.
  `docs/how-to/upgrade-a-cluster.md` verifies an upgrade by it.
- One more counter for each member, of the 256 that a driver gives its clients.
- A member of a build older than the counter has no such gauge: every member of
  version 0, and one of the first build of version 1, 31da2412, which is ADR 0136's
  own change. `doctor` says of such a member that its build is older than the
  counter, and cannot say which of the two versions it runs: nothing in its
  counters file tells them apart. No release exists yet, so no released build is
  one of them.
- `doctor` on one host does not name the other members' versions. An operator
  runs it on each member, as for the leader and the voters.
- An older build's `doctor` prints no clause for any member, since it does not know
  the counter. The how-to says to ask the newer build's.
- `cluster.messages_rejected`'s sentence and the how-to now say "one major" where
  they said "the same version": members of one major read each other.
- The gate reads the clause from the newer build's `doctor` alone, so a change that
  rewords it changes the script's two strings in the same change. It decides which
  clause a member at N must have by N's `counters.toml` alone, in N's exported
  tree, and not by the version of N's page, so it holds for the first build of
  version 1 as N too.
- Revisit: when a version adds an entry or a message that a leader may use only
  once every member is of that version, since the leader must then know its
  followers' versions and a counter of its own does not tell it; and at the first
  release, when `cavalry --version` differs between builds.

## References

- Work item 231 on the Brigade Board, "Run a rolling upgrade across the cluster
  protocol's version, under conformance/upgrade".
- `spec/cluster.md`: "Roles and the election" ("Versions and the upgrade"),
  "Counters". `counters.toml`, `cluster.protocol_version` and
  `cluster.messages_rejected`.
- `crates/cavalry-raft/src/module.rs` (`Counters`);
  `crates/cavalry-control/src/doctor.rs` (`protocol_version_text`,
  `member_findings`); `conformance/upgrade/run.sh` (`named_versions`);
  `conformance/README.md`, "The upgrade gate";
  `docs/how-to/upgrade-a-cluster.md`; `docs/how-to/run-a-cluster.md`.
- [ADR 0136](0136-run-the-versions-of-one-major-as-one-cluster-and-upgrade-from-the-last-tree-of-the-version-before.md),
  the versions of one major as one cluster, and what its Consequences left to this
  decision;
  [ADR 0134](0134-mark-a-standby-in-the-members-string-and-change-one-mark-at-a-time.md),
  `cluster.voters`, the model: a gauge a member stores once, which `doctor` names
  in every member's note.
