- A client no longer closes as `corrupt` when an image comes and goes before its
  conductor has read the image's announcement: a publication added and removed with
  nothing published beside a subscriber, a replay that fails at its first frame, a
  committed log closed straight after its open, or a subscription removed with no
  poll. The driver now keeps the log and the position counter an `available image`
  names for the counter reuse timeout, 1 s, unless the subscription is removed first.
  A client that still finds one gone reports it on that subscription, as
  `Fault::Image` with the reason `gone` and no position (C ABI: fault 120 with
  `CAVALRY_NULL_POSITION`), and stays open. `cavalry replay --from` at a position
  inside a frame now prints `the replay failed at position N`
  ([replay a recording](docs/how-to/replay-a-recording.md#when-it-does-not-work);
  [ADR 0078](docs/decisions/0078-keep-an-announced-images-log-for-the-counter-reuse-timeout.md);
  card 241).
