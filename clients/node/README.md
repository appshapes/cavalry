# @cavalry/client, the Node client

## What this does

A Node package over the C ABI of `spec/c-abi.md`, generated from the committed header
by `clients/generate.py node`: a native addon in C over N-API (`src/addon.c`, built by
`node-gyp` against the header and linked with `cavalry-ffi`'s static library) that calls
the ABI function for function, and the client's own code (`index.js`, typed by
`index.d.ts`) that gives the handles the forms a Node user expects. A `Client` closes
with `close()` or `using`; a `Publication` claims a slice of the log as a `Buffer`
over it or offers a `Buffer`; a `Subscription` polls with a fragment handler function
that receives the body as a `Buffer` into the log and the frame's header; a
`Recording` and a `Replay` wrap the driver's archive. Every message the client sends or
receives goes through one `extern "C"` call on the caller's thread, as the ABI's page
promises (ADR 0046's boundary; ADR 0051). The package has no runtime dependency: N-API
is part of Node, and `npm install` in `clients/node` builds the addon with the
`node-gyp` that ships inside `npm`, so nothing comes from the registry.

The addon is the card's choice over a foreign-function package (`koffi`, `ffi-napi`):
a package would be a third-party native module the client depends on, and its
call-back builds more objects per fragment than N-API's own call does; `napi-rs` over
the Rust crates is not an option, since it would go round the header (ADR 0046,
Alternatives). The cost of the choice is a C compiler at install, which `node-gyp`
needs and both CI runners and the ten-minute path's machines have.

## What it must never do

- Never run a message through anything but one ABI call: no lock, no `await`, no
  promise, no copy of a fragment into a new array for the handler. `clients/lint.py`
  reads the per-message functions for JavaScript's forms of each (`async`, `await`,
  `Promise`, `.then`, `throw`, `Atomics.wait`) and `just lint` fails on any of them.
- Never throw for an `Offer` code: `tryClaim` and `offer` return the position after
  the frame or the code below zero (`NOT_CONNECTED`, `BACK_PRESSURED`, `ADMIN_ACTION`,
  `CLOSED`, `MAX_POSITION_EXCEEDED`, `ERROR`), as the ABI does; the caller branches.
- Never swallow the handler's exception: a fragment handler that throws keeps every
  later fragment of that poll reaching the handler, and `poll` throws a
  `HandlerError` once the ABI has returned, with the first exception as its `cause`
  and the counts, the one throw the per-message path allows.
- Never edit a generated file by hand: `just lint` regenerates the package and fails
  when the committed files differ.

## Example

The ten-minute path's program (the README's `cavalry run`, the command that builds
the library from the same checkout, and the addon's build), as
`conformance/node/quickstart.js` runs it with its checks:

```sh
cargo build --release -p cavalry --bin cavalry   # the driver
cargo build --release -p cavalry-ffi --lib       # libcavalry_ffi.a, which the addon links
(cd clients/node && npm install)                 # the addon, build/Release/cavalry_addon.node
target/release/cavalry run &                     # one driver per host
node - <<'JS'
const cavalry = require("./clients/node");

const client = new cavalry.Client();                                   // the driver at CAVALRY_DIR, or its default
const subscription = client.addSubscription("cavalry:ipc", 1001);
const publication = client.addPublication("cavalry:ipc", 1001);
const received = [];
const onFragment = (body, header) => received.push(body.readUInt32LE(0)); // body: a view into the log, for the call

let sent = 0;
while (sent < 1000) {
  const result = publication.tryClaim(32);                              // the position after the frame, or a code below 0
  if (result < 0) { subscription.poll(onFragment, 10); continue; }      // back-pressured or not yet connected: drain, retry
  publication.claim.writeUInt32LE(sent, 0);                             // write into the log
  publication.commit();
  sent += 1;
}
while (received.length < 1000) subscription.poll(onFragment, 10);
if (received.some((value, index) => value !== index)) throw new Error("every message once, in order");
console.log(`${received.length} messages over cavalry:ipc, position ${publication.position()}`);
client.close();
JS
kill -TERM %1
```

You will see `1000 messages over cavalry:ipc, position 64000`.

## The runtime's side of the boundary

