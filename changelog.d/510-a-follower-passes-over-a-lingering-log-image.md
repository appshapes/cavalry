- A cluster follower that joins its leader's log while a publication the leader gave
  up with an earlier term still lingers at the follower's `log` endpoint now records
  the leader's log and closes its election. Before, it took the old publication's
  image, waited the leader heartbeat timeout, 10 s, for the leader's, and then failed
  its election ([spec/cluster.md](spec/cluster.md), state 15).
