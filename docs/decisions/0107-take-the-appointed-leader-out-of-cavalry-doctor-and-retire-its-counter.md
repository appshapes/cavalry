# 0107. Take the appointed leader out of cavalry doctor, and retire its counter

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 3, 4, 6
- **Supersedes:** none. Does what ADR 0104 left to the change that follows it.
  Changes `spec/cluster.md`: the row of `cluster.appointed_leader_behind` leaves
  "Counters". No message's layout changes, and the page stays at version 0.

## Context

ADR 0104 had the consensus module elect its leader, and took the appointed leader
out of the data plane. It left four things for one change more, because
`cavalry-control` is the control plane and a pull request under a data-plane crate
does not touch it:

- `cavalry doctor`'s check `cluster-leader-behind`, which read the counter
  `cluster.appointed_leader_behind`.
- The texts of `doctor` that name an appointed leader.
- The last sentence of the `cluster.member.id` key's doc, which called the member
  with the lowest id the appointed leader. `just gen-config` writes that doc into
  `cavalry-control` and into the configuration reference.
- The counter, id 113, registered in `counters.toml` and added by no member.

**`doctor` had two sets of findings for a member.** A member whose counters held
`cluster.leader_member_id` and no `cluster.appointed_leader_behind` was read as a
member that elects (ADR 0100, second change; ADR 0102). Every other member was
read as one whose leader is appointed, or as one of a version before the two
gauges. That second set had two checks of its own, `cluster-leader-behind` and
`cluster-joined-late`, and its own texts for `cluster-no-leader` and
`cluster-joining`. The texts named the member with the lowest id as the leader.
They said that this version has no catch-up, and they told the operator to start
the cluster again as a new cluster.

**None of that is true of a member of this version.** A member started with
`cavalry run` elects (ADR 0104). It adds the two gauges when it starts and never
adds `cluster.appointed_leader_behind`. It catches up within its leader's term
(ADR 0088), and it begins its election again after one that fails. So only a
counters file that no member of this version writes reached the second set.

**No earlier version exists to write one.** Checked on this change's branch:

- `git tag --list` prints nothing. The repository has no tag.
- `Cargo.toml` has `version = "0.1.0"` under `[workspace.package]`, and all ten
  members of the workspace, the nine crates under `crates/` and `bench`, have
  `publish = false`. ADR 0003 decided both: one version, and no publishing until
  the first release.
- `CHANGELOG.md` has one section, "Unreleased".

## Decision

**The check, the texts, the sentence and the counter leave together, in this
change.**

**`cavalry doctor` has one set of findings for a cluster member**, the set a
member that elects had: `cluster-member`, `cluster-no-leader`, `cluster-joining`,
`cluster-quorum-lost`, `cluster-commit-waits`, and the three counts that have a
doctor sentence of their own. No word of what they say of a member that elects
changes.

**`cluster-leader-behind` leaves**, with the member record's two fields for the
counter and the function that found the member with the lowest id.

**`cluster-joined-late` leaves with it.** It was a check of the second set alone:
a member that elects never had it (ADR 0100). Its text told the operator to start
the cluster again. That is untrue of a member that begins its election again and
catches up. The elections a member has failed are named at the end of
`cluster-no-leader` and `cluster-joining`, with the counter of each rule.

**A member whose counters hold no gauge is read as every other member.** Its note
leaves out who it follows and why its election began, and `cluster-joining` says
`its leader` where it would name a member by its id. A member's consensus module
adds its counters one after another when it starts, so `doctor` can read a member
between two of them.

**The `cluster.member.id` key's doc loses its last sentence.** The id says which
member a host is. It says nothing of who leads.

**Counter 113 leaves `counters.toml`, and id 113 is never used again.** A comment
stands where the entry stood and says so. `spec/counters-file.md` already has the
rule: an entry has an id that is never reused. The counter's row leaves
`spec/cluster.md`, "Counters". The generated reference page and the constants in
`cavalry-shm` follow from `just gen-counters`.

**The glossary keeps a short row for "appointed leader"**, since ADRs 0060, 0063,
0081, 0100 and 0104 use the term: what it was, and that no member of this version
is one.

## Alternatives considered

- **Keep the texts for a member of an earlier version.** Lost. No version was
  released, by the three facts above, so no operator runs a member that the texts
  are true of. Kept, they are a second set of rules with tests of their own, about
  a leader no cluster has (invariant 6). Invariant 3 asks that mixed versions run
  during an upgrade. The first release has no version before it, and
  `conformance/upgrade/`, the gate for that invariant, is not in the tree yet. A
  member of another version still shows in `cluster.messages_rejected`, whose
  doctor sentence says to run every member at the same version.
- **Keep `cluster-joined-late` for a member whose elections keep failing.** Lost.
  `cluster-no-leader` and `cluster-joining` already end with how many elections
  failed and on which rule. A third finding would give the same count twice, and
  the remedy in its text is wrong for a member that catches up.
- **Keep the counter registered with no reader.** Lost. A registered client
  counter is one a driver lets a client add, with a row in the counters reference
  and a constant in `cavalry-shm`. Nothing would add this one or read it. The
  reference would list a counter that no counters file holds.
- **Reuse id 113 for the next counter.** Lost. `spec/counters-file.md` says an id
  is never reused. A counters file written by a build before this change may hold
  a record of type 113 with the old meaning, and a reader of a later version would
  read the new counter's meaning into it. An id is an i32, and the registry holds
  140 entries.
- **Name the retired id in a key of `counters.toml` that `just lint-counters`
  checks.** Lost for this change. `scripts/gen-counters.py` accepts `schema` and
  three tables at the top of the file and refuses any other key, so a list of
  retired ids is a change to the registry's schema, with a decision of its own.
  One retired id is held by a comment and a test until then (Consequences).

