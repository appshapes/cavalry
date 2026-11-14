- A program in C runs a cluster's service. The C ABI is at version 5:
  `cavalry_cluster_service` makes a service beside the member on the client's
  driver, and `cavalry_cluster_service_poll` hands the fragment handler each
  committed entry and is the service's duty cycle. The handler reads the entry with
  `cavalry_cluster_entry_values`, answers a session with
  `cavalry_cluster_service_reply`, which returns one of six reply codes,
  `CAVALRY_CLUSTER_REPLY_*`, and says whether the entry is taken with
  `cavalry_cluster_service_action` and `CAVALRY_ACTION_ABORT`, `_BREAK` or
  `_CONTINUE`. A service's poll holds the client it was called with: from inside
  the handler a function that takes that client answers `CAVALRY_ERR_POLLING`, a
  refusal that is new for every function that takes a client. The new error codes
  are 144 to 146, `CAVALRY_ERR_CLUSTER_ENTRY`, `_NO_ENTRY` and `_ACTION`. A package
  generated for version 4 refuses the library until it is regenerated; the five
  clients bind the new functions, and each gains its service object in a change
  after this one
  ([ADR 0123](docs/decisions/0123-take-the-c-abi-to-version-5-with-the-cluster-service-answered-from-inside-the-one-fragment-handler.md);
  work item 229).
