- `spec/udp.md` is at version 3 and `spec/client-protocol.md` at version 9: a
  `cavalry:udp` publication's channel may name `ssc=true`, by which a live recording of
  the publication connects it while no receiver answers, so that a publisher appends
  to a recorded stream that nobody receives yet. In Aeron, this is the channel
  parameter `ssc`, spies simulate connection. The channel parser reads the key; a
  subscription's, a destination's and a replay's channel refuse it with code 1. Until
  the driver's change lands, `ssc=true` on a publication's channel is answered with
  code 11 and a message that says so
  ([ADR 0098](docs/decisions/0098-let-a-recording-connect-a-cavalry-udp-publication-whose-channel-names-ssc.md);
  card 271).
