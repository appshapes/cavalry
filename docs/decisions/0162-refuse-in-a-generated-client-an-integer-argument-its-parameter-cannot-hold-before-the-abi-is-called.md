# 0162. Refuse in a generated client an integer argument its parameter cannot hold, before the ABI is called

- **Status:** Accepted
- **Date:** 2026-11-25
- **Invariants affected:** 1, 2, 6
- **Supersedes:** ADR 0046 on one point, its check that "the per-message functions the
  generator writes have no lock, no `await` and no raise". A per-message function may
  now refuse an integer argument that its parameter cannot hold, by a raise in an
  argument reader that the lint names with its callers, before the function's ABI
  call. The rest of ADR 0046 stands: no raise for an `Offer` code, none in a
  trampoline, and none after the ABI was called but the one a poll makes for its
  handler.

## Context

A generated client passes its caller's integers to the C ABI through its runtime's
foreign-call layer, and two of the five runtimes convert an integer that the C
parameter cannot hold into another one, with nothing said.

- **Python, `ctypes`.** A function's `argtypes` entry of `c_int64`, `c_int32` or
  `c_size_t` takes any `int` and keeps its low bits. `client.truncate_recording(id,
  2 ** 64 + 96)` reaches the ABI as a truncate at 96, and
  `client.add_subscription(channel, 2 ** 32 + 1001)` as stream 1001.
- **Node, N-API.** `napi_get_value_int64` drops a number's fraction, reads a `NaN`
  and both infinities as 0, and holds a number past int64 to the nearest int64.
  `napi_get_value_int32` drops a fraction, reads the same as 0 and keeps the low 32
  bits of the rest. `client.truncateRecording(id, NaN)` reaches the ABI as a truncate
  at 0.
- **Go, the JVM and .NET** pass typed 64-bit and 32-bit integers, which their
  compilers hold to the parameter's type. One conversion remains in them: a count or
  a length is a signed integer in each language and a `size_t` in the ABI, and the
  cast each client makes keeps the bits, so a negative one reaches the ABI as a count
  near 2^64. `TryClaim(-1)` is a claim of 2^64 − 1 bytes, and `Poll(handler, -1)` a
  poll with no limit.

Work item 419 met this for a timer's id and its deadline and closed it for those two
arguments alone: a reader in the Python client, `_int64`, and one in the Node addon,
`int64_exact_arg`, each refusing before the ABI is called, with an allowance in
`clients/lint.py` for the reader's raise. No ADR recorded that allowance. Every other
integer argument was left as the runtime converts it, and work item 460 asks for
them.

The harm is not a wrong answer that the caller can see. A truncate or a purge at
another position removes recorded messages for good, a subscription to another stream
delivers another stream's messages, and a reply to another session answers a client
that did not ask. Each is a lie about delivery (invariant 1), and none says anything.

## Decision

**A generated client never passes the ABI another integer than its caller gave.** An
integer argument that its parameter cannot hold, and a value that is no integer of
the runtime, is refused before the ABI is called, with an error that names the
parameter, its range, what to pass, and the section of the client's README that says
this rule. The rule covers every function of a client that passes a caller's integer
to the ABI, per-message or not. It is stated once in the Python and in the Node
client's README, in "What it must never do".

**Python.** Who refuses depends on the function, so that a message's path gains no
call.

- An integer is an `int`, or a value with an `__index__`, such as a numpy integer or
  an `IntEnum`'s member, read as the exact `int` it gives. A value of the
  parameter's own `ctypes` type is passed as it is. `ctypes` passed all of these
  exactly, and they are passed still. A `float` is refused, as `ctypes` refused it.
  A `bool` is refused, though `ctypes` passed it as 0 or 1: `True` as a position or
  a length is a mistake, as it is no action for a service's handler.
- A function that is not per-message, an add, a command, a close: the binding gives
  each of its integer parameters a checking type, `_checked(ctype, name)` in
  `_abi.py`, in place of the plain `ctypes` type. The generator writes it from the
  header's type, so a function added to the header is covered with no line of the
  client's written for it. `ctypes` calls the type's `from_param` as it converts the
  argument; the refusal reaches the caller as `ctypes.ArgumentError`.
- A per-message function keeps the plain type. It tests its own argument in line, a
  type and two comparisons, and calls a reader, `_size` or `_int64`, only for a value
  that failed the test. The reader raises `OverflowError` or `TypeError`, or returns
  what the function passes on: the exact `int` of an integer in range. An `int` in
  range costs no call.
