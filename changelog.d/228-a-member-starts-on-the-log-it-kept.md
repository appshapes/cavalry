- A cluster member made with `cavalry_raft::Consensus::with_election` starts on the
  log directory it kept: in its first init it reads the candidate term it stored
  and the highest term of its record of terms, so it votes once in a term across a
  restart and canvasses with the log it holds. `Io` has two more operations,
  `candidate_term` and `log_leadership_term_id`. A new leadership term's `is
  startup` is 1 only in a term its leader leads from its first election since it
  started. `memory::Cluster::restart` starts a member again on what its recordings
  had synced. A member started with `cavalry run` is unchanged: its leader is
  appointed, and it still refuses a log directory that holds a term
  ([ADR 0089](docs/decisions/0089-start-a-member-that-elects-on-the-log-directory-it-kept.md)).
