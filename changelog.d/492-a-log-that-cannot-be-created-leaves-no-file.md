- A log that the driver cannot create now leaves no file in its directory. Where the
  directory's filesystem had no room for a log, each refused add left a file as long
  as the log, under a new name each time, and the first kept whatever room was left
  until the driver's next start: 249 files after 25 seconds of an image asked for ten
  times a second, in a container whose `/dev/shm` is 64 MiB. The driver now removes
  the file before it answers with error code 12, so the room is the filesystem's
  again and a caller may ask as often as it likes
  ([ADR 0171](docs/decisions/0171-remove-the-file-of-a-log-that-could-not-be-created.md)).
