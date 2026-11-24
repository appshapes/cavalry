- A cluster member that followed and wins an election now leads at once. Its log
  publication binds its own `log` endpoint as its control address, which its follower
  subscription's endpoint held until the receiver closed it. Before, an election that
  closed while the receiver was still closing that endpoint failed the new leader's
  duty cycle. The driver now holds an `add publication` whose control address a
  receive endpoint still holds while that endpoint closes, counted in
  `publication.add_deferred`, and refuses one whose address a live subscription holds
  with a message that names it
  ([ADR 0154](docs/decisions/0154-hold-an-add-publication-while-a-receive-endpoint-that-holds-its-control-address-closes.md)).
