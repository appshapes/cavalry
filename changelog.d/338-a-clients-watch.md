- On macOS a client now has a second thread, its watch: the client's conductor and
  the watch each end a park of the other that outlived its timeout by more than
  200 ms. On one Mac the system loses about one timer wake-up in 40 million, and a
  client whose conductor was held sent no keepalive and was timed out by its driver
  after 10 s. A park so ended is counted in a counter the client adds for itself at
  the first one, `client.conductor_parks_overdue`, which `cavalry doctor` explains
  with the client's id. Clients on other systems have no watch and are unchanged
  ([ADR 0177](docs/decisions/0177-watch-a-clients-conductor-and-count-its-overdue-parks-in-a-counter-the-client-adds.md)).
