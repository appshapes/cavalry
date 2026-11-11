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
`Recording` and a `Replay` wrap the driver's archive; a `CommittedLog` is a service's
read of a cluster's committed log beside a member (below). Every message the client sends or
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

### The committed log

A service is a program beside a cluster member that applies the entries the cluster
has committed (`docs/how-to/read-the-committed-log.md`). `client.committedLog(position)`
opens the log of the member on the client's driver at a position, 0 or the
`position()` an earlier run reached, and returns `null` while the driver hosts no
member with a log, so a service started before its member asks again. `log.poll(handler,
fragmentLimit)` calls `handler(body, header, position)` for each committed entry, in
order, none at or past the member's commit position; `position` is the entry's
position in the log, a number. It returns the entry count, or −1 when the ABI's poll
stopped on a fault, which `log.fault()` gives as `{ code, sessionId, position }` until
the next poll. `log.position()` is the position after the last entry delivered, the
one a service keeps for its next start, and `log.commitPosition()` the bound of the
last poll. `log.isEnded` says that no poll will deliver another entry: stop reading
when it is true, not when a poll returns 0, then close the log and open another from
`log.position()`. `log.close()` stops the driver's replay, and a second close does
nothing; a log closed after its client is freed, and the driver ends the replay by
itself. When the ABI could not stop the replay, `log.close()` throws `CavalryError`
after it has freed the log, and a second close does nothing. `log.memberId` and
`log.recordingId` say whose log it is (`spec/c-abi.md`, "Functions", committed log;
ADR 0070).

An ended log delivered every committed entry it may read only when three things hold:

- `log.failedAt()` is `null`. A position is where the driver's replay of the member's
  recording failed, and no entry from it on was delivered.
- `log.withdrawnAt()` is `null`. A position is where the entries begin that the log
  was not given, because the driver withdrew the replay, 5 s after it ended, before
  the service had read it.
- `log.faulted()` is `null`. A fault, `{ code, sessionId, position }`, is where this
  client's read of the replay's image, the log's own, stopped: the code is
  `CAVALRY_ERR_FAULT_IMAGE` when a frame of the image failed a check, or when the
  image was gone before this client read it, and `CAVALRY_ERR_FAULT_IMAGES_FULL`
  when the image could not join. No entry from the fault on was delivered.

Read the three reports once `log.isEnded` is true. The driver withdraws a replay that
failed as it does any other, so the first two can both be positions:
`log.withdrawnAt()` is then the lower, and the entries from it on, those below
`log.failedAt()` among them, were not delivered. The poll that meets the image's fault
returns −1 once, `log.fault()` answers it until the next poll, and the next poll
returns 0 and clears it there. `log.faulted()` answers it from that poll on, for as
long as the log is held (ADR 0080). `log.withdrawnAt()` is `null` for such a log, and
`log.failedAt()` is a position only when the driver's replay failed as well, which is
then the cause to look at first. In every case what the log delivered ends at `log.position()`.

The addon reads a committed log through the trampoline a subscription's poll uses,
with one step more for each entry: the number its position is handed to the handler
as. A handler's exception goes where a subscription's goes ("The exception"): the
poll goes on handing the handler its later entries, and `poll` throws `HandlerError`
once the ABI has returned, with the poll's result as its `fragments`, the −1 of a
fault included. An entry whose handler call threw was delivered and is not delivered
again: `log.position()` is past it, and past every entry the same poll delivered after
it. A handler cannot stop a poll: the ABI goes on handing it entries up to the
fragment limit.

The runtime can also keep an entry from the handler, when N-API refuses a step of the
call ("The exception"). The ABI has consumed that entry, so it was not delivered. The
addon then hands the handler no later entry of the same poll either, so that a
service never applies an entry after one it did not see. `poll` throws `HandlerError`
with `undelivered` counting these entries and `undeliveredAt` the position of the
first. When no handler call of the poll threw before that entry, the `cause` is the
addon's `Error` for it, which names it by its position, its `position` property too.
`log.position()` is past them all, so polling the same log again does not deliver
them. `undeliveredAt` is `null` when every entry reached the handler.

So a service that must apply every entry, in order, does this:

1. The handler catches its own exceptions. When it cannot apply an entry, it keeps
   that entry's `position` and sets a flag.
2. While the flag is set the handler applies nothing and returns at once, for the rest
   of that poll: the poll still hands it the entries after the one that failed.
