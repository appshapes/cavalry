- The cluster protocol closes a candidate's ballot on the members it has heard: a
  candidate leads at once when the votes for its term make a quorum and every member
  it has read a consensus message from since its election began has cast one of
  them, so a leader that was killed is not waited for until the election timeout.
  `cavalry_proto::cluster::ballot` takes the members heard. No member takes a vote
  yet: the election is card 228's next changes
  ([ADR 0077](docs/decisions/0077-elect-the-leader-by-vote-and-close-a-ballot-on-the-members-heard.md)).
