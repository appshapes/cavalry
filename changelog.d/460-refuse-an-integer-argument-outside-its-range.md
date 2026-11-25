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
- The Node client now reads every integer argument exactly, and refuses one its
  parameter cannot hold before the ABI is called. N-API read a `NaN` or an infinity
  as 0, dropped a fraction, held a number past int64 to the nearest int64 and kept the
  low 32 bits of a stream id: `client.truncateRecording(recordingId, NaN)` truncated
  the recording at 0. An int64 argument is a number that is an integer from
  −(2^53 − 1) to 2^53 − 1 or a BigInt in int64, a count or a length the same from 0,
  and a stream id a number in int32; anything else is a `RangeError`, or a `TypeError`
  for a value of another type, with the parameter's name and its range. The types
  say `number | bigint` for each int64 and each count, so `startReplay` takes
  `constants.CAVALRY_FOLLOW_LENGTH` as it is; as the number `2 ** 63` it is now
  refused. Refused too, where each was taken before: a stream id from 2^31 to
  2^32 − 1, which wrapped the same way for a publisher and a subscriber, and a
  number at or past 2^53 as a fragment limit. A count below zero is a `RangeError`
  where it was a `TypeError`
  ([ADR 0162](docs/decisions/0162-refuse-in-a-generated-client-an-integer-argument-its-parameter-cannot-hold-before-the-abi-is-called.md)).
- The Node client now refuses a typed array of a kind its addon does not name, a
  `Float16Array`, in `offer`, a session's `offer`, a service's `reply` and its
  `snapshot`, with a `TypeError` that says to pass a `Uint8Array` over the same
  bytes. The addon took such an array's elements for 8 bytes wide, and handed the
  ABI four times a `Float16Array`'s length
  ([the Node client](clients/node/README.md#what-it-must-never-do)).
