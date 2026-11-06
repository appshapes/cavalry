# cavalry, the Python client

## What this does

A Python package over the C ABI of `spec/c-abi.md`, generated from the committed
header by `clients/generate.py`: `ctypes` loads `libcavalry_ffi` and declares every
function, struct and constant of the header (`cavalry/_abi.py`), and the client's own
code (`cavalry/__init__.py`) gives the handles the forms a Python user expects. A
`Client` is a context manager that closes itself; a `Publication` claims a slice of the
log as a writable `memoryview` or offers `bytes`; a `Subscription` polls with a callable
fragment handler that receives the body as a `memoryview` into the log and the frame's
header; a `Recording` and a `Replay` wrap the driver's archive. Every message the
client sends or receives goes through one `extern "C"` call on the caller's thread, as
the ABI's page promises (ADR 0046's boundary; ADR 0051). The package has no dependency:
`ctypes` is in Python's standard library, and both CI runners carry Python 3 already.

## What it must never do

- Never run a message through anything but one ABI call: no lock, no `await`, no
  thread, no copy of a fragment into a new array for the handler. `clients/lint.py`
  reads the per-message functions, `try_claim`, `set_reserved_value`, `commit`,
  `abort`, `offer`, `poll`, the trampoline and `Header.values` with the `Header`
  properties that call it, for Python's forms of each (`threading`'s locks,
  `async def`, `await`, `raise`, and building `CavalryError`), in their own bodies and
  in every function of the package they call, such as `_check`; `just lint` fails on
  any of them. `Header.values()` is None when the ABI refuses the header's pointer,
  which inside the handler's call it cannot.
- Never raise for an `Offer` code: `try_claim` and `offer` return the position after
  the frame or the code below zero (`cavalry.BACK_PRESSURED`, `NOT_CONNECTED`,
  `ADMIN_ACTION`, `CLOSED`, `MAX_POSITION_EXCEEDED`, `ERROR`), as the ABI does; the
  caller branches, as a Rust caller matches `Offer`.
- Never drop a fragment and never swallow the user's exception: the ABI's poll
  consumes every fragment it delivers, so every one reaches the handler, once and in
  order, even after a call raised; `poll` raises once the ABI has returned, the one
  raise the per-message path allows ("The exception", below).
- Never edit a generated file by hand: `just lint` regenerates the package and fails
  when the committed files differ.

## Example

The ten-minute path's program (the README's `cavalry run`, plus the one command that
builds the library from the same checkout), as `conformance/python/quickstart.py` runs
it with its checks:

```sh
cargo build --release -p cavalry --bin cavalry   # the driver
cargo build --release -p cavalry-ffi --lib       # libcavalry_ffi, which the package loads
target/release/cavalry run &                     # one driver per host
PYTHONPATH=clients/python python3 - <<'PY'
import cavalry

with cavalry.Client() as client:                              # the driver at CAVALRY_DIR, or its default
    subscription = client.add_subscription("cavalry:ipc", 1001)
    publication = client.add_publication("cavalry:ipc", 1001)
    received = []

    def on_fragment(body, header):                            # body: a memoryview into the log, for the call
        received.append(int.from_bytes(body[:4], "little"))

    sent = 0
    while sent < 1000:
        result = publication.try_claim(32)                    # the position after the frame, or a code below 0
        if result < 0:
            subscription.poll(on_fragment, 10)                # back-pressured or not yet connected: drain, retry
            continue
        publication.claim[:4] = sent.to_bytes(4, "little")    # write into the log
        publication.commit()
        sent += 1
    while len(received) < 1000:
        subscription.poll(on_fragment, 10)
    assert received == list(range(1000)), "every message once, in order"
    print(f"{len(received)} messages over cavalry:ipc, position {publication.position()}")
PY
kill -TERM %1
```

You will see `1000 messages over cavalry:ipc, position 64000`.

## The runtime's side of the boundary

What `ctypes` does per message, measured by `conformance/python/quickstart.py`'s report
(the figures below are the laptop's; CI prints the runners'):

- **The call.** The package loads the library with `ctypes.PyDLL`, which keeps the
  GIL held across each foreign call, so a poll's call-backs run with no GIL release
  and re-acquire pair per message; `ctypes.CDLL` would release the GIL around each
  call and take it back in each call-back, two lock operations per message, for the
  sake of other Python threads during the call, which a client on the caller's thread
  does not need. Every per-message function makes one call; `poll`'s call-backs are
  the ABI calling the trampoline, not calls of their own.
- **The call-back.** `ctypes` builds the trampoline's arguments as new Python objects
  on every call, an `int` each for the buffer pointer, the length and the header
  pointer; the trampoline then builds the view over the body, a `c_uint8` array at the
  buffer's address, its `memoryview` and that view cast to the native byte format,
  three objects. Those are the runtime's and the client's allocations per received
  message, by construction: six objects, every one freed before the next fragment.
- **The view over a claimed slice.** `publication.claim` is a new `memoryview` per
  claim, built the same three-object way from the pointer the ABI filled into the one
  `cavalry_buffer_claim_t` the publication keeps: Python has no view that can be moved
  to a new address, so the one object per publication the card asks for is the struct,
  and the view is the client's own code's allocation per sent message, three objects
  plus the `int` the position comes back as, recorded here with that reason.
- **The count, measured.** The runner's bare round, 1,000 messages whose handler only
  counts and whose publisher writes one constant, on this laptop (macOS, Python
  3.14): `tracemalloc` reads +0.01 traced blocks and −1 byte net per message, so what
  each message allocates is freed before the next, with a peak of 172 KB traced across
  the round; `sys.getallocatedblocks` reads +8.3 live blocks net per message, the
  allocator's own bookkeeping of freed objects it keeps. The checked round's one-way
  latency, the publisher's `perf_counter_ns` in the body to the handler's read of it,
  was p50 20.5 µs, p99.99 43.8 µs, max 43.8 µs over 1,000 ipc messages, unpinned; the
  C runner's figures through the same ABI are the measure of the boundary itself, and
  the difference is Python's call-back.
- **The exception.** The trampoline wraps the user's handler in `try`; an exception
  is kept on the subscription and the trampoline returns, so no Python frame unwinds
  through C. The handler returns nothing the ABI reads, so the ABI's poll goes on
  delivering up to its `fragment_limit`, and every fragment it delivers is consumed:
  the image's position moves past it. The trampoline therefore calls the handler for
  every one of them, the ones after a raise included, so that no message is lost
  (invariant 1). Once the ABI has returned, `poll` raises `HandlerError`, with the
  first exception as its `__cause__`, the poll's result as `fragments` (the count, or
  −1 when the poll also stopped on a fault, which `fault()` reads) and the number of
  handler calls that raised as `raised`. An exception that is not an `Exception`, a
  `KeyboardInterrupt` or a `SystemExit`, is raised as itself and outranks the others,
  so Ctrl-C and `sys.exit(2)` in a handler do what they do anywhere else. The runner
  checks both: a handler that raises on message 3 of a poll of 10 sees all 10, and a
  `SystemExit(2)` after a `ValueError` in one poll comes out as `SystemExit(2)`.

## What to read next

- `spec/c-abi.md`: the page the package wraps, function for function.
- `clients/README.md`: how a client is generated and what every client passes.
- `conformance/python/`: the runner and its report.
- `docs/decisions/0046-build-the-c-abi-and-the-clients-fourth.md`: the slice this
  client is the second feature of, and what the boundary's three parts are.
