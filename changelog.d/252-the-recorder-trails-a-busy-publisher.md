- A recorded stream's publisher no longer pays for its recorder on every message. The
  recorder reads at the position of a publisher that does not pause once in 50 µs,
  where it read there every duty cycle, and writes what is committed before a
  `stop recording` or a `sync recording` it is asked for. `Recording::position()`
  trails such a publisher by about 50 µs of its messages, and by what the recorder's
  idle strategy parks for. See
  [ADR 0084](docs/decisions/0084-let-the-recorder-trail-a-publisher-it-is-keeping-up-with.md)
  for the measurement, and
  [Record a stream](docs/how-to/record-a-stream.md) for what to wait for before a sync.
