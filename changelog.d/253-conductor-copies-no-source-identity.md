- A client's conductor no longer allocates for an image that is another client's.
  The driver announces every image to every client of its directory, and the
  conductor made a `String` of each one's source identity, `cavalry:ipc` for an image
  on the host, before it looked for the subscription: 11 bytes allocated and freed
  for every image a neighbour gained. It now copies nothing from an `available image`
  whose subscription is not its own. That allocation is what failed `cavalry-raft`'s
  allocation gate now and then in CI, when a member's client read its service's
  image inside the gate's window (card 253).
