- `cavalry doctor` now tells a publication that is connected by its recording alone
  from one that nobody answers. A `cavalry:udp` publication whose channel names
  `ssc=true` and whose stream has a live recording gets a note,
  `udp-publication-recorded-alone`, with the bytes its sender passed without sending
  them, where it got the `udp-publication-not-connected` warning. That warning now
  names the key when the stream is recorded and the channel does not name it, and
  says that a recording would connect the publication when the channel names it and
  nothing records the stream
  ([Record a stream](docs/how-to/record-a-stream.md); ADR 0098; card 271).
