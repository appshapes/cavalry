- `just bench` no longer records a message row's samples in a histogram inside the
  measured loop. Each thread counts its samples under 256 ns in a kilobyte of counts
  and writes the later ones in order into a buffer made at setup, and the histogram is
  made of both after the loop, to the same nanosecond. A late delivery used to be
  recorded in a part of the histogram that was cold in the subscriber's cache, and
  the miss made the next delivery later: on one bench host that was most of the
  one-way rows' tail, and about one run in two hundred read two or three times its
  row's p99.99 with nothing wrong on the host. `--messages` is now at most
  100,000,000, since a row has room for 16 bytes a message, 24 in the link rows,
  until its loop is over
  ([ADR 0090](docs/decisions/0090-keep-a-message-rows-samples-out-of-the-histogram-until-its-loop-is-over.md); card 198).
