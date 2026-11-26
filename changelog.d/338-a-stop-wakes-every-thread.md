- A driver's stop and a client's close no longer wait on a thread's idle wait to
  time out: the idle loops park where they slept, for the same 1 µs to 1 ms, and a
  stop wakes every thread it joins. On one Mac a timed wait of 1 ms in about 20
  million was never ended by its timer, and a test run or a `cavalry run` then hung
  in its stop. A loop of your own that idles with `Idler` and is stopped from another
  thread needs the same: store its flag, then unpark its thread
  ([ADR 0171](docs/decisions/0171-park-the-idle-loops-where-a-stop-can-wake-them-and-wake-every-thread-at-a-stop.md);
  [Find a wait that does not return](docs/how-to/find-a-wait-that-does-not-return.md)).
