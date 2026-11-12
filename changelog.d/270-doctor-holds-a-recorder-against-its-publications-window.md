- `cavalry doctor`'s `recording-stalled` now holds a recorder against its publication's
  own window, and warns at half of it. It compared every recorder's lag with the
  `cavalry:ipc` window, 32 MiB, and asked for a whole window, so it could not fire for a
  `cavalry:udp` publication, which the driver holds 8 MiB past its recorder, and it
  rarely fired over `cavalry:ipc` either: the driver raises a limit in steps of an
  eighth of the window and a publisher stops a message short of its limit, so a
  publisher held by its recorder is between five eighths of a window and a whole
  window ahead. The message names the window
  ([ADR 0097](docs/decisions/0097-count-the-recorder-among-a-cavalry-udp-publications-readers.md);
  card 270).