What N-API does per message, and what `conformance/node/quickstart.js`'s report
measures of it. The figures below are the laptop's (Node v24.16.0, macOS 26.6 on
arm64); every CI run prints its runners'.

### The call

Each per-message function is one N-API call into the addon, which reads its arguments
without copying, the handle as an external, the claim struct and the offered bytes
through their `ArrayBuffer`'s memory, and makes the one ABI call on the caller's
thread. A position comes back as a number (`napi_create_int64`), exact below 2^53,
which every position is. Node has one thread for JavaScript, so a call holds it for the
call, and an add, a command or a close, which waits on the driver for up to 10 s
(`spec/c-abi.md`, "Threads"), holds it as long: the client is synchronous, and a
program that must not block its event loop runs the client on a worker thread.

### Threads

A `Client` and the handles it makes are used from the thread that made them, one call
at a time; the client adds no lock, and the ABI runs one call on a handle at a time.
A program with several threads gives each `worker_threads` worker its own `Client`:
the addon's per-call state, the flags its argument readers set, is per thread, and its
per-subscription state belongs to the subscription.

### The call-back, by construction

The ABI calls the addon's trampoline with the body's address and length and the
frame's header. The trampoline opens a handle scope, makes a `Buffer` over the body's
bytes with `napi_create_external_buffer`, no copy, takes the subscription's one
`Header` object and the handler from the references it keeps, calls the handler
through `napi_call_function`, detaches the `Buffer`'s `ArrayBuffer` so that a kept body
reads as empty, and closes the scope. That is two objects per received message, the
`Buffer` and the `ArrayBuffer` under it, both the runtime's, freed by the next
collection. The `Header`'s getters each call the addon's `headerField`, which reads
`cavalry_header_values` once per fragment, on the first getter, into the one
`cavalry_header_values_t` the addon keeps per subscription, and returns the field as a
number, which V8 keeps on the stack for a small integer; outside the handler call every
getter is `undefined`, since the header is valid for the call alone.

### The view over a claimed slice, by construction

`publication.claim` is a new `Buffer` per claim, over the bytes the ABI reserved,
detached by `commit`, `abort` and `close`: V8 has no view that can be moved to a new
address, so the one object per publication is the claim struct, a 32-byte
`ArrayBuffer` the addon fills, and the view is the client's own code's allocation per
sent message, two objects, the `Buffer` and its `ArrayBuffer`, recorded here with that
reason.

### The count, measured

The runner's allocation round sends 10,000 messages whose handler only counts and whose
publisher copies one constant, with V8's sampling heap profiler on for that round
alone, the objects a collection freed included, and a full collection before and after
(`node --expose-gc`). On the laptop: 490 bytes allocated per message, an unbiased
estimate from samples every 64 bytes, which is the four objects above and what V8 makes
around a call; and -0.7 bytes kept per message after the collection
(`process.memoryUsage().heapUsed`, nothing read between), so everything a message
makes is freed by the next collection.

### The latency, measured

The runner's latency round sends 100,000 messages over `cavalry:ipc`, one at a time on
one thread, with the profiler off. The publisher writes `process.hrtime.bigint()` into
the claim before its commit, and the handler reads the clock first. On the laptop,
unpinned: p50 375 ns, p99 834 ns, p99.99 15,292 ns (10 samples above it, nearest
rank), max 73,042 ns. The `BigInt` each side makes for the clock is the round's own
cost, not the client's.

### The exception

The trampoline sees the handler's exception as N-API's pending exception
(`napi_get_and_clear_last_exception`), keeps the first, counts every one, and goes on
calling the handler for every later fragment of the poll, so that no fragment is
consumed unseen; `poll` throws `HandlerError` once the ABI has returned, with
`fragments`, `raised`, the first exception as its `cause`, and a message that says
what to do. A value that is not an `Error` is kept as thrown.

## What to read next

- `spec/c-abi.md`: the page the package wraps, function for function.
- `clients/README.md`: how a client is generated and what every client passes.
- `clients/python/README.md`: the first client, whose shape this one copies.
- `conformance/node/`: the runner and its report.
- `docs/decisions/0046-build-the-c-abi-and-the-clients-fourth.md`: the slice this
  client is the third feature of.
