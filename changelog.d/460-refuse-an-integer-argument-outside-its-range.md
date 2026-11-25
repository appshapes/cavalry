- The Python client now refuses an integer argument that its parameter cannot hold,
  before the ABI is called. `ctypes` passed such an `int` as its low bits and said
  nothing: a truncate at `2 ** 64 + 96` was a truncate at 96, and a subscription to
  stream `2 ** 32 + 1001` one to stream 1001. An add or a command now raises
  `ctypes.ArgumentError`, and `try_claim`, `set_reserved_value`, a poll and a service's
  `reply` raise `OverflowError`. Each message names the parameter and its range. An
  action a service's handler returns that is outside int32 is no action, where
  `ctypes` passed its low 32 bits. An integer in range is passed as before, an `int`
  or a value with an `__index__`, such as a numpy integer. A program that passed no
  out-of-range `int` meets the change in two places. A `bool` was passed as 0 or 1
  and is now refused. A value that is no integer, a `float`, a `str` or `None`, given
  to one of those per-message functions, was refused with `ctypes.ArgumentError` and
  is now refused with a `TypeError`
  ([ADR 0162](docs/decisions/0162-refuse-in-a-generated-client-an-integer-argument-its-parameter-cannot-hold-before-the-abi-is-called.md)).