- The generator holds a list, `PER_MESSAGE_INTEGERS`, of every integer parameter of a
  per-message function with who reads it: a reader, the package's own `len` of a
  message, or the trampoline, which holds the action a service's handler returned to
  an int32 and treats one outside it as no action. The generator fails on a
  per-message integer parameter that the list does not name, on a line for anything
  else, and on a line that the text of the function that makes the ABI call does not
  bear out. It also fails on a parameter of a scalar type that no checking type
  reads, a `bool` today.

**Node.** The addon is to call neither `napi_get_value_int64` nor
`napi_get_value_int32`. Every integer argument is to be read by the reader the timers
have: an int64 parameter takes a BigInt in the int64 range or a number that is an
integer from −(2^53 − 1) to 2^53 − 1; a count or a length takes the same from 0; an
int32 parameter takes a number that is an integer in the int32 range. Anything else
is a `RangeError`, or a `TypeError` for another type. A reader asks N-API for the
number first, `napi_get_value_double`, and for the value's type only when that call
says the value is no number, so a number costs one N-API call, as its conversion did.
This is the second pull request of work item 460. On this ADR's date the Node client
still converts as the Context says.

**The lint.** `clients/lint.py` allows a per-message function a raise in one place
before its ABI call: the own body of an argument reader that the rule names, for the
functions the rule names with it, on a line before the function's ABI call or as an
argument of it. A reader called from a trampoline, from a function the rule does not
name for it, or after the ABI call, a poll's among them, is a finding, as a raise
anywhere else on the path is. For Python the rule names `_int64` with a service's
`schedule_timer`, `cancel_timer` and `reply` and a publication's
`set_reserved_value`, and `_size` with `try_claim` and the four polls. The raise ADR
0046 gives a poll for its handler's exception, once its ABI call has returned, is as
it was.

**Go, the JVM and .NET** are held to the rule and break it in the one place the
Context names, a negative count or length. Work item 465 closes that. This ADR
changes nothing in them, and their READMEs say nothing new until it lands.

## Alternatives considered

- **Say in the README, per function, that the wrap is harmless.** The work item
  allows it. It is not harmless for one function that takes a position, a recording's
  id, a stream id or a session's id, and for the rest a sentence per function is more
  to keep true than a generated type.
- **A list of the arguments that matter, each with a reader.** That is what work item
  419 did for two. A list is wrong the day the header gains a function, and the
  manager's lean was every integer argument, written by the generator from the
  header's types. The decision takes that for every function that is not
  per-message, where a list is not needed at all.
- **The checking type on the per-message functions too.** One mechanism, and nothing
  for the lint to allow, since the raise would be `ctypes`' own. But `from_param` is a
  call into Python for every integer argument of every claim and every poll, on the
  path invariant 2 budgets, to refuse a value that a correct program never passes.
  The test in line makes no call for a value in range.
- **A reader called for every per-message argument**, as work item 419's
  `schedule_timer` called `_int64`. A call per claim and per poll where the test in
  line has none. The two timer calls now test in line as the rest do, so that one
  sentence says what every per-message function does.
- **Refuse what is no `int`, an object with an `__index__` among them**, as work item
  419's reader did. A numpy integer in range was passed exactly before this change,
  and is no mistake. The rule is about the value that reaches the ABI, and that value
  is the caller's.
- **In Node, keep `int64_arg` as N-API converts and read exactly only what matters.**
  The same list, with the same fault. The exact reader costs a number the same one
  call.
- **Refuse in the ABI.** The ABI cannot: it is handed an int64 that is in range, and
  cannot know it was another number in the caller's hands.

## Evidence

All of it is from one laptop, macOS on Apple silicon, on 2026-11-25, with Python 3.14 and
Node 24.16; none is from the bench hosts.

**What the runtimes do with such a value.**

- `ctypes`: a function of the C library that takes one int64, `llabs`, called with
  `2 ** 64 + 5` through an `argtypes` of `c_int64`, answers 5. With the binding's
  checking type in its place the same call raises `ctypes.ArgumentError`
  (`clients/generate-test.py`).
- N-API, through a probe addon of two functions that return what
  `napi_get_value_int64` and `napi_get_value_int32` read from a number:
  - As int64: 96.5 is 96 and −1.9 is −1; `NaN`, `Infinity` and `-Infinity` are 0;
    2^63, 2^64 + 96 and 1e300 are 2^63 − 1, and −1e300 is −2^63.
  - As int32: 1001.7 is 1001; `NaN` and `Infinity` are 0; 2^31 is −2^31,
    −2^31 − 1 is 2^31 − 1, and 2^32 + 1001 is 1001.