## Evidence

All on this change's branch, on a laptop, macOS.

- `cargo test -p cavalry-control`: 50 unit tests and 3 doctests passed.
  - `a_member_that_elects_and_has_no_leader_is_told_why_its_election_began` and
    `a_member_that_elects_and_joins_names_the_leader_its_gauge_holds` pass with
    their bodies as they were before this change. What `doctor` says of a member
    that elects is what it said.
  - `every_election_state_of_the_spec_is_named` and
    `a_member_count_with_a_doctor_sentence_is_reported_with_it` pass with their
    assertions as they were. Their members' counters hold no gauge: such a member
    is read as every other.
  - `a_record_of_the_retired_type_113_changes_no_finding`, new. A running driver's
    counters file holds a member in canvass. `doctor` reports it. A record of type
    113 with the value 2 is then added under the member's key, and the report has
    the same count of checks and the same findings, the note of the driver's
    heartbeat left out because it holds the heartbeat's age. Before this change
    the same record made the member `FAIL cluster-leader-behind`. The test also
    fails when an entry of `counters.toml` has id 113.
  - `a_member_in_every_state_of_the_join_is_joining`, new: each of the states 9 to
    16 gives `cluster-joining` with the leader's id from the gauge.
  - `the_members_string_names_a_quorum_and_a_path_for_a_member_it_holds`, new: the
    checks of the members string's ids and endpoints that two removed tests held,
    and a member the string does not hold.
  - `a_leader_whose_election_closed_has_the_note_alone`: the case of an elected
    leader, moved unchanged out of a removed test.
  - `a_cluster_member_is_explained_from_its_counters`: a member's checks count 8
    where they counted 10, and the cases of the two checks that left are gone.
  - Removed: `a_member_whose_leader_is_appointed_keeps_its_texts_with_the_gauges_read`
    and `a_member_with_no_leader_is_told_whether_it_is_the_appointed_leader`.
- `cargo test -p cavalry-shm`: 109 tests and 5 doctests passed, with the table of
  140 entries. `cargo test -p cavalry-proto`: 39 tests and 3 doctests passed.
- `just lint` exits 0. Its clippy step builds every crate and every test of the
  workspace with warnings denied, so no code names the constant that left. Its
  `lint-counters` step prints `140 counters, 9 state machines, 2 generated files
  current, ok` with ids 113 and 139 absent: the gate asks that the ids in the file
  be unique, and nothing of a gap. Id 139 is another change's (ADR 0102).
- `just docs` and `just lint-docs` exit 0.
- Not run for this record: `just test`, `just conformance` and `just replay-check`.
  No file of `cavalry-raft`, `cavalry-driver`, `cavalry-client`, `cavalry-ffi` or
  `cavalry` changes, and before this change `cavalry-control` alone named the
  counter's constant.

## Consequences

Easier. `doctor` has one path for a cluster member, and every text on it is true
of a member started with `cavalry run`. Outside the decisions, the changelog and
the glossary's row, three places still say "appointed", each as history: the
comment in `counters.toml` where the entry stood, a sentence of
`crates/cavalry-raft/README.md` on ADR 0063, and the assertions of two tests that
no text of `doctor` says the word.

**A counters file that holds a record of type 113.** A doctor of this version
reads a member's counters by the type ids `counters.toml` holds. A record of type
113 is of none of them, so no check reads it, whatever its value: the report is
the one the file gives without the record. The record counts toward the fill of
the counters file, as every allocated record does. The member that wrote it is of
a build before ADR 0104, and `doctor` reads it as a member that elects, so
`cluster-no-leader` says of it that the members elect their leader, which that
build does not do. No such build was released.

**A driver of this version refuses `add counter` for type 113**, with error code
11, as it refuses every type `counters.toml` does not give the scope `client`. A
consensus module built before ADR 0104 does not start as a client of such a
driver.

Changed. On a driver that is a cluster member, the `checks run:` count in the
first line of `doctor`'s report is 2 lower: the member's checks are 8 where they
were 10.

To maintain. Id 113 is kept free by a comment in `counters.toml` and by the first
assertion of `a_record_of_the_retired_type_113_changes_no_finding`. `just
lint-counters` would pass an entry that took the id.

Revisit when a second id is retired: the registry then names its retired ids in a
key that `scripts/gen-counters.py` checks, and the comment and the assertion go.

## References

- [ADR 0104](0104-have-the-consensus-module-elect-its-leader-and-take-the-appointed-leader-out.md),
  which left this change to follow it;
  [ADR 0100](0100-move-the-module-to-the-election-in-ordered-changes-around-one-that-takes-the-appointed-path-out.md),
  the order, and `doctor`'s two sets of findings;
  [ADR 0102](0102-publish-who-a-member-follows-and-why-its-last-election-began-as-two-gauges-on-both-paths.md),
  the two gauges;
  [ADR 0088](0088-catch-up-within-a-leaders-term-and-join-the-live-log-from-the-replay.md),
  the catch-up;
  [ADR 0063](0063-run-the-consensus-module-as-a-client-and-lead-by-appointment.md),
  the appointed leader and its counter;
  [ADR 0062](0062-add-a-clients-counter-with-aerons-counter-commands.md), a
  client's counter and the driver's check of its type;
  [ADR 0012](0012-lay-out-the-counters-file.md), `counters.toml`;
  [ADR 0003](0003-lay-out-the-workspace.md), one version and no publishing yet.
- `spec/cluster.md`, "Counters"; `spec/counters-file.md`, "`counters.toml`".
- `crates/cavalry-control/src/doctor.rs`, `cluster_checks` and `member_findings`;
  [Run a cluster of three members](../how-to/run-a-cluster.md#when-it-does-not-work).
- Card 228.