3. The service catches `HandlerError` from `log.poll`. Its `undeliveredAt`, when it is
   not `null`, is where the entries begin that the handler was not handed.
4. After the poll, when the handler kept a position or `undeliveredAt` is a position,
   the service closes the log and opens a new one at the position the handler kept,
   or, when it kept none, at `undeliveredAt`. A position the handler kept is always
   the lower, since no entry from `undeliveredAt` on reached the handler.

With entries at 0, 96 and 192 and a failure at 96, opening again at `log.position()`
would lose the entry at 96, and applying 192 before opening again at 96 would apply
192 twice. When the runtime kept the entry at 96 from the handler, `undeliveredAt` is
96, the handler was not handed 192 either, and the service opens the log again at 96.
A `fragmentLimit` of 1 is the simple form: a poll then delivers at most one entry, and
the service decides after each.

## What it must never do

- Never run a message through anything but one ABI call: no lock, no `await`, no
  promise, no copy of a fragment into a new array for the handler. `clients/lint.py`
  reads the per-message functions of `index.js` for JavaScript's forms of each
  (`async`, `await`, `Promise`, `.then`, `throw`, `Atomics.wait`), and those of
  `src/addon.c`, the trampoline, the Buffers' finalizer, `poll`, `committedLogPoll`,
  `headerField`, `view`, `detach` and the wrappers of the claim, the reserved value, the commit, the abort
  and the offer, for C's (`pthread_mutex_*`, `pthread_rwlock_*`, `mtx_*`,
  `uv_mutex_*`, `uv_rwlock_*`, `napi_*threadsafe_function*`, and `napi_throw*` and
  `napi_fatal_*` outside the argument readers). `just lint` fails on any of them, in
  the function's own body or in a function of the package it calls.
- Never throw for an `Offer` code: `tryClaim` and `offer` return the position after
  the frame or the code below zero (`NOT_CONNECTED`, `BACK_PRESSURED`, `ADMIN_ACTION`,
  `CLOSED`, `MAX_POSITION_EXCEEDED`, `ERROR`), as the ABI does; the caller branches.
- Never swallow the handler's exception, or a fragment: a fragment handler that throws
  keeps every later fragment of that poll reaching the handler, a fragment the runtime
  keeps from the handler is counted, and `poll` throws a `HandlerError` once the ABI
  has returned, with the first exception or failure as its `cause` and the counts, the
  one throw the per-message path allows. Never hand a committed log's handler an entry
  after one the runtime kept from it: the rest of that poll is counted too, and
  `HandlerError.undeliveredAt` is where the service opens the log again.
