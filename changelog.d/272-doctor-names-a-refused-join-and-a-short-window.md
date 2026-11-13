- `cavalry doctor` now says when a term is too short for its stream. A `cavalry:udp`
  publication that is alone with its recording under `ssc=true` and has refused a
  receiver's join, because the publisher wrote more than a term before the receiver
  answered, gets the warning `udp-publication-join-refused`, read from the
  publication's `sender.joins_refused`, where it got the note
  `udp-publication-recorded-alone`. `recording-stalled` says when the room it reads
  is a short window, 4 MiB or less. Both name `term-length` as the remedy
  ([Record a stream](docs/how-to/record-a-stream.md); ADR 0099; card 272).