**That the harm is real, on `master`'s clients.** Each client's conformance runner
records 1,000 messages, stops the recording, and later replays all of it. With the
refusals of this change added to the runner and the client left as `master` has it:

- Python, the truncate's `position` given the plain `c_int64`: the truncate at
  `2 ** 64 + 96` returned with no error, having truncated the stopped recording at 96,
  and the runner failed there.
- Node, `master`'s addon: `client.truncateRecording(recordingId, NaN)` returned with
  no error, having truncated at 0, and the runner failed there.

With the change, the Python runner refuses a truncate, two purges and an add with an
integer outside its parameter's range, and a truncate at a `float`, and the replay
then reads the 1,000 messages at their positions: 20 runs of 20.

**What a message's path pays in Python.** The runner's latency round sends 100,000
messages over `cavalry:ipc`, one at a time on one thread, unpinned, and reports the
time from the publisher's clock read before the commit to the handler's first clock
read. Each message makes one `try_claim` and one `poll`, so two of the tests in line.
The clock steps by 41 or 42 ns. Ten runs of `master`'s client and twenty of the
changed one, in the same two hours, the lowest and the highest run of each:

| | p50 | p99 | p99.99 | max |
| --- | --- | --- | --- | --- |
| `master`, 10 runs | 750 to 792 ns | 959 to 1,334 ns | 4,500 to 43,458 ns | 12,000 to 201,792 ns |
| changed, 20 runs | 791 to 833 ns | 958 to 1,084 ns | 4,334 to 11,750 ns | 14,375 to 136,125 ns |

The median is one step of the clock higher: 750 ns in six of `master`'s ten runs and
in none of the changed client's twenty, of which nineteen read 791 or 792 ns. That is
the two tests. The p99, the p99.99 and the max of the changed client's runs are
inside the spread of `master`'s. These are a laptop's runs, and no run's number is a
bound.

**That the gates hold it.** `clients/generate-test.py` holds every refusal of every
checking type at each end of its range, through `ctypes` itself for one, and the two
readers; `clients/lint-test.py` holds the allowance's edges for both readers; and the
generator fails on a per-message integer parameter that its list does not name.

## Consequences

- A program that passed the Python client an `int` its parameter cannot hold had
  another value passed, and is now refused. That is the change's purpose.
- Two programs that had nothing converted are refused too, and `changelog.d/` says
  so. One passed a `bool`, which reached the ABI as 0 or 1. The other catches
  `ctypes.ArgumentError` for a `float` given to a per-message function, which is now
  a `TypeError`.
- The Node client's second pull request will refuse a fraction, a `NaN`, an infinity
  and an out-of-range number, each of which N-API converted.
- The Python client refuses with two kinds of error by the function called:
  `ctypes.ArgumentError` from a command, and `OverflowError` or `TypeError` from a
  per-message function. One kind would need either the call the per-message path is
  spared or a wrapper around every command. A caller that catches a refusal catches
  the three; the README says which function raises which.
- A per-message function of the Python client that takes a caller's integer gains a
  type test and two comparisons per call. The Evidence has the runner's histograms
  before and after. The bench's rows do not move: they measure the path through the C
  ABI, which this change does not touch.
- `PER_MESSAGE_INTEGERS` is one more list to keep. The generator fails when it is
  behind the header, in either direction, so it cannot go stale in silence.
- The lint's allowance is wider by one reader and seven callers. It is still a raise
  before the ABI call for a caller's own mistake, and the lint's tests hold each edge
  of it for both readers: another function, the other reader's function, a
  trampoline, a helper between, a call after the ABI's. The lint reads a function's
  text, so a reader reached through an alias or a closure is not seen, as no call so
  reached is; that limit is older than this ADR.
- The Go, the JVM and the .NET client do not hold the rule for a negative count until
  work item 465 lands.
- A new client for a runtime that converts integers, should Cavalry gain one, is held
  to this rule by its README's section and its conformance runner; no gate finds such
  a runtime by itself.

## References

- Work item 460; work item 419, whose two readers this generalizes; and work item
  465, for a negative count in the Go, the JVM and the .NET client.
- ADR 0046, "The client's own code", for the check this supersedes on one point; ADR
  0157 for the timers whose id and deadline were read first.
- `clients/python/README.md` and `clients/node/README.md`, "What it must never do".
- `clients/generate.py` (`_checked`, `PER_MESSAGE_INTEGERS`), `clients/lint.py` (the
  `readers` rule), `clients/generate-test.py` and `clients/lint-test.py` for their
  tests, and `conformance/python/quickstart.py` for the refusals through `ctypes`.
