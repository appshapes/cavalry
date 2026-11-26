- A driver whose directory has no room for a log refuses the log on a Linux kernel
  without the populate advice too. On a kernel before 5.14, the ones under RHEL 8,
  Ubuntu 20.04, Debian 11 and Amazon Linux 2, such a driver was killed by a bus error
  at its first touch of the log's pages, and the file stayed. The driver now asks the
  filesystem for a new file's room before it maps it. On a tmpfs, which the default
  directory is on wherever `/dev/shm` exists, a refused ask is also cheap again: 0.13
  to 0.41 ms in a container, where it took 20 to 55 ms, since the filesystem is no
  longer filled and emptied by it. The refusal's message says "could not give the
  file its room", and gives the file's length and the free room where a tmpfs has
  less than the file. On macOS a full volume is still not refused
  ([ADR 0175](docs/decisions/0175-give-a-new-shared-file-its-room-before-it-is-mapped.md)).
