- The Python client now refuses an integer argument that its parameter cannot hold,
  before the ABI is called. `ctypes` passed such an `int` as its low bits and said
  nothing: a truncate at `2 ** 64 + 96` was a truncate at 96, and a subscription to
  stream `2 ** 32 + 1001` one to stream 1001. An add or a command now raises
  `ctypes.ArgumentError`, and `try_claim`, `set_reserved_value`, a poll and a service's
  `reply` raise `OverflowError`. Each message names the parameter and its range. An
  integer in range is passed as before, an `int` or a value with an `__index__`, such
  as a numpy integer. Two things change for a program that passed no such `int`: a
  `bool` was passed as 0 or 1 and is now refused, and a `float` given to one of those
  per-message functions, which `ctypes` refused with `ctypes.ArgumentError`, is now a
  `TypeError`. An action a service's handler returns that is outside int32 is no
  action, where `ctypes` passed its low 32 bits
  ([ADR 0162](docs/decisions/0162-refuse-in-a-generated-client-an-integer-argument-its-parameter-cannot-hold-before-the-abi-is-called.md)).
