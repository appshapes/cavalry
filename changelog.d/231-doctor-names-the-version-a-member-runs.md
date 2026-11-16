- `cavalry doctor` now names the version of the cluster protocol that a cluster
  member runs, at the end of the member's `note cluster-member` line: `it runs
  version 1 of the cluster protocol, 0.1.0`. While a cluster is upgraded, run
  `doctor` on each member to see which members are of the newer version. A member
  holds its own version in a new gauge, `cluster.protocol_version`, 256 for version
  1, and holds no other member's. A member of a build older than the gauge has
  none: every member of version 0, and one of the first build of version 1. Of
  such a member `doctor` says that `it has no cluster.protocol_version, so it runs
  a build older than that counter`, and an older build's own `doctor` prints no
  such clause at all. `WARN cluster.messages_rejected` now tells you to run every
  member on one major of the cluster protocol, where it said one version: members
  of one major read each other. `conformance/upgrade/` now holds what `doctor`
  names each member to the build that member runs, after every step of the upgrade
  ([Upgrade a cluster one member at a time](docs/how-to/upgrade-a-cluster.md);
  [ADR 0137](docs/decisions/0137-hold-a-members-version-of-the-cluster-protocol-in-a-counter-and-no-peers.md);
  work item 231).
