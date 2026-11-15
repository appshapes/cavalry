# 0133. Keep the newest snapshot of each state in a member's list

- **Status:** Accepted
- **Date:** 2026-11-15
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. Changes one rule of ADR 0128, which entry a full list of
  snapshots leaves out. ADR 0128's file, its 64 entries, its two slots and its
  checks stand. Changes `spec/durable-log.md`: the sentences of "Snapshots" on a
  sixty-fifth entry, and a phrase of "The driver" on a short file. The page's
  version stays 4, and no vector changes: which entry a writer leaves out is no
  part of what a reader checks.

## Context

A member lists its snapshots in a file of 64 entries (ADR 0128). The entries are of
two states: the member's own, service id −1, and its service's, service id 0. A
sixty-fifth entry left out the first of the list, the oldest, whichever state it
was of.

The two states are listed by two events. The member lists its own snapshot when it
has taken one, at every cluster action its leader logs (ADR 0129). It lists its
service's when the service tells it of one (ADR 0131). A service can take none for
a time: it is stopped, or its program gives no state.

Since ADR 0132 a service starts from the snapshot its member names, and a member
says from which position its log is needed, `cluster.log_purge_position`: the lower
of the positions of the two newest snapshots. An operator may purge the log below
it.

Those three together lose a service its start. Both states have a snapshot at one
position, and the log below it is purged, as the counter allows. The service stops.
The member takes 64 snapshots of its own, which is 16 GiB of log at the snapshot
interval, or 64 asks of the leader's own process in any time. The service's entry is
then the oldest and is left out. The member names no snapshot of a service, the
service reads its member's log from position 0, and the log no longer begins there:
its replay is refused, and it does not run. Both reviews of pull request 298 named
the cause: the list can leave out the newest snapshot of a state.

## Decision

A full list leaves out the first entry whose service id a later entry of the new
list has, the new snapshot counted among them. The newest snapshot of every state
the list names therefore stays in it. A list of 64 entries of 64 service ids, given
a snapshot of a sixty-fifth, has no such entry and leaves out its first.

## Alternatives considered

- **Keep the rule, and hold the log for a state that has no snapshot listed.** The
  first review's other answer. Lost. `cluster.log_purge_position` already falls to
  0 when a state has none listed, and that is too late: the log was purged while
  the state had one. To hold the log, the member must remember where the state's
  last snapshot was, and remembering that is keeping the entry.
- **A list for each state, in two files or two halves of 32.** Lost. It is a change
  of the file's format, with its version and vectors, for what one rule of the
  writer gives.
- **More entries.** Lost. Any bound is reached by a state that has stopped, and a
  larger file only moves the day.
- **Refuse a snapshot when the list is full of other states' entries.** Lost. A
  member that cannot list takes no snapshot, its log is then needed from an old
  position for good, and the bound must leave the member going (invariant 3).
- **Count an entry left out.** Both reviews asked for it. Lost. After this rule an
  entry left out has a newer one of its state, so nothing follows from it: no
  start begins elsewhere and no position falls. A count that rises by one at every
  snapshot after the sixty-fourth says nothing an operator acts on. The one case
  with a consequence is 65 service ids, which this version cannot reach: it has
  one.

## Evidence

- `crates/cavalry-log/src/snapshots.rs`, three tests, on an Apple M5 Pro laptop,
  macOS 26.6.1. A service lists one snapshot and stops, and the module lists 100:
  the list holds the service's entry, first, and the module's newest 63 in their
  order, and reads back the same. The service lists one more: its older entry is
  the one left out. Before this change the service's entry was gone at the
  module's sixty-fourth. The module and a service that alternate, the test ADR
  0128 landed with: the list is what it was, the oldest left out each time. And
  64 entries of 64 service ids with a sixty-fifth: the first is left out.
- `cargo test -p cavalry-log`: 35 unit tests pass, with the crate's six other test
  binaries and its doctest.
- `list` still allocates nothing: the new list is built in the vector the file
  holds for it, 64 entries, and the entry to leave out is found before it is
  built. Finding it compares at most 64 × 64 service ids, once for each snapshot,
  beside one synced write of the slot. It was not timed.
- `spec/vectors/durable-log.json` is unchanged, 204 vectors: a list is checked
  entry by entry, whichever entries a writer kept.

## Consequences

- A state keeps its newest snapshot for as long as its member runs this rule, so
  a service that stopped starts from its snapshot, however many snapshots its
  member took meanwhile.
- `cluster.log_purge_position` stays at the snapshot of a state that has stopped.
  It does not rise until that state takes a snapshot again, and the log above it
  is kept. That is the price, and it is visible:
  `cluster.service_snapshot_position` stays behind `cluster.snapshot_position`.
- A list is no longer the newest 64 snapshots. It can hold one old entry of a
  state that stopped and the newest 63 of the other.
- A member of a version before this one writes ADR 0128's rule into the same
  file, and each version reads the other's list: the format is one.
- A service that is taken away for good leaves its last entry in the list, and the
  log is needed from that entry's position from then on. Before this rule the
  entry went at the sixty-fourth snapshot and the position fell to 0, where
  nothing may be purged at all.
- Revisit: when a member has more services than one, with ADR 0128's Revisit on
  more states than entries; and when a service can be taken away from a member,
  for a way to drop its entry.

## References

- Work item 230 on the Brigade Board, "Catch a member up from a peer's recording,
  snapshot the service and follow as a standby"; the two reviews of pull request
  298, the snapshots file's change, each with this as a note for the changes that
  follow.
- `spec/durable-log.md`: "Snapshots", "The driver".
- `crates/cavalry-log/src/snapshots.rs` (`SnapshotsFile::list`).
- [ADR 0128](0128-list-a-members-snapshots-in-a-file-of-their-own-in-its-log-directory.md),
  the file;
  [ADR 0129](0129-snapshot-a-members-sessions-when-its-read-of-its-log-reaches-the-leaders-action.md)
  and
  [ADR 0131](0131-have-a-service-write-its-own-snapshot-and-its-member-list-it.md),
  the two listings;
  [ADR 0132](0132-start-a-service-from-the-snapshot-its-member-names-when-its-program-asks.md),
  a service's start and the position a log is needed from.
