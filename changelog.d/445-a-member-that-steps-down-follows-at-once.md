- A cluster member that led and steps down now follows the next leader at once. Its
  log subscription binds its own `log` endpoint, which its closed log publication held
  through a 5 s linger: before, the subscription was refused, and the member's duty
  cycles failed until the linger ended. The driver now ends a closed control-channel
  publication's linger when a subscription on the same driver needs its control
  address, counted in `subscription.lingers_ended`, and holds the subscription until
  the socket is gone
  ([ADR 0153](docs/decisions/0153-end-a-closed-control-publications-linger-for-a-subscription-on-its-control-address.md)).
