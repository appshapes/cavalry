# 0046. Build the C ABI and the language clients fourth, in six features

- **Status:** Proposed
- **Date:** 2026-11-05
- **Invariants affected:** 1, 2, 3, 5, 6
- **Supersedes:** none; ADR 0039's Revisit named the fourth slice as Raft or the C ABI
  and the language clients, and this ADR is the one it asked for

## Context

The third slice is done. ADR 0039's Revisit clause fired on 2026-11-04: card 182
closed and was archived with its eight commits on `master` (1db07a2 to 61fbfdd);
`bench (udp)` is a required status check of `master` beside `gates (linux)`,
`gates (macos)`, `bench (linux)` and `bench (grip)`, read back from the branch
protection on 2026-11-05; and `just quickstart` publishes the ten-minute path's stream
over UDP on both runners since 56a4f0a, with `scripts/quickstart.sh` checking the line.
`master` at 61fbfdd is green (run 41899418656, all six jobs), every recipe in the
`justfile` is real, and nothing prints `not built yet`. The clause names the fourth
slice as "Raft or the C ABI and the language clients". Both lost twice before, each
time for a reason that has since run out. ADR 0007 left the clients out because "the C
ABI cannot be undone, so it follows a client API that has been used, not precedes it",
and Raft because it sits on a stream and no stream existed. ADR 0039 left the ABI out
because UDP would change the client API it wraps, with the channel status and the
source identity, and Raft because replication between hosts needs a transport between
hosts and "nothing of it can be gated before a frame has crossed a wire". The API has
been used, by the ten-minute path, the bench's rows, the soak program and the client's
nine test binaries, and a frame has crossed a wire. This ADR picks.

What the repository promises each candidate. The ABI: `CLAUDE.md`'s map gives
`cavalry-ffi` the `extern "C"` ABI and its cbindgen header, on the data plane, the
other crate where `unsafe` is allowed, and `clients/` the "thin clients generated over
the C ABI: python, node, dotnet, jvm, go"; its gate for invariant 1 says
"`conformance/` runs every client against `spec/`"; its rule says clients are "thin
wrappers over `cavalry-ffi`, generated where possible, released together, all passing
`conformance/`"; its Unsafe rule says FFI buffer lifetimes are by convention, so
debug builds poison released buffers; and it defines the data plane, where invariant 2
forbids a heap allocation per message, as "any code a message passes through between
`try_claim` on the publisher and the fragment handler on the subscriber", which does
not say where that path ends in a client written in another language.
`crates/cavalry-ffi/README.md`: one Rust core serves every language through a
generated header; every non-Rust client's messages cross this boundary, which is why
it is on the data plane; it is "the one surface frozen between releases"; it never
changes without an accepted ADR, never behaves differently from the Rust client, and
never allocates per message; its example is "Not yet".
`docs/explanation/why-cavalry.md`: "One core, every language", whose thin clients
were Python through PyO3 and Node through napi-rs beside .NET through P/Invoke, the
JVM through Panama FFM and Go through cgo, until this ADR's commit (Consequences);
"Stable ABI only at the C boundary"; the FFI-lifetimes risk; and, of Aeron's clients, Java
first-class, C and C++ in the main repository, .NET in a separate Adaptive repository,
Rust only as community wrappers over the C client, "No Python, TypeScript or Go", with
the beachhead "every team that evaluated Aeron and chose Kafka, NATS or gRPC". Raft:
`CLAUDE.md`'s first sentence, "a durable log and Raft replication";
`crates/cavalry-raft/README.md`: a durable log replicated across a cluster of drivers,
a deterministic state machine fed from the committed log, a leader elected in under a
second, mixed versions in a rolling upgrade, standby members in the open core, the
whole crate under the data-plane rules until a decision splits the control parts out;
the glossary's rows for cluster and standby, which point at a reference page that does
not exist; and `why-cavalry.md`'s "Archive plus Cluster is the real product" and "the
differentiators are the durable log plus Raft story, the operator experience and the
open core". Neither crate has a public item. `clients/` and `conformance/` do not
exist as directories, and `just conformance` runs eight Rust vector suites. Three
Revisit clauses name the ABI: ADR 0021's, "when the C ABI wraps these handles, which
fixes their layout"; ADR 0019's, client counters through `add counter`, for later; and
ADR 0025's, the client's rule on an unknown event type, "before the first release".

