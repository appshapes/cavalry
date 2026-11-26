- On macOS a driver now has a sixth thread, the watch, which ends a park of one of
  its five threads that outlived its timeout by more than 200 ms, and counts it in
  the new counter `driver.parks_overdue`, which `cavalry doctor` explains. On one
  Mac the system loses about one timer wake-up in 40 million, and a driver left
  running there went silent one thread at a time: a lost wake-up now costs a stall of
  at most 300 ms past its timeout. Drivers on other systems have no watch and are unchanged
  ([ADR 0176](docs/decisions/0176-watch-the-drivers-parks-and-end-one-that-outlived-its-timeout.md);
  [Find a wait that does not return](docs/how-to/find-a-wait-that-does-not-return.md)).
