- `cavalry doctor`'s `cluster-joining` note now names what the member's election
  still waits for in the state it is in: its own replay first in `follower replay`,
  which fails the election where it leaves the member's log behind the leader's;
  and in `follower ready`, where the image of the leader's log has arrived, the
  append position alone and the way to the leader's consensus endpoint. Before, it
  said in every state that an image must still arrive. The cluster how-to says, as
  `doctor` does, to run `doctor` on each member's host when the commit position
  stays behind
  ([run a cluster](docs/how-to/run-a-cluster.md#when-it-does-not-work); card 258).