- Never lose a handle the ABI keeps open. Every close frees its handle, whatever it
  returns, so `close()` forgets the handle once the ABI's close has returned. There
  are two exceptions: a subscription's close from inside a handler of its own poll,
  and a committed log's close from inside a handler of its own poll. The ABI refuses
  each with `CAVALRY_ERR_POLLING` and leaves the handle open (`spec/c-abi.md`, "Calls
  during a claim or a poll"). `close()` then throws `CavalryError` and keeps the
  handle, so the subscription or the log can be polled and closed after the poll. The close zeroes the claim struct too, so `commit`, `abort` and `setReservedValue` after it answer -1 with
  `CAVALRY_ERR_NO_CLAIM` and the ABI reads no freed handle through the struct's
  `publication` (`spec/c-abi.md`, "Which claim a struct names").
- Never read a refusal as an answer. A `poll` from inside a handler of the same
  subscription returns −1 with `CAVALRY_ERR_POLLING`, and the poll in progress goes
  on. A committed log's handler is held to the same rule. From inside it, a `poll` of
  the same log, `log.position()` and `log.commitPosition()` return −1, `log.isEnded`
  is `false`, `log.failedAt()`, `log.withdrawnAt()` and `log.faulted()` are `null`,
  and `log.fault()` and `log.close()` throw `CavalryError`, each with
  `CAVALRY_ERR_POLLING`, and the log stays open. The `false` and the three `null` are
  refusals there, not answers: read the four after the poll has returned. The handler
  has each entry's own `position`.
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
measures of it. The figures below are one run's, on the Linux runner that applied the
review of card 206 (Node v22.23.4, npm 10.9.8, Ubuntu 24.04 on four vCPUs of an AMD
EPYC, unpinned); every CI run prints its runners'.

### The call

Each per-message function is one N-API call into the addon, which reads its arguments
without copying, the handle as an external, the claim struct and the offered bytes
through their `ArrayBuffer`'s memory, and makes the one ABI call on the caller's
thread. A struct argument smaller than its struct, or not aligned for it, is refused
with a `RangeError` before the ABI is called, since the ABI reads and writes the whole
struct at the address it is given. A position comes back as a number
(`napi_create_int64`), exact below 2^53 in magnitude, which every position is. The
reserved value is not a position: it is any int64 a publisher sets, such as a
timestamp in nanoseconds or a CRC-64. So `header.reservedValue` comes back as a
`BigInt` ("The call-back, by construction"), and `setReservedValue` takes a `BigInt`
or a number. A `BigInt` carries every int64 exactly, and a number carries those below
2^53 in magnitude. A `BigInt` outside the int64 range is refused with a `RangeError`,
since N-API would otherwise wrap it to another value. Node has one
thread for JavaScript, so a call holds it for the
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
frame's header. The trampoline makes eight N-API calls per fragment, which
`clients/generate-test.py` pins. It opens a handle scope, makes a `Buffer` over the
body's bytes with `napi_create_external_buffer`, no copy, takes the subscription's one
`Header` object from the reference it keeps, reads `undefined` for the handler's
`this`, and calls the handler through `napi_call_function`. Then it detaches the
`Buffer`'s `ArrayBuffer` (`napi_get_typedarray_info`, `napi_detach_arraybuffer`), so
that a kept body reads as empty, and closes the scope. For a committed log's entry it
makes a ninth, `napi_create_int64`, for the number the entry's position is handed to
the handler as, and it asks the ABI for that position with one call of its own per
entry, `cavalry_committed_log_entry_position`.

What Node does for those calls, per received message, as Node v24.x's source has it
(`src/js_native_api_v8.cc`, `src/node_api.cc`, `src/node_buffer.cc`, `src/env-inl.h`):

- On V8's heap: the `Buffer` and the `ArrayBuffer` under it, freed by a collection.
- On the native heap: N-API's wrapper of the handle scope, freed when the scope
  closes; a `BufferFinalizer`; a `CallbackInfo`, which holds a mutex of its own and is
  linked onto the environment's list of objects to clean up at exit; V8's
  `BackingStore` over the log's bytes, with V8's own bookkeeping for the
  `ArrayBuffer`; and a weak global handle to the `ArrayBuffer`.
- At the detach: V8 frees the `BackingStore` at once. Its deleter,
  `CallbackInfo::OnBackingStoreFree`, locks the `CallbackInfo`'s mutex and calls
  `Environment::SetImmediateThreadsafe`. That allocates a native callback, locks the
  environment's `native_immediates_threadsafe_mutex_`, queues the callback, and calls
  libuv's `uv_async_send`. `uv_async_send` writes to the event loop's wake-up file
  descriptor the first time after each turn of the loop; until the loop turns again,
  it only finds the flag that write set.
- When the event loop next turns, Node runs the queued callback. The callback calls
  the addon's finalizer, `buffer_freed`, and frees the `CallbackInfo`, the
  `BufferFinalizer` and itself ("The runtime's queue").
- For a committed log's entry, the position: `napi_create_int64` is
  `v8::Number::New(isolate, (double) value)`. V8 keeps a small integer in the handle
  scope's slot and not on its heap: one below 2^31 on Node's own builds, which do not
  compress V8's pointers, and one below 2^30 on a build that does. From there on it
  makes a `HeapNumber` on V8's heap, freed by a collection. So once a log passes
  2 GiB, each entry makes one more object on V8's heap, the number its position is
  handed to the handler as.

So a received message costs, on the runtime's side, at least five native
allocations, two mutex lock and unlock pairs, and a share of one `write` system call
per turn of the event loop, and a committed log's entry at a position of 2^31 or more
one object more on V8's heap. The mutexes are contended only when another thread
queues work for the same environment at the same moment. None of this is code
Cavalry writes; ADR 0046 leaves it outside the per-message rules, and this README
records it. The addon's own share is one counter increment per `Buffer`, for
`buffersHeld`.

The `Header`'s getters each call the addon's `headerField`, which reads
`cavalry_header_values` once per fragment, on the first getter, into the one
`cavalry_header_values_t` the addon keeps per subscription, and returns the field.
Every field but one is a number, which V8 keeps on the stack for a small integer.
`reservedValue` is a `BigInt` (`napi_create_bigint_int64`), because the field is any
int64 a publisher sets and a number is exact only below 2^53 in magnitude. A `BigInt`
is an object on V8's heap, freed by a collection, and the runtime makes a new one for
the return value on each read of `reservedValue` (ADR 0046, the runtime's side). A
handler that does not read `reservedValue` makes none. Outside the handler call every
getter is `undefined`, since the header is valid for the call alone. The handler is the
poll's own argument: the addon's `poll` keeps its value in the subscription's state for
the call, and N-API keeps that value valid until the call returns, so a poll makes no
reference of its own. The subscription's state, the struct the trampoline works from,
holds the `Header` by a weak reference. The `Header` holds the state through the
external N-API wraps it in, so a strong reference would keep both for the life of the
process. Once the program drops a `Subscription`, a collection frees its `Header` and
the external, and the external's finalizer frees the state. The runner checks this
with a `WeakRef` to each of a closed subscription's `Header` and state.

### The view over a claimed slice, by construction

`publication.claim` is a new `Buffer` per claim, over the bytes the ABI reserved,
detached by `commit`, `abort` and `close`: V8 has no view that can be moved to a new
address, so the one object per publication is the claim struct, a 32-byte
`ArrayBuffer` the addon fills, and the view is the client's own code's allocation per
sent message, two objects, the `Buffer` and its `ArrayBuffer`, recorded here with that
reason. The runtime's side of the view is the call-back's without the handle scope,
which the addon's `view` does not open, since it returns to JavaScript at once. That is
a `BufferFinalizer`, a `CallbackInfo` with its mutex, a `BackingStore` and a weak
global handle at the claim. At the `commit` or `abort` that detaches the view, it is
the queued callback, the same two locks, and a share of the same wake-up `write`.

A runtime built with V8's sandbox, such as Electron, refuses external buffers:
`napi_create_external_buffer` returns `napi_no_external_buffers_allowed`, and the
addon's `view` returns `null`. On such a runtime `tryClaim` returns the position
after the frame while `publication.claim` is `null`, and the claim stays
outstanding. Call `publication.abort()`, which turns the frame into padding that a
subscriber skips, and publish the message with `offer`, which reads the caller's own
bytes and needs no external `Buffer`. Such a runtime cannot receive either: every
fragment a poll consumes is counted in `HandlerError.undelivered` ("The exception").
So run the client on a Node built without the sandbox.

### The runtime's queue

Node frees the native objects of an external `Buffer` when its event loop turns, not
at the detach and not at a collection: the queued callback runs on the next turn.
Every message holds two `Buffer`s until then, the view of its claim and the body its
handler received. A program that publishes or polls in one synchronous loop holds
every one of them until it returns to the event loop, however many messages that is.
`cavalry.buffersHeld()` counts them on the calling thread: the addon counts each
`Buffer` it makes, and its finalizer counts each one Node frees.

So a program that runs for longer than a burst lets the event loop turn. It publishes
and polls in batches and returns to the loop between them, for example by scheduling
its next batch with `setImmediate`, as `dutyCycle` in
`conformance/node/quickstart.js` does. A program that must spin without returning
runs the client on a `worker_threads` worker that returns to its own loop between
batches.

The runner shows both cases. Its allocation round sends 10,000 messages without a
turn. On this Linux runner it held 20,000 `Buffer`s after the round, still 20,000
after a full collection, and 0 after one turn of the event loop. Its duty cycle then
runs ten turns of 1,000 messages each, returning to the loop with `setImmediate`
after each. It held 0 `Buffer`s at the start of every turn and 2,000 at the end of
each, and it fails if they ever pass two turns' worth, 4,000. In bytes,
`process.memoryUsage().external` and `arrayBuffers` do not count these objects on
Node v22.23.4. `rss` grew by 275 bytes per message over the allocation round. That
figure also counts the log's own pages and V8's heap, and it does not fall after the
turn, since malloc keeps freed memory for its next allocations rather than giving it
back. The runner prints all three readings before the round, after it, after a full
collection and after the turn.

### The count, measured

The runner's allocation round sends 10,000 messages whose handler only counts and whose
publisher copies one constant, with V8's sampling heap profiler on for that round
alone, the objects a collection freed included, and a full collection before and after
(`node --expose-gc`). On this Linux runner: 458 bytes of V8's heap allocated per
message, an unbiased estimate from samples every 64 bytes, which is the four objects
of V8's heap above, a `Buffer` and its `ArrayBuffer` on each side, and what V8 makes
around a call; and 2.6 bytes kept per message after the
collection (`process.memoryUsage().heapUsed`, nothing read between), so what a message
makes on V8's heap is freed by the next collection. Neither figure sees the native
heap: what a message holds there is counted in `Buffer`s and freed by the next turn
of the event loop ("The runtime's queue").

### The latency, measured

The runner's latency round sends 100,000 messages over `cavalry:ipc`, one at a time on
one thread, with the profiler off. The publisher writes `process.hrtime.bigint()` into
the claim before its commit, and the handler reads the clock first. On this Linux
runner: p50 661 ns, p99 1,493 ns, p99.99 53,330 ns (10 samples above it, nearest
rank), max 124,544 ns. The `BigInt` each side makes for the clock is the round's own
cost, not the client's. The round does not return to the event loop for its 100,000
messages, so it holds 200,000 `Buffer`s by its end ("The runtime's queue"); the
allocation round, which runs last, starts on a turn of its own.

### The exception

The trampoline sees the handler's exception as N-API's pending exception
(`napi_get_and_clear_last_exception`), keeps the first, counts every one, and goes on
calling the handler for every later fragment of the poll, so that no fragment is
consumed unseen; `poll` throws `HandlerError` once the ABI has returned, with
`fragments`, `raised`, the first exception as its `cause`, and a message that says
what to do. `fragments` is the poll's result: the count, or −1 when the poll also
stopped on a fault, which `fault()` reads. A committed log's `HandlerError` has
`committedLog` true and speaks of entries, and when `fragments` is −1 it names
`log.fault()`, to read before the next poll, and `log.faulted()`, which keeps the
fault of the log's own image ("The committed log"). A value that is
not an `Error`, a string or a number, is kept as thrown, and stays the first when an
`Error` follows it in the same poll. The trampoline keeps
the first exception as element 0 of a holder object the subscription makes once, when
it is added, and clears the element when the poll returns. A reference to the value
itself would not do: at the N-API version the addon builds against, 8,
`napi_create_reference` refuses a value that is not an object, a function or a symbol.

The ABI has consumed a fragment before it calls the trampoline, so a fragment the
trampoline cannot hand to the handler is counted and reported, never dropped. That
happens when N-API refuses a step of the call: the handle scope, the body's `Buffer`,
the read of the `Header` or of the handler, or the call itself. One documented case is
a runtime built with V8's sandbox, such as Electron, on which
`napi_create_external_buffer` returns `napi_no_external_buffers_allowed`. The
trampoline counts such a fragment in `undelivered` and keeps the first failure: the
N-API call, its status, and the frame's session id and term offset, read through
`cavalry_header_values`, which needs no N-API. `poll` then throws `HandlerError` with
`undelivered` above 0. Its `cause` is an `Error` whose message names the call, the
status and the frame and says what to do, with `napiStatus`, `sessionId` and
`termOffset` as its properties. The `cause` is the poll's first failure or first
exception, whichever came first, and `raised` and `undelivered` count every one.

A committed log's poll differs in one way, because a service applies its entries in
order. Once the runtime has kept an entry from the handler, the trampoline hands the
handler no later entry of that poll. It counts each of them in `undelivered` and makes
no N-API call for them. The trampoline keeps the first such entry's position, which
`cavalry_committed_log_entry_position` gives without N-API, and `HandlerError` reports
it as `undeliveredAt`. A committed log's `Error` for a failure names its entry by
position, as its `position` property too, and when an entry of the poll did not reach
the handler its message says to close the log and open a new one at `undeliveredAt`
("The committed log").

`clients/generate-test.py` compiles the generated trampoline against a stand-in for
N-API that fails each of its calls in turn, and checks that every fragment the handler
did not return from is counted once, with the first failure kept. For a committed log
it checks that the position kept is the one the ABI gave, and that a later entry of
the same poll makes no N-API call and does not reach the handler. It compiles the
addon's `failure_error` and `report` too, and checks the position each reports and the
cause's message.

## What to read next

- `spec/c-abi.md`: the page the package wraps, function for function.
- `clients/README.md`: how a client is generated and what every client passes.
- `clients/python/README.md`: the first client, whose shape this one copies.
- `conformance/node/`: the runner and its report.
- `docs/decisions/0046-build-the-c-abi-and-the-clients-fourth.md`: the slice this
  client is the third feature of.
