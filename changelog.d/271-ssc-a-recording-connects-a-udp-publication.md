- A `cavalry:udp` publication's channel may name `ssc=true`: a live recording of the
  publication then connects it while no receiver answers, so that a publisher appends
  to a recorded stream that nobody receives yet. The sender sends no data meanwhile and
  moves its position with the recorder's; `sender.bytes_unsent`, a gauge of the
  publication's own, holds what went to the recording alone, and never reads below
  zero when a receiver comes back behind what it was sent. A receiver that joins is
  sent the stream from where its own image begins, the sender going back up to a term
  for it, and what came before that is in the recording, for a replay. Without the
  key nothing changes: a publication is connected by a receiver. In Aeron, this is
  the channel parameter `ssc`, spies simulate connection. `spec/udp.md` is at version
  3 and `spec/client-protocol.md` at version 9; a subscription's, a destination's and
  a replay's channel refuse the key with code 1, and a join under another `ssc` is
  refused
  ([ADR 0098](docs/decisions/0098-let-a-recording-connect-a-cavalry-udp-publication-whose-channel-names-ssc.md),
  [ADR 0111](docs/decisions/0111-count-unsent-bytes-above-the-sent-position-and-refuse-a-recording-position-the-sender-cannot-follow.md);
  card 271).
