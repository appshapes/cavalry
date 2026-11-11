- Of two drivers started on one directory at nearly the same moment, exactly one now
  runs and the other is refused with `driver-active`, naming the first's pid. Before,
  a second `cavalry run` within about 15 ms of the first could remove the first's
  half-made directory, fail with an I/O error that named no driver, or leave a
  driver that no client could use. A driver now holds its directory by a lock of the
  operating system on the directory until it stops, so a driver whose process is
  stopped keeps its files, and a driver that was killed no longer holds up the next
  start for ten seconds
  ([run the driver](docs/how-to/run-the-driver.md#when-it-does-not-work);
  [ADR 0079](docs/decisions/0079-hold-the-driver-directory-by-a-lock-for-the-drivers-life.md);
  card 250).