The client API the ABI would wrap, as it stands at 61fbfdd. `cavalry-client` exports
`Cavalry` (connect, connect to a directory, add a publication, an exclusive publication
or a subscription, start and stop a recording, truncate, purge and delete one, start
and stop a replay, close, and why it closed), `Publication` (`try_claim`, `offer`,
position, limit, connected, close) with `BufferClaim` (commit, abort), `Subscription`
(`poll` with a handler over the body and the data frame header, the images, connected,
close), `Image` (session id, position, end of stream, source identity, the position
arithmetic), `Offer` (five of Aeron's codes, `NotConnected`, `BackPressured`,
`AdminAction`, `MaxPositionExceeded` and `Closed`, as ADR 0021 lists them, and a
sixth variant, `TooLong`, which is not one of Aeron's codes), `Fault`'s three,
`Recording`, `Replay`, `DIR_ENV` and `MAX_IMAGES`. Since ADR 0021 shaped it on
2026-10-13 the API
has gained recording (ADR 0025), replay (ADR 0030), retention (ADR 0031), the
`cavalry:udp` channel with the channel status and the source identity (ADR 0040) and
replay over UDP (ADR 0043), and the ten-minute path runs the same program over shared
memory and over UDP with nothing changed but the channel. Nothing on the list is
waiting for a transport to change it: what Raft adds is a client of its own on top,
below.

What Aeron does, read on 2026-11-05. Its C client (`aeron-client/src/main/c/aeronc.h`
at `master`) is a context (`aeron_context_init`, its setters, `aeron_context_close`), a
client (`aeron_init`, `aeron_start`, `aeron_close`, `aeron_is_closed`), asynchronous
adds polled to completion (`aeron_async_add_publication` and `_poll`, the same for
exclusive publications and subscriptions, a subscription's add taking available-image
and unavailable-image callbacks), `aeron_publication_offer` and
`aeron_publication_try_claim` with `aeron_buffer_claim_commit` and
`aeron_buffer_claim_abort`, `aeron_subscription_poll(subscription, handler, clientd,
fragment_limit)` with `typedef void (*aeron_fragment_handler_t)(void *clientd, const
uint8_t *buffer, size_t length, aeron_header_t *header)`, per-image polls,
`aeron_image_position` and `aeron_image_is_end_of_stream`; offer and try-claim return
the new position or one of six negative values, `NOT_CONNECTED` −1, `BACK_PRESSURED`
−2, `ADMIN_ACTION` −3, `CLOSED` −4, `MAX_POSITION_EXCEEDED` −5 and `ERROR` −6, the
first five of them `Offer`'s five and `ERROR` with no `Offer` variant, while
`Offer::TooLong` has no code of Aeron's; `aeron_errcode()` and `aeron_errmsg()` give
the calling thread's last error; and
`aeron_version_major`, `_minor`, `_patch`, `_full`, `_text` and `_gitsha` give the
version. That C client re-implements the Java one; Aeron.NET is "A .NET port of the
Aeron Client" that ships the Java media driver in a NuGet package (its README); and
rusteron-client 0.2.19 "wraps the Aeron C client API", generated by a code generator
over the C bindings, with callback arguments borrowed for the duration of the call
(docs.rs). Aeron Cluster (the Cluster Tutorial wiki page, `AeronCluster.java` and
`ConsensusModule.java` at `master`): a node runs four components, the media driver,
the Archive, which persists the Raft log, the consensus module and the clustered
service container; the tutorial starts "with a static three-node cluster", available
while `(n / 2) + 1` nodes remain; a member is five endpoints, ingress, consensus, log,
catch-up and archive, and the members list has no default, since `conclude()` throws
without one; the leader heartbeat timeout is 10 s at an interval of 200 ms, the
election timeout 1 s, the startup canvass 60 s; and the consensus module's log channel
defaults to `aeron:udp?term-length=64m` with no endpoint, which it treats as
multi-destination cast. Its client, `AeronCluster`, takes an `Aeron` client in its
context or connects one, opens an egress subscription and an ingress publication, one
per member when the ingress endpoints are listed, and offers and claims through them
with a session header prepended: a session protocol of connect, challenge, keepalive
and new-leader events over publications, subscriptions and images, and no transport
primitive of its own.

The hosts. ADR 0044's Consequences: every isolated core of both bench hosts is taken by
the UDP rows, and a fifth measured thread needs a host change. `kip` is the soak host
(ADR 0034) and carries card 159's week-long run, which has not run yet; card 159's
brief says the soak host "joins the cluster slice later as its third member, which is
a card of its own when that ADR is written". A cluster is three members. A cluster of
three drivers on one host's loopback runs anywhere, as the UDP tests run two endpoints
on one host; a gate across hosts, which is the promise a cluster makes, has no third
host today, as UDP had no second host when ADR 0023 held it for the slice after.

## Decision

The fourth slice is the C ABI and the five language clients over it, on the client API
as it stands: one Rust implementation of the client protocol, the log buffer's claim
protocol and the image's reader, exposed by `cavalry-ffi` as an `extern "C"` ABI with
Aeron's C client's names and result codes where the concept matches (`CLAUDE.md`,
Protocol), and thin generated clients for Python, Node, Go, the JVM and .NET, each
passing one conformance runner on both runners, released together. Raft is the fifth
slice. Six features make the slice, one Trello card each, in this order:

1. **The C ABI, specified first (card 204).** Its ADR: the handle types, context,
   client, publication, exclusive publication, subscription, image, buffer claim,
   recording and replay; the functions over `cavalry-client`'s API under Aeron's C
   names, `cavalry_publication_try_claim`, `cavalry_buffer_claim_commit`,
   `cavalry_subscription_poll` with a fragment handler of the shape above, and the
   rest; Aeron's negative result codes, −1 to −6, with `Offer`'s five Aeron variants
   on −1 to −5 and `TooLong`'s code decided by this card (below); `cavalry_errcode`
   and `cavalry_errmsg` per thread; `cavalry_version_*` and how a caller checks the
   header's version; the lifetime rules, who frees what, a claim committed or aborted
   once, a released buffer poisoned in debug builds; and the thread rules, a
   publication or a subscription from one thread at a time, as the Rust API. The ADR
   lands Proposed and Rjae accepts it, because the ABI is the one surface frozen
   between releases (`CLAUDE.md`, When in doubt), as ADR 0040 did for the frames.
   Nothing is released (ADR 0003), so a client that later finds the ABI wanting sends
   it to a new version under a new ADR before the first release; the first release is
   what freezes it. `cavalry-ffi` gains the functions, every `unsafe` block with its
   `// SAFETY:` comment, the header generated by cbindgen and committed at
   `crates/cavalry-ffi/include/cavalry.h`, with `just lint` failing when the committed
   header differs from the generated one, the generator named and justified as a
   dependency in the ADR (ADR 0003's allowlist, ADR 0041's shape), and the README's
   example, a Rust doctest calling the ABI, in the commit with the first public item.
   It builds `conformance/`, the directory gate 1 names, with its first runner,
   `conformance/c/`: a C program that runs the ten-minute path through the header
   against a driver, publish and subscribe over `cavalry:ipc` and `cavalry:udp`,
   record and replay, every message once and in order, compiled with the runner's C
   compiler and run by `just conformance` after the Rust vector suites on both
   runners. It builds two rows through the ABI in `just bench`, `claim_commit_32_ffi`
   and `one_way_32_ffi`, gated on both bench hosts against baselines recorded as
   ADR 0033 says, and the allocation assertion over the ABI's message path (gate 2).
2. **The Python client (card 205).** `clients/python/`: the package generated from the
   committed header by a generator under `clients/` that the four cards after it
   reuse, with the idioms a Python user expects over the ABI's handles, a context
   manager that closes a client, a callable fragment handler, a writable view over a
   claimed slice; its README answering the four questions with the ten-minute path's
   program in Python as the example; and `clients/README.md`, once, saying how a
   client is generated, what every client must pass, and that the clients are
   released together. It builds the per-client runner shape, `conformance/python/`:
   the README's example extended with the checks, publish and subscribe over both
   channels, record and replay, every message once and in order, the handler's header
   fields against `spec/frame.md`'s vectors, and a report printed and never gated: its
   one-way p50, p99.99 and max, and the allocations per message on its runtime's side
   of the boundary (below). The card records how `ctypes` calls the fragment handler
   back and what that allocates per message: a `ctypes` callback builds its arguments
   as new Python objects on every call, and a view over a claimed slice is a new
   object per claim. Python first because both runners have Python 3 for the scripts
   (ADR 0003), `ctypes` is in its standard library so the client has no dependency,
   and Aeron has no Python client.
3. **The Node client (card 206).** `clients/node/` with TypeScript types,
   `conformance/node/`, and Node on both runners through its set-up action pinned to
   a full commit SHA (ADR 0003). How the client reaches the header, a native addon
   over N-API that calls the header or a foreign-function package, is the card's
   decision, recorded in its commit with the reason, and under the allowlist rule if
   it is a dependency; napi-rs over the Rust crates is not one of the two
   (Alternatives); the
   card records how the fragment handler is called back and what that allocates per
   message.
4. **The Go client (card 207).** `clients/go/` over the header through cgo,
   `conformance/go/`, and Go on both runners the same way; cgo's pointer rules decide
   what crosses the boundary, and the card records how the fragment handler is called
   back and what that allocates per message.
5. **The JVM client (card 208).** `clients/jvm/` through Java's foreign function and
   memory API, no JNI, `conformance/jvm/`, and a JDK on both runners the same way;
   the card records the Java release it needs, and how an upcall for the fragment
   handler is made and what that allocates per message.
6. **The .NET client (card 209).** `clients/dotnet/` through P/Invoke with the
   source-generated marshalling, `conformance/dotnet/`, and the .NET SDK on both
   runners the same way; the card records how the handler delegate is called back
   and what that allocates per message.

Each of the four runners after Python's has the shape card 205 builds, its report
included.

Where invariant 2's per-message rules stop, one rule for all five clients. For a
client over the ABI, the data plane those rules hold is the Rust core behind the ABI:
each function a message passes through, `cavalry_publication_try_claim`,
`cavalry_buffer_claim_commit`, `cavalry_publication_offer` and
`cavalry_subscription_poll`, from its call to its return, except the time inside the
C fragment handler the poll was given. That path makes no heap allocation, takes no
lock, makes no syscall, has no `await` and no panic path per message; card 204's
allocation test over the ABI's path asserts it, and its two bench rows gate it.
Outside those rules and outside their
gate is the runtime's side of each call: the language's trampoline, the objects it
makes for the arguments, and the client's code between the user's code and the ABI,
in both directions. Every client's card measures that side with what its runtime
counts, allocations or bytes allocated per message; takes the call-back with the
fewest allocations its runtime offers; records the call-back and the count in its
commit and its README; and its runner prints the count in the report beside the
one-way p50, p99.99 and max, which nothing gates. A card whose runtime cannot call
the handler back without allocating, as Python's `ctypes` cannot, still lands, with
its count on record: it does not hold the slice, add a knob, or move its client to a
re-implementation of the protocol. Three reasons, given again in the alternative that
lost: the allocation test counts Rust's global allocator and sees no runtime's heap,
so a rule on the runtime's side would have no gate; the allocator, the collector and
the interpreter belong to the runtime the user chose, and nothing in Cavalry removes
the tail they add; and a rule that every client's side allocates nothing rules out
Python, which `CLAUDE.md` names first among the clients. Card 204 writes this
boundary into `CLAUDE.md`'s definition of the data plane and into
`crates/cavalry-ffi/README.md`, in the commit with the first public item.

The clients' order: Python first, for the reasons on its card; Node and Go next, the
other two languages Aeron leaves open and the beachhead's; the JVM and .NET last,
where Aeron already serves a client and the toolchains are the heaviest to add to the
gates. The slice's acceptance test is `just conformance` running the C program and
the five clients on both runners and passing; the card that lands last changes the
README's status paragraph. The README's six commands stay six. Whatever the client's
language, the driver is the `cavalry` binary that the README's first command builds
from the checkout, started with `target/release/cavalry run`; each client's README
starts from those two commands, adds the one that builds `cavalry-ffi`'s library
from the same checkout, and carries the path in its own language. `cargo install
cavalry` gives the binary from the first release on: until then crates.io holds the
`0.0.0` placeholders ADR 0002 reserved, and every crate is `publish = false`
(ADR 0003). Nothing in the slice needs a hand on the hosts:
the runners run on the shared runners, and the ABI rows run under `just bench` on the
bench hosts with what they have.

Out of the slice: Raft, the fifth slice, which the Revisit clause names; the first
release, crates.io and the five package registries, with `conformance/upgrade/` and
the ABI's freeze, which is ADR 0003's Revisit and an ADR of its own; a C++ wrapper,
since a C++ program includes the header; a Rust client over the ABI, since the Rust
client is the core; client-side counters and `add counter` (ADR 0019's Revisit); the
invoker mode and image handlers (ADR 0021's Revisit); the `console` command; OTel
export and discovery (`cavalry-control`); encryption; multicast and multi-destination
cast (ADR 0039). Three decisions are left to the feature ADRs that must carry their
evidence: where the ABI's page lives, under `spec/` beside the layouts with the
committed header as its vector or as the header's reference page, and what a reader
of it rejects (card 204); how `Offer::TooLong`, which has no Aeron code, reaches the
ABI, as `ERROR` (−6) with `cavalry_errcode` and `cavalry_errmsg` giving the length
asked for and the limit, or as a code of Cavalry's own past −6, read against what
Aeron's C client returns for a message over its limit (card 204); and how each
runtime calls the fragment handler back, with the fewest allocations it offers and
the count on record (cards 205 to 209).

The open cards, placed. Nine are open on the Cavalry list and none joins the slice.
Card 159, the soak host's week, waits on Rjae's hand on `kip`; its note that the host
joins the cluster slice as a third member is the fifth slice's. Card 176, the macOS
allocation gate's one allocation in twenty million messages, is a gate's own card;
the ABI feature adds a second allocation-counting path on the same runner, so the
backtrace card 176 asks for serves both. Card 177, the bench's purge mode, is the
harness's. Cards 185, 187 and 197, three flakes of the client's tests, are the
slice's cost until fixed, since five more runners over the same driver on the same
runners meet the same windows; the C runner and every client's runner wait on a
publication's limit and not on its connected flag, card 187's pattern, and the fixes
keep their place on the list. Card 194, the add on a closing endpoint, is a conductor
change with a small ADR of its own, since ADR 0042 is immutable; a client in any
language that removes and re-adds a subscription on a `cavalry:udp` channel meets it,
and it keeps its place on the list. Cards 198 and 199 are the bench's, the second an
ADR amending ADR 0033's trigger. Issues 14, 16 and 17 are the client protocol's and
the driver's, fixed once in the Rust client, which is the point: no client of this
slice reads the protocol.

## Alternatives considered

- **Raft fourth.** Lost for now, on four counts. The hosts: a cluster is three
  members, both bench hosts' isolated cores are taken (ADR 0044), `kip` is the soak
  host with its week still to run (ADR 0034, card 159), and a gate across hosts waits
  on a host that does not exist, which is the reason ADR 0023 held UDP for the slice
  after; the clients need no host. The fan-out: Aeron Cluster's log channel is
  multi-destination cast, which ADR 0039 put out of the UDP slice, so Raft either
  builds it first, with `add destination` in the client protocol and a sender that
  sends a burst to several addresses, or replicates over one publication per
  follower, and that is a decision with its evidence before the slice, not inside it.
  The scope: a cluster protocol page in `spec/`, the durable log as the Raft log with
  its catch-up and snapshots, an election, membership, a session protocol for the
  client, a service container, fault injection for a lost leader and a partitioned
  member, and mixed versions through a rolling upgrade, which is larger than any slice
  so far and ends with an ABI still unfrozen and five clients still absent. What it
  blocks: Raft's client sits on publications and subscriptions and adds a session
  protocol, as `AeronCluster` does, so it adds to the ABI and changes nothing the
  ABI fixes; the ABI first costs Raft nothing, while Raft first leaves the beachhead's
  languages without a client for another slice. `why-cavalry.md`'s "Archive plus
  Cluster is the real product" is honoured by the order, not refused: Raft is fifth,
  and this ADR's Revisit names it.
- **Raft fourth, gated on one host's loopback.** Lost: a cluster's promise is
  availability across hosts, a loopback cluster leaves the third host's question
  open for a slice whose point is that question, and ADR 0023 held UDP for the same
  reason rather than gate it on the loopback.
- **Both in one slice.** Lost: a slice is one page a stranger reads what is next from
  (ADR 0007, "No slice decision"); two threads of work with no order between them
  re-open the question on every card.
- **Each client a re-implementation of the client protocol, as Aeron's Java, C and
  .NET clients are.** Lost: `CLAUDE.md` says thin wrappers over `cavalry-ffi`; the
  claim protocol of `spec/log-buffer.md`, the client protocol and the image's reader
  in five languages is five places for invariant 1 to break, each with its own
  conformance against the vectors, and Aeron.NET is a port that ships Aeron's Java
  driver beside it (its README, read 2026-11-05).
- **One client in the slice, the rest later.** Lost: the clients are released
  together (`CLAUDE.md`), and the ABI is frozen by the first release, so the five
  runtimes' rules for pointers, callbacks and threads must have met it before then;
  a slice that ends with one client has not proven the surface it freezes.
- **The JVM first, where Aeron's users are.** Lost: the heaviest toolchain for the
  gates, a JDK and a build on both runners, for a language Aeron serves first-class;
  the first client proves the generator and the runner shape with the least new CI,
  and Python is on both runners today.
- **Invariant 2's per-message rules over every client's own side, Python's
  included.** Lost: CPython's `ctypes` cannot call a Python handler without making
  objects for its arguments, and a view over a claimed slice is an object per claim,
  so the rule rules out the first client and a language `CLAUDE.md` names; the
  allocation test counts Rust's global allocator and sees no runtime's heap, so the
  rule would be one with no gate, and `CLAUDE.md` says not to ship around a missing
  gate; and a runtime's collector and interpreter add a tail that nothing in Cavalry
  can remove. Each runner reports the runtime's side instead, and the Revisit names
  the report that reads zero.
- **A zero-allocation gate on the clients whose runtime can meet one.** Lost: which
  runtimes can is what cards 205 to 209 find out, not what this ADR knows, and a rule
  that differs by client is five rules for a stranger to read; one rule holds all
  five, and a runner moves to a gate when its report earns it (Revisit).
- **Hand-written clients.** Lost: `CLAUDE.md` says generated where possible, five
  hand-written wrappers drift from the header, and a generator over the one committed
  header is a change in one place per ABI change.
- **A Rust extension per language, PyO3 for Python and napi-rs for Node, as
  `why-cavalry.md`'s "One core, every language" named them.** Lost: such an
  extension binds `cavalry-client` directly through the language's own extension API,
  not the header, so it is not a thin wrapper over `cavalry-ffi` (`CLAUDE.md`,
  Clients and developer experience); two of the five clients would go round the
  surface the first release freezes, so `conformance/c/` and the committed header
  would not cover what they call, and the ABI would meet three runtimes' rules for
  pointers, callbacks and threads before it freezes instead of five; and each would
  be a further Rust crate on the data plane with its binding as a dependency under
  the allowlist, built per platform into the language's package beside the one
  library `cavalry-ffi` builds for all five, where Python's `ctypes` loads that
  library and needs no dependency. The bullet now names the header (Consequences).
- **A binding generator that owns the ABI.** Lost: such a generator's calling
  convention and buffer passing sit on the message path, and the ABI is Cavalry's own,
  decided under an ADR with Aeron's names, so that a C program calls it with no
  generator at all; the generator this slice builds reads Cavalry's header and writes
  idioms, and owns nothing on the wire.
- **The ABI as the Rust types' layout.** Lost: a handle across the boundary is an
  opaque pointer, as Aeron's C client's are, and the ABI is a C header with lifetime
  rules, not a struct layout; ADR 0021's Revisit says the ABI fixes the handles'
  layout, and this is how.
- **The first release in the slice.** Lost: a release is ADR 0003's Revisit and an
  ADR of its own, `publish` flipped, the five registries, the version the ABI freezes
  at, and `conformance/upgrade/`; the slice ends with the clients passing from the
  checkout, and the release follows when Rjae says.
- **`conformance/upgrade/` in the slice.** Lost: the N and N+1 it runs together exist
  at the first release, and before it N is whatever `master` was; the release's ADR
  builds it, as the first feature that needs it (`CLAUDE.md`, Gates).
- **A ten-minute path per language in the README, under `just quickstart`.** Lost:
  the README's six commands stay six, five toolchains on the quickstart step would
  make the path the slow one, and each client's README carries the path in its
  language with its runner as the test.
- **Deciding the slice in chat.** Lost: a decision that is not an ADR does not exist
  (ADR 0001), and this one orders six cards and places nine more.

## Evidence

None measured: a scope decision. What informed it: ADR 0039's Revisit clause and the
state of `master` at 61fbfdd on 2026-11-05, every gate real, card 182 archived,
`bench (udp)` among the required checks, the UDP step in `scripts/quickstart.sh`; the
promises in `CLAUDE.md`, the two crate READMEs, the glossary and `why-cavalry.md`; the
client API's exports at 61fbfdd and the Revisit clauses of ADRs 0019, 0021 and 0025;
the absence of `clients/` and `conformance/` and the eight suites `just conformance`
runs; Aeron's sources named in Context, read on 2026-11-05; the hosts' state in
ADR 0034 and ADR 0044; the nine open cards and five open issues with their briefs;
and cards 203 to 209 on the Cavalry list with their briefs and "Done when" sections.

## Consequences

Easier: the product's promise of one core in every language on the API users already
have, so a stranger reads what is next from cards 204 to 209 and this page; one
implementation of the protocol, so a client protocol fix lands once; Aeron's C names
on the ABI, so its documentation explains Cavalry's; the beachhead's three languages
get a client before the two Aeron serves; Raft, when it comes, adds a client on top of
a frozen surface instead of changing it.

Harder: the gates grow by five toolchains, each a pinned set-up action and minutes on
both runners; `cavalry-ffi` is the second `unsafe` crate, and its lifetimes are by
convention, so misuse must be loud; a callback into a managed runtime is the slowest
path a message takes, each client's runner reports it and nothing gates it; invariant
2's per-message rules and their gate stop at the ABI, so a client's own side may
allocate per message, as Python's does, and each client's runner reports what it
allocates, which nothing gates either; the bench hosts' baselines gain two rows per
profile, recorded as ADR 0033 says; the ABI's ADR waits on Rjae, as the format ADRs
did, and a later client can send it back to a new version before the first release;
a flake in the client's tests is met by six runners instead of one until it is fixed.

Changed in this ADR's commit: `why-cavalry.md`'s "One core, every language" named
PyO3 for Python and napi-rs for Node, which bind the Rust crates and not the header,
and now says that every client reaches the header through its language's
foreign-function interface, as `CLAUDE.md`'s repository map and its rule for clients
already say; the alternative that lost says why.

Revisit: when the fifth client passes `just conformance` on both runners and the
README says the fourth slice works, with a new ADR for the fifth slice, Raft, which
needs its third host (card 159's note; ADR 0034) and its fan-out decision before it,
one publication per follower or multi-destination cast (ADR 0039's Revisit); at the
first release, for the release's ADR, `publish` flipped, the five registries, the
version the ABI freezes at and `conformance/upgrade/` (ADR 0003's Revisit); when a
user asks for a sixth language or a C++ wrapper; when a client needs counters of its
own (ADR 0019's Revisit); when a managed runtime's callback cost or its allocations
per message show in a client's report, for a batched poll across the boundary; when a
client's report reads zero allocations per message on its runtime's side on both
runners, for whether that runner asserts it.

## References

- `CLAUDE.md`: invariants 1 to 6; Gates; Repository map; Unsafe; Protocol; Clients
  and developer experience; When in doubt
- ADR 0001, ADR 0002, ADR 0003, ADR 0007, ADR 0019, ADR 0021, ADR 0023, ADR 0025,
  ADR 0033, ADR 0034, ADR 0039, ADR 0040, ADR 0041, ADR 0042, ADR 0044
- `README.md`, the ten-minute path; `crates/cavalry-ffi/README.md`,
  `crates/cavalry-raft/README.md`, `crates/cavalry-client/README.md`;
  `crates/cavalry-client/src/publication.rs`, `Offer`; `docs/glossary.md`;
  `docs/explanation/why-cavalry.md`; `docs/how-to/implement-a-feature.md`;
  `scripts/quickstart.sh`; `justfile`, the `conformance` recipe
- Aeron, read 2026-11-05 at `master`: `aeron-client/src/main/c/aeronc.h`
  (<https://github.com/aeron-io/aeron/blob/master/aeron-client/src/main/c/aeronc.h>);
  `aeron-cluster/src/main/java/io/aeron/cluster/client/AeronCluster.java`;
  `aeron-cluster/src/main/java/io/aeron/cluster/ConsensusModule.java`; the Cluster
  Tutorial (<https://github.com/aeron-io/aeron/wiki/Cluster-Tutorial>)
- Aeron.NET (<https://github.com/AdaptiveConsulting/Aeron.NET>) and rusteron-client
  0.2.19 (<https://docs.rs/crate/rusteron-client/latest>), read 2026-11-05
- AppShapes Trello cards 159, 176, 177, 185, 187, 194, 197, 198, 199, and 203 to 209
