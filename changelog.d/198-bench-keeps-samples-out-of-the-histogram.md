- `just bench` no longer records a delivery's latency in a histogram inside a message
  row's measured loop. The subscriber counts its samples under 256 ns in a kilobyte of
  counts and writes the later ones in order into a buffer made at setup, and the
  one-way histogram is made of both after the loop, to the same nanosecond. A late
  delivery used to be recorded in a part of the histogram that was cold in the
  subscriber's cache, and the miss made the next delivery later: on one bench host
  that was most of the one-way rows' tail, and about one run in two hundred read two
  or three times its row's p99.99 with nothing wrong on the host. The publisher's
  samples are recorded as before. `--messages` is now at most 100,000,000, since a
  subscriber has room for 8 bytes a message, 16 in the link rows, until its loop is
  over
  ([ADR 0090](docs/decisions/0090-keep-a-subscribers-samples-out-of-the-histogram-until-its-loop-is-over.md); card 198).
