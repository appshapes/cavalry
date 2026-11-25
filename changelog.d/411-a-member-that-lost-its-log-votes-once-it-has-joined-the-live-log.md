- A cluster member that came back with its log empty now withholds its vote until it
  has joined its leader's live log as well as caught up past its vote floor. This closes the case ADR 0145 left open: a new leadership term the
  leader wrote before an entry the member had acknowledged, held back by flow control
  and read after the member lost its log, gave a vote floor below that entry, and an
  election while the member caught up could lose it. The member clears its floor once
  it has both, counted in the new `cluster.vote_floors_cleared`, and a new
  leadership term at position 0 now gives a floor too. `cavalry doctor` names a member
  that holds a floor at or past its position and has not joined since it started
  ([ADR 0161](docs/decisions/0161-hold-a-members-vote-floor-until-it-has-joined-the-live-log-and-then-clear-it.md);
  work item 411).
