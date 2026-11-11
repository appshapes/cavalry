- Fixed, for a cluster member made with `cavalry_raft::Consensus::with_election`: a
  leader named its own term as the next term of every member whose log was of an
  older term, where its record of terms held a term between the two. A member that
  had missed that term, and held in its place an entry no quorum had recorded that
  ended at the leader's term base, joined the leader with bytes that were never
  committed. A leader now names the lowest term its record holds above the member's,
  with that term's base and end, as `spec/cluster.md` gives the next fields of new
  leadership term, through a new method of `cavalry_raft::Io`, `terms_after`. A
  member started with `cavalry run` was not affected: its leader is appointed, and
  an appointed leader's record holds its own term alone.
