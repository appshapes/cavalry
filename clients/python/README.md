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

The package loads the library at its first use, `cavalry.Client()` or
`cavalry.version()`, and calls `cavalry_abi_version()` before any other function. A
library of another ABI version than the header the package was generated from raises
`cavalry.CavalryError` with code `CAVALRY_ERR_ABI_VERSION` and both numbers: load the
library built from the same checkout, or run `just generate-clients` against the
library's header (`spec/c-abi.md`, "Versions and what a reader rejects").

## What it must never do

- Never run a message through anything but one ABI call: no lock, no `await`, no
  thread, no copy of a fragment into a new array for the handler. `clients/lint.py`
  reads the per-message functions, `try_claim`, `set_reserved_value`, `commit`,
  `abort`, `offer`, `poll`, the trampoline and `Header.values` with the `Header`
  properties that call it, for Python's forms of each (`threading`'s locks,
  `async def`, `await`, `raise`, and building `CavalryError`), in their own bodies and
  in every function of the package they call, such as `_check`; `just lint` fails on
  any of them. `Header.values()` is None when the ABI refuses the header's pointer,
  which inside the handler's call it cannot. Otherwise it is the one frame struct the
  header keeps, rewritten by each call, so copy a field out to keep it.
- Never raise for an `Offer` code: `try_claim` and `offer` return the position after
  the frame or the code below zero (`cavalry.BACK_PRESSURED`, `NOT_CONNECTED`,
  `ADMIN_ACTION`, `CLOSED`, `MAX_POSITION_EXCEEDED`, `ERROR`), as the ABI does; the
  caller branches, as a Rust caller matches `Offer`.
- Never publish bytes the caller did not offer: `offer` takes `bytes`, or a `ctypes`
  `c_char` array such as `ctypes.create_string_buffer(n)`, which a caller may reuse
  across offers. A `str`, a `bytearray`, a `memoryview` or anything else is refused
  by `ctypes` with `ctypes.ArgumentError` before the ABI is called, so nothing is
  published; pass `bytes(view)`, or claim and write into `publication.claim`.
- Never let a view or a header outlive its call unnoticed. The views and the header
  point into the log, which is reused and unmapped later. Once the handler returns,
  the trampoline releases `body` and clears the header's pointer, so a kept `body`
  raises `ValueError` when used, and a kept header's `values()` is None (its
  properties raise `AttributeError`). `commit`, `abort` and the publication's `close`,
  which aborts a claim still outstanding, release `publication.claim` in the same way.
  This costs one call per message on each side. A slice of a view,
  or an export of it such as a `numpy.frombuffer` array, is not released and must not
  outlive the call: copy with `bytes(body)`. A handler that still holds an export of
  `body` when it returns makes `poll` raise `HandlerError` with a `BufferError`; an
  export of the claim held across `commit` is left to the caller to drop before the
  next claim.
