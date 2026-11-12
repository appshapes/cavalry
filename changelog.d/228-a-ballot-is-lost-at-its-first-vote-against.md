- A cluster member that is a candidate goes back to canvass as soon as it cannot win
  its ballot: when it reads a vote against its candidate term, or holds no vote of
  its own for that term. It waited out the election timeout, 1 s, before. A split
  vote, two members that nominate themselves within a few milliseconds of each
  other, now costs a second nomination delay of at most 500 ms: in memory the
  same election closes 132 ms after the cluster's start where it took 1,129 ms.
  `cavalry_proto::cluster::ballot` says whether a vote against has lost a ballot,
  and `cluster.ballots_lost` counts as before
  ([ADR 0105](docs/decisions/0105-lose-a-ballot-at-its-first-vote-against.md)).
