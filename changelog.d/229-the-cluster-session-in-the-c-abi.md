- A program in C holds a session with a cluster. The C ABI is at version 4:
  `cavalry_connect_cluster` opens the session, `cavalry_cluster_session_offer` sends
  the leader a message with a publication's result codes, and
  `cavalry_cluster_session_poll` hands the fragment handler each reply and is the
  session's duty cycle. The handler reads a reply's leadership term and the
  cluster's time with `cavalry_cluster_reply_values`. A header now says what it
  carries: `cavalry_header_values` refuses a cluster session's header, and
  `cavalry_cluster_reply_values` a subscription's or a committed log's, each with
  the new code `CAVALRY_ERR_WRONG_HEADER`. The connect's own refusals are the codes
  140 to 143, `CAVALRY_ERR_CLUSTER_*`. A package generated for version 3 refuses
  the library until it is regenerated; the five clients bind the new functions, and
  each gains its session object in the change after this one. A cluster's service
  comes with version 5
  ([ADR 0122](docs/decisions/0122-take-the-c-abi-to-version-4-with-the-cluster-session-through-the-one-fragment-handler.md);
  work item 229).
