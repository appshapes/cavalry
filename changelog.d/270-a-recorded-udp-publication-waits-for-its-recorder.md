- A recorded `cavalry:udp` publication now waits for its recorder, as a recorded
  `cavalry:ipc` publication does. The publisher's limit is the publication window past
  the lesser of the sender position and the recording's position, and the log is
  cleaned no further than the recording's position. Before, the driver cleaned the log
  a term behind the sender position wherever the recorder stood, so a recorder more
  than a term behind, on a disk that stalled, lost bytes that receivers were given.
  A publisher on such a stream now reads `back pressured` until the recorder catches
  up
  ([ADR 0097](docs/decisions/0097-count-the-recorder-among-a-cavalry-udp-publications-readers.md);
  card 270).
