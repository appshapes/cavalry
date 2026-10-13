# 0021. Shape the client API after Aeron's, with a conductor thread and no lock

- **Status:** Proposed
- **Date:** 2026-10-13
- **Invariants affected:** 1, 2, 3, 4, 5, 6
- **Supersedes:** none

## Context

Feature 5 of the first slice (ADR 0007) is the client API in `cavalry-client`,
re-exported by `cavalry`, the ten-minute path and `just quickstart`, the slice's
acceptance test. Everything a client does is specified: `spec/client-protocol.md` says how
it attaches, what it sends and what it receives (ADR 0019); `spec/log-buffer.md` says how
it claims, commits, offers and reads (ADR 0013); ADR 0013 gives the subscriber its
`subscription.position` counter; ADR 0020 gives it `CAVALRY_DIR`. What is not decided is
the shape of the Rust API a user sees, and how a client that must send a keepalive every
500 ms and read events while the user's thread publishes and polls is built without a lock
on the data plane: `cavalry-client` is a data-plane crate, so the workspace lint denies
`Mutex`, and the per-message checklist (`.claude/rules/data-plane.md`) denies allocation,
syscalls and panics on the path from `try_claim` to the fragment handler.

Aeron's client (`io.aeron.Aeron`, `ClientConductor`, `Publication`, `Subscription`,
`Image`, `BufferClaim`, read from the Java source on 2026-10-13) answers the shape: a
client conductor thread (an `AgentRunner`, or the user's own invoker) sends keepalives,
reads the broadcast buffer, completes `addPublication` and `addSubscription` under a
client lock, and swaps a subscription's image array copy-on-write; `offer` and `tryClaim`
return negative result codes `NOT_CONNECTED`, `BACK_PRESSURED`, `ADMIN_ACTION`,
`CLOSED`, `MAX_POSITION_EXCEEDED`; `poll(handler, fragmentLimit)` hands each fragment to a
`FragmentHandler(buffer, offset, length, header)`. The lock sits on the add and remove
path, never on the message path, and the GC frees retired image arrays.

## Decision

The client API is Aeron's, with Aeron's names, in Rust, and its two threads share
nothing that needs a lock. `Cavalry::connect()` attaches to the driver at `CAVALRY_DIR`
or the default directory (ADR 0020), `Cavalry::connect_to(dir)` to a named one: it opens
the command ring writable, the broadcast buffer read-only, and the counters file writable,
so that the subscriber can store its own `subscription.position` (ADR 0013;
`spec/counters-file.md`, "Value slot": a value is written by its owner) through
`cavalry_shm::counters::CountersWriter`, which never allocates or frees a record; takes
its client id from the ring's correlation id counter, and starts the client conductor
thread, which every 500 ms sends `client keepalive`, on every cycle reads the broadcast
buffer and the consumer heartbeat, completes pending operations, applies image events,
and closes the client on `driver-timeout`, `driver-closed` or `client timeout`, with the
backoff idle strategy. `add_publication(channel, stream_id)`,
`add_exclusive_publication` and `add_subscription` take `&mut self`, write the command
and wait for the answer or the 10 s response timeout, which is an error, never a hang;
one operation is in flight at a time per client, so the hand-off is one slot of atomics
the conductor fills with a release store. `Publication::try_claim(length) ->
Result<BufferClaim, Offer>` and `offer(&[u8]) -> Result<i64, Offer>` return Aeron's codes
as `Offer::{NotConnected, BackPressured, AdminAction, Closed, MaxPositionExceeded,
TooLong}`; `BufferClaim` writes in place and `commit`s or `abort`s; `position`,
`is_connected`, `close`. `Subscription::poll(handler, fragment_limit) -> usize` hands
each fragment to `FnMut(&[u8], &Header)` across the subscription's images in turn,
storing each image's `subscription.position` with a release store after its poll;
`image_count`, `images`, `close`. `Image` has `session_id`, `position`,
`is_end_of_stream`, `source_identity`. Every type is `Send`; a `Publication` or
`Subscription` is used from one thread at a time, which the borrow checker enforces.

Images cross from the conductor to the user's thread without a lock: the conductor
opens an image's log buffer when `available image` arrives and sends the mapped handle
through a bounded single-producer single-consumer queue the subscription drains at the
start of `poll`; an `unavailable image` goes the same way, and the user's thread returns
the mapping through a second queue so that the conductor, never the poll, unmaps it.
Both queues are `cavalry_shm::spsc`, an in-process SPSC queue in the crate where
`unsafe` is allowed, with a `loom` model and Miri, capacity fixed at the subscription's
maximum images; when the queue is full the conductor keeps the event in its own bounded pending
list and counts `client.image_events_pending`, and past that list the client closes with
`client-overrun`, counted, since a client that cannot keep its images is lying about
what it will deliver (invariant 1 over 3). A dropped `Subscription` drops its halves of
both queues, and the conductor learns of it with no message between the threads: on each
cycle it asks its own halves `peer_dropped()`, which is true once the other half is gone,
and sweeps that registration, unmapping its images and sending `remove subscription`.
Nothing on the poll or publish path allocates,
locks, opens, unmaps, panics or logs: the one clock the conductor reads is its own.

## Alternatives considered

- **An invoker mode only: the user's thread drives the conductor.** Lost for the default:
  a quiet client that forgets to call `do_work` is timed out by the driver after 10 s and
  told so, which a stranger on the ten-minute path would hit. Aeron offers both; the
  invoker returns when a user asks for it, as a Revisit.
- **A lock on the add and remove path, as Aeron's client has.** Lost: the workspace lint
  denies `Mutex` in the crate, and one operation in flight per `&mut self` makes the
  slot of atomics enough; a second thread wanting its own operations takes a second
  `Cavalry`.
- **Copy-on-write image arrays, as Aeron's, with the user's thread dropping the old one.**
  Lost: the drop that unmaps a log would land on the poll path when the user's thread
  holds the last reference; the two queues give the unmap back to the conductor.
- **`crossbeam` for the queues.** Lost: a dependency in a data-plane crate needs an ADR
  and an allowlist entry for two bounded SPSC queues that `cavalry-shm` can hold in a
  hundred lines with a `loom` model, under the same rules as its rings.
- **Result codes as negative integers, as Aeron's.** Lost: a Rust `Result` with a named
  enum cannot be ignored by mistake, and `?` carries it; the names stay Aeron's.
- **A callback for events (`AvailableImageHandler`).** Deferred: the queues carry the
  events; a handler the conductor calls is a Revisit when a user needs to act on an
  image before the next poll.

## Evidence

None measured yet: a shape decision. What will accept it: the `cavalry-client` README
example as its doctest, the end-to-end tests through a real driver, the allocation count
of zero per message on the client path in `just bench`, and `just quickstart` on both
runners. The bench row `one_way_32_client` records what the public API costs over the
raw log buffer on the bench host.

## Consequences

Easier: a user who knows Aeron reads the API in minutes, and a user who does not reads
the ten-minute path; the C ABI in `cavalry-ffi` wraps the same handles; no lock anywhere
on the data plane. Harder: a second SPSC primitive in `cavalry-shm` to model and keep;
one operation in flight per client; the image queues' capacities are fixed. To maintain:
the conductor's state machine and its tests against the driver.

Revisit: when a user needs the invoker mode or an image handler; when UDP adds channel
status and destinations to publications and subscriptions; when the C ABI wraps these
handles, which fixes their layout; when a client needs counters of its own (ADR 0019's
Revisit).

## References

- `spec/client-protocol.md`, `spec/log-buffer.md` ("Claim, commit and abort",
  "Reading"); ADR 0007, 0013, 0019, 0020
- `crates/cavalry-client/README.md`, the example; `crates/cavalry-driver/tests/driver.rs`,
  the hand-written client the tests used before this crate;
  `crates/cavalry-shm/src/spsc.rs`, the queue
- Aeron, `aeron-client/src/main/java/io/aeron/{Aeron,ClientConductor,Publication,
  ExclusivePublication,Subscription,Image,BufferClaim}.java`, read 2026-10-13
