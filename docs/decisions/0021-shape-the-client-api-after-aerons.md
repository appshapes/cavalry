# 0021. Shape the client API after Aeron's, with a conductor thread and no lock

- **Status:** Accepted
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
and closes the client on `driver-closed`, `driver-timeout`, `client-timeout`, `lapped`
when the broadcast buffer overwrote events it had not read, `corrupt` when the ring, the
buffer or an event fails a check, or `overrun` (below), with the backoff idle strategy;
`Cavalry::closed()` says which. `add_publication(channel, stream_id)`,
`add_exclusive_publication` and `add_subscription` take `&mut self`, write the command
and wait for the answer or the 10 s response timeout, which is an error, never a hang;
one operation is in flight at a time per client, so the hand-off is one slot of atomics
the conductor fills with a release store. `Publication::try_claim(length) ->
Result<BufferClaim, Offer>` and `offer(&[u8]) -> Result<i64, Offer>` return Aeron's codes
as `Offer::{NotConnected, BackPressured, AdminAction, Closed, MaxPositionExceeded,
TooLong}`; `BufferClaim` writes in place and `commit`s or `abort`s; `position`, `limit`,
`max_payload_length`, `max_message_length`, `is_connected`, `close`.
`Subscription::poll(handler, fragment_limit) -> Result<usize, Fault>` hands each
fragment to `FnMut(&[u8], &DataFrameHeader)`, the body and the wire header of ADR 0008 as
a view into the log, across the subscription's images in turn, storing each image's
`subscription.position` with a release store after its share. The count is what the
poll delivered; a `Fault` is what stopped it: `Image` when a frame or a join position
failed a check of `spec/log-buffer.md`, which retires that image and delivers nothing
from it on, `ImagesFull` when the driver announced an image past the 256 a subscription
holds, which is retired unread, and `Closed` once the client is closed. What a faulted
poll handed over before the fault was delivered and is not counted again: a subscriber
is told, never left to count (invariant 1). `image_count`, `images`, `is_connected`,
`close`. `Image` has `session_id`, `position`,
`is_end_of_stream`, `source_identity`. Every type is `Send`; a `Publication` or
`Subscription` is used from one thread at a time, which the borrow checker enforces.
The conductor's registry holds as many subscriptions as the driver allows one client;
when it is full, `add_subscription` returns `ClientError::SubscriptionsFull` and drops
the one the driver had accepted, which sends `remove subscription`, so nothing is left
half added.

Images cross from the conductor to the user's thread without a lock: the conductor
opens an image's log buffer when `available image` arrives and sends the mapped handle
through a bounded single-producer single-consumer queue the subscription drains at the
start of `poll`; an `unavailable image` goes the same way, and the user's thread returns
the mapping through a second queue so that the conductor, never the poll, unmaps it.
Both queues are `cavalry_shm::spsc`, an in-process SPSC queue in the crate where
`unsafe` is allowed, with a `loom` model and Miri; each holds 64 events, and a
subscription holds at most 256 images (`MAX_IMAGES`). When the image queue is full the
conductor closes the client with `overrun`: a poll drains the queue, so 64 unread events
mean the subscription has not polled through 64 image changes, and a client that cannot
keep its images is lying about what it will deliver (invariant 1 over 3). The close is
not counted, because a client has no counter of its own until ADR 0019's Revisit adds
`add counter`; `Cavalry::closed()` reports it and the error says what to do. A dropped
`Subscription` drops its halves of
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
- **`poll` returning the count alone, as Aeron's.** Lost at acceptance: a frame that
  fails a check retires its image, which Aeron reports through an error handler the
  conductor calls; this client has no handler (deferred above), so the poll returns the
  fault to the thread that must act on it, and the count when nothing stopped it.
- **`Header` as the handler's second argument, Aeron's name.** Lost at acceptance: the
  bytes the handler sees are the data frame header ADR 0008 adopted,
  `cavalry_proto::frame::DataFrameHeader`, re-exported; one name for one layout beats a
  second name for the same bytes.
- **A bounded pending list in the conductor before closing on a full image queue**, as
  this ADR proposed. Lost at acceptance: the list defers the same close by its length,
  and the counter it would keep needs a client counter the protocol does not have yet
  (ADR 0019's Revisit); the queue's 64 events already outlast a subscription that polls,
  and one that does not is closed the sooner.

## Evidence

Accepted on 2026-10-13 against what the proposal named, all on `master`. The
`cavalry-client` README example is its doctest and the ten-minute path's program
(commit fd208d6). Fourteen end-to-end tests run against a real driver: five attach and
close (70792e9); five publish, back-pressure at exactly the publication window among
them, and four subscribe, 100,000 messages once and in order, a late joiner, a slow
subscriber and images that come and go (fd208d6). `just quickstart` passes on both
runners in 1 to 2 s (2a3d9cb). On the bench host, run 38343347725 of commit 767d581,
twenty-one runs of 5,000,000 messages per row and the median run by p99.99, counted 0
allocations in 315,000,000 measured messages and measured the public API against the
raw log buffer, in nanoseconds:

| Row | p50 | p99 | p99.99 | max |
| --- | --- | --- | --- | --- |
| `claim_commit_32_exclusive` | 130 | 200 | 420 | 970 |
| `claim_commit_32_client` | 150 | 220 | 460 | 1,160 |
| `one_way_32_exclusive` | 230 | 300 | 660 | 8,711 |
| `one_way_32_client` | 250 | 330 | 670 | 1,820 |

The conductor thread, the response slot, the two queues and the release store of the
position cost 10 to 20 ns at p50 and p99 and 10 to 40 ns at p99.99 over the raw log.
The rows joined the bench host's baseline in f98b1c8, so the gate holds the client path
to the budget from here on.

## Consequences

Easier: a user who knows Aeron reads the API in minutes, and a user who does not reads
the ten-minute path; the C ABI in `cavalry-ffi` wraps the same handles; no lock anywhere
on the data plane. Harder: a second SPSC primitive in `cavalry-shm` to model and keep;
one operation in flight per client; the image queues' capacities are fixed. To maintain:
the conductor's state machine and its tests against the driver.

Revisit: when a user needs the invoker mode or an image handler; when UDP adds channel
status and destinations to publications and subscriptions; when the C ABI wraps these
handles, which fixes their layout; when a client needs counters of its own (ADR 0019's
Revisit), which is when an overrun is counted and not only reported by `closed()`.

## References

- `spec/client-protocol.md`, `spec/log-buffer.md` ("Claim, commit and abort",
  "Reading"); ADR 0007, 0013, 0019, 0020
- `crates/cavalry-client/README.md`, the example; `crates/cavalry-driver/tests/driver.rs`,
  the hand-written client the tests used before this crate;
  `crates/cavalry-shm/src/spsc.rs`, the queue
- Aeron, `aeron-client/src/main/java/io/aeron/{Aeron,ClientConductor,Publication,
  ExclusivePublication,Subscription,Image,BufferClaim}.java`, read 2026-10-13
