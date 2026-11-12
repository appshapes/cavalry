- A `cavalry:udp` publication's channel may name `ssc=true`: a live recording of the
  publication then connects it while no receiver answers, so that a publisher appends
  to a recorded stream that nobody receives yet. The sender sends no data meanwhile and
  moves its position with the recorder's, counted in `sender.bytes_unsent`; a receiver
  that joins later joins there, and what came before is in the recording, for a
  replay. Without the key nothing changes: a publication is connected by a receiver.
  In Aeron, this is the channel parameter `ssc`, spies simulate connection.
  `spec/udp.md` is at version 3 and `spec/client-protocol.md` at version 9; a
  subscription's, a destination's and a replay's channel refuse the key with code 1,
  and a join under another `ssc` is refused
  ([ADR 0098](docs/decisions/0098-let-a-recording-connect-a-cavalry-udp-publication-whose-channel-names-ssc.md);
  card 271).
