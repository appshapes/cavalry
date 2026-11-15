- A cluster can be upgraded one member at a time, and one of three voters or more
  without stopping it. The cluster protocol moves to version 1, and `spec/cluster.md` now has the rule it lacked:
  the versions of one major form one cluster, and no version of a major changes a
  message's layout or what an entry does. Version 1 adds no message, field or
  entry. A member and a client of version 1 write 256, their version in semantic
  form, where those of version 0 wrote 0, in canvass position, request vote,
  session connect and session event; every message's header still carries 0, so
  a member of version 0 reads a member of version 1, and the two elect and commit
  as one cluster. `just conformance` now runs `conformance/upgrade/`, the gate of
  invariant 3 that no feature had needed: it builds the last tree of the version
  before beside the tree under test, upgrades a cluster of three on the loopback
  one member at a time, with a ballot that crosses the two versions, and holds
  every acknowledged entry to every member's log
  ([Upgrade a cluster one member at a time](docs/how-to/upgrade-a-cluster.md);
  [ADR 0136](docs/decisions/0136-run-the-versions-of-one-major-as-one-cluster-and-upgrade-from-the-last-tree-of-the-version-before.md);
  work item 231).
