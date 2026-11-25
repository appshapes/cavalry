- The Python client now refuses an integer argument that its parameter cannot hold,
  before the ABI is called. `ctypes` passed such an `int` as its low bits and said
  nothing: a truncate at `2 ** 64 + 96` was a truncate at 96, and a subscription to
  stream `2 ** 32 + 1001` one to stream 1001. An add or a command now raises
  `ctypes.ArgumentError`, and `try_claim`, `set_reserved_value`, a poll and a service's
  `reply` raise `OverflowError`, or `TypeError` for a value that is no `int`, a `float`
  and a `bool` among them. Each message names the parameter and its range. A program
  that passed such a value was served another one, and is now refused
  ([ADR 0162](docs/decisions/0162-refuse-in-a-generated-client-an-integer-argument-its-parameter-cannot-hold-before-the-abi-is-called.md)).
