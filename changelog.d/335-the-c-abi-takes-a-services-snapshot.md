- The C ABI is at version 6: a cluster service written in C takes a snapshot of its
  program's state with `cavalry_cluster_service_snapshot` after a poll that ends at
  the leader's request, now handed over as `CAVALRY_CLUSTER_ENTRY_SNAPSHOT`, and
  starts from its last snapshot with `cavalry_cluster_service_restore` before its
  first poll. The four ways these fail have codes of their own, 147 to 150. A program
  or a generated client's package built against version 5 is refused by a version 6
  library until it is rebuilt or regenerated; the five clients' packages in the
  repository are regenerated
  ([ADR 0146](docs/decisions/0146-take-the-c-abi-to-version-6-with-a-services-snapshot-and-its-start-from-one.md);
  [C ABI](docs/reference/c-abi.md#a-services-snapshot-and-its-start)).
