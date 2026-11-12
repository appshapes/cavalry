- Fixed: a cluster member made with `cavalry_raft::Consensus::with_election` that
  had recorded a term which fewer than a quorum recorded kept it in its record
  under a leader whose record had none; when that member led, another could be
  asked for a truncation below a commit position it knew, refuse it at every
  election, and never join. A leader now answers a member for the highest term its
  own record holds at or below the one reported, and a member told a term below its
  own drops the terms its leader's record lacks, counted in the new
  `cluster.terms_dropped`. `Io` has two more operations, `term_at_or_below` and
  `drop_terms_above`; `cavalry_proto::cluster` has `answered_term`,
  `cavalry_raft::consensus` has `term_at_or_below`, and
  `cavalry_log::terms::TermsFile` has `drop_above_term`. No
  member started with `cavalry run` elects yet
  ([ADR 0096](docs/decisions/0096-answer-a-member-for-a-term-the-leaders-record-holds-and-drop-the-terms-its-leaders-lacks.md)).
