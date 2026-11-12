- A `cavalry:udp` publication's channel may name `term-length=<length>`, the term
  length of its log: a power of two from 65536 to 67108864, written in bytes or with
  `k`, `m` or `g`, as `term-length=1m`. A log is three terms and resident from its
  creation, so a stream that names `256k` holds under 1 MiB where the default holds
  48 MiB: three servers that each record one feed and follow the two others held
  11.8 MiB of driver directory each with the key and 248 MiB without it. The longest
  message is the term ÷ 8 and the publication window half the term, so name a term
  that holds what the publisher writes in 16 milliseconds at its fastest. A channel
  that names none has the 16 MiB term, as before. In Aeron, this is the `term-length`
  channel parameter. A publication on `ssc=true` has a new counter,
  `sender.joins_refused`: the receivers that could not join it while it was alone with
  its recording because the publisher wrote more than a term before they answered,
  which a longer term ends. The refusal of a message that is too long names the key.
  `spec/udp.md` is at version 4 and `spec/client-protocol.md` at version 10; a
  subscription's, a destination's and a replay's channel refuse the key with code 1,
  and a join under another term length is refused
  ([ADR 0099](docs/decisions/0099-let-a-cavalry-udp-publications-channel-name-its-term-length.md);
  card 272).