- Never let a call the ABI refuses change a claim or a poll (`spec/c-abi.md`, "Calls
  during a claim or a poll"). A `try_claim` while a claim is outstanding returns
  `cavalry.ERROR` with `CAVALRY_ERR_CLAIM_OUTSTANDING`, and `publication.claim` stays
  the outstanding claim's view until its `commit` or `abort` releases it. A `poll`
  from inside a handler of the same subscription returns −1 with
  `CAVALRY_ERR_POLLING`, and the poll in progress goes on with its own handler and
  its own count of raises.
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

`just conformance` runs this program as written, through
`conformance/python/readme.py`, against the runner's driver, and fails unless it prints
that line within 10 s: change the program or the line, and the run says which.

## The runtime's side of the boundary

What `ctypes` does per message, and what `conformance/python/quickstart.py`'s report
measures of it. The figures below are one Linux CI runner's; every CI run prints its
runners'.

### The call

The package opens the library twice, which the loader maps once. The seven
per-message functions of `crates/cavalry-ffi/README.md` come through `ctypes.PyDLL`,
which keeps the GIL held across the foreign call, so an offer, a claim, a commit or
abort and a poll's call-backs run with no GIL release and re-acquire pair per message.
`ctypes.CDLL` would release the GIL around each call and take it back in each
call-back, two lock operations per message. Every other function comes through
`ctypes.CDLL`: `cavalry_init`, the adds, the recording and replay commands and the
closes wait on the driver, for up to 10 s on a timeout (`spec/c-abi.md`, "Threads"),
and with the GIL held every other Python thread of the process would stop for that
long, one polling another subscription included. The cost is the pair on each of those
calls, which are not per message. Every per-message function makes one call; `poll`'s
call-backs are the ABI calling the trampoline, not calls of their own. The runner
checks each function's loader.

### Threads

Releasing the GIL lets two Python threads be inside the ABI at once, and the ABI runs
one call on a handle at a time (`spec/c-abi.md`, "Threads"). A `Client` takes its own
lock around every call on its handle, never on a message's path, so threads may share
it. A `Publication`, `Subscription`, `Recording`, `Replay` or `Image` is used from one
thread at a time; give each thread its own.

### The call-back, by construction

For each received message `ctypes` builds the call's argument tuple and a new `int`
for each of the two pointers, the buffer's and the header's; the length is an `int`
too, one CPython keeps cached below 257. The trampoline then builds the view over the
body: a `c_uint8` array at the buffer's address, the `memoryview` over it with the
managed buffer CPython keeps behind a view, and that view cast to the native byte
format. That is seven objects per received message, the runtime's three and the
client's four, each freed before the next fragment, plus whatever `ctypes` makes inside
its own steps, such as finding the array type for the length, which this list does not
count.

The client's four cannot be one view kept per subscription, which ADR 0046 asks for
where the runtime allows it: the ABI hands the handler an address, not a term buffer
and an offset into it, and Python has no view whose address can be moved, so each body
needs a view of its own. What can be kept is kept, once per subscription: the
trampoline, the `Header`, its `cavalry_header_values_t` and the one frame struct over
it that every `header.values()` returns. ctypes builds a new object on each read of a
struct field, so the header keeps that one rather than reading the field per call.
Reading the header therefore makes no object of the client's own; each field read
gives an `int`, which CPython builds for a value above 256.

### The view over a claimed slice, by construction

`publication.claim` is a new `memoryview` per claim, built the same four-object way
from the pointer the ABI filled into the one `cavalry_buffer_claim_t` the publication
keeps: Python has no view that can be moved to a new address, so the one object per
publication the card asks for is the struct, and the view is the client's own code's
allocation per sent message, four objects plus the `int` the position comes back as,
recorded here with that reason. `ctypes` also converts each argument of each call into
an object of its own for the call.

### The count, measured

No counter in Python's standard library sees an object made and freed within one
message, so the runner reports net figures. Its allocation round sends 10,000 messages
whose handler only counts and whose publisher writes one constant, with `tracemalloc`
on for that round alone. On a Linux CI runner (Python 3.12.3): `sys.getallocatedblocks`,
read before and after the round with nothing read between, +0.000 live blocks per
message; `tracemalloc`, its own snapshots left out, +0.001 traced blocks and +0.0 bytes
per message, and a peak of 1,084 bytes traced above the round's start. So every object
a message makes is freed before the next, and no more than about 1 KB of them is alive
at once. The objects listed above are the per-message churn behind those net figures.

### The latency, measured

The runner's latency round sends 100,000 messages over `cavalry:ipc`, one at a time on
one thread, with `tracemalloc` off. The publisher writes `perf_counter_ns` into the
claim before its commit, and the handler reads the clock first. On the same runner,
unpinned: p50 1,022 ns, p99 1,503 ns, p99.99 6,342 ns (10 samples above it, nearest
rank), max 23,364 ns. That is the whole path through Python, the claim, the commit, the
poll and the call-back, the release of each view included ("What it must never do").
The bench's `one_way_<body>_ffi` row measures the path through the C ABI alone, with
the publisher and the subscriber pinned on threads of their own
(`docs/how-to/run-the-bench.md`).

### The exception

The trampoline wraps the user's handler in `try`; an exception is kept on the
subscription and the trampoline returns, so no Python frame unwinds through C. The
handler returns nothing the ABI reads, so the ABI's poll goes on delivering up to its
`fragment_limit`, and every fragment it delivers is consumed: the image's position
moves past it. The trampoline therefore calls the handler for every one of them, the
ones after a raise included, so that no message is lost (invariant 1). Once the ABI has
returned, `poll` raises `HandlerError`, with the first exception as its `__cause__`,
the poll's result as `fragments` (the count, or −1 when the poll also stopped on a
fault, which `fault()` reads) and the number of handler calls that raised as `raised`.
An exception that is not an `Exception`, a `KeyboardInterrupt` or a `SystemExit`, is
raised as itself and outranks the others, so Ctrl-C and `sys.exit(2)` in a handler do
what they do anywhere else. The runner checks both: a handler that raises on message 3
of a poll of 10 sees all 10, and a `SystemExit(2)` after a `ValueError` in one poll
comes out as `SystemExit(2)`.

When `poll` raises `HandlerError`, fix the handler. Python prints the first
exception's traceback above the `HandlerError`'s, as `The above exception was the
direct cause of the following exception`, and that traceback ends at the handler's
line that raised. The fragments the poll delivered were all handed to the handler, so
do not poll again to retry them: the next poll reads the fragments after them. A
handler that must not lose a message to its own failure catches its own exceptions and
copies the message with `bytes(body)` to handle later.

## What to read next

- `spec/c-abi.md`: the page the package wraps, function for function.
- `clients/README.md`: how a client is generated and what every client passes.
- `conformance/python/`: the runner and its report.
- `docs/decisions/0046-build-the-c-abi-and-the-clients-fourth.md`: the slice this
  client is the second feature of, and what the boundary's three parts are.
