- `cavalry doctor`'s `recording-stalled` now reads a publisher held by its recorder
  from the publication's own limit: it warns when the publisher has used at least
  five sevenths of the bytes its limit gives it past the recorder, which every held
  publisher has. It compared every recorder's lag with the `cavalry:ipc` window,
  32 MiB, and asked for a whole window, so it could not fire for a `cavalry:udp`
  publication, which the driver holds 8 MiB past its recorder, and it rarely fired
  over `cavalry:ipc` either. The message gives both numbers. A recording `doctor`
  cannot place, of a stream the driver also receives or of one of two publications
  with the same session id and stream id, is listed and not judged
  ([ADR 0101](docs/decisions/0101-read-a-publisher-held-by-its-recorder-from-its-limit-in-doctor.md);
  card 270).
