# 0068. Take the C ABI to version 2 with the committed log

- **Status:** Accepted
- **Date:** 2026-11-10
- **Invariants affected:** 1, 2, 6
- **Supersedes:** none. Amends `spec/c-abi.md` to version 2 with one opaque type, one
  struct, the ten functions of a new "committed log" group, one constant and one error
  code; every declaration of version 1 is unchanged (ADR 0051; ADR 0057; ADR 0066)

## Context

ADR 0066 feeds a cluster's committed log to a service through a replay of its
member's recording, bounded by the member's commit position, and the Rust client has
it as `Cavalry::committed_log` and `CommittedLog`. `spec/cluster.md` opens by saying
that a service is "a subscriber through the client API and the C ABI", and ADR 0052
asks card 227 for the committed log "through the client API and the ABI, so that a
service is a program in any of the five" languages. The ABI at version 1 has no such
call, and a service cannot build one from what the ABI has: the bound is a counter's
value, checked against the member's record and its recording id on every load (ADR
0066), and the ABI has no counters and no bounded poll.

`spec/c-abi.md` says that a change to any declaration "is a new version of this page,
a new ADR and new vectors, before any code". Nothing is released (ADR 0003), so the
number is free to move.

Card 227 also gave the Rust client an armed recording, a recording's stop position, a
client's counter, the bounded poll and the sync of a recording on command (ADR 0059,
ADR 0062, ADR 0066, ADR 0067). Each is used by a consensus module, which is Rust and
runs beside the driver. None is something a service needs.

The Rust `CommittedLog::poll` hands its handler three things: the entry's bytes, its
frame's header, and its position in the log. The ABI's fragment handler has two, and
each of the five generated clients has one trampoline for it, a per-message path with
its own check and record (ADR 0046).

## Decision

Version 2 of the page adds the committed log and nothing else.

A handle, `cavalry_committed_log_t`, made by `cavalry_committed_log(log, client,
position)` and freed by `cavalry_committed_log_close(log, client)`. The open is the
Rust call: the subscription on the service log stream first, then the replay that
follows the member's recording. A driver that hosts no member with a recording of
its log is the Rust `None`: the function returns 0 and writes a null handle, with no
error set, as `cavalry_subscription_image_at_index` answers past the count. A service
started before its member asks again.

`cavalry_committed_log_poll(log, handler, clientd, fragment_limit)` takes the fragment
handler the subscription's poll takes. The entry's position reaches the handler
another way: the header the poll hands over carries it, and
`cavalry_committed_log_entry_position(header)` returns it from inside the handler. A
header from a subscription's poll carries none, and the function then returns −1 with
a new error code, 138, `CAVALRY_ERR_NO_POSITION`, never a number.

The reads are the Rust handle's: `_position`, `_commit_position`, `_is_ended`,
`_failed_at`, which writes the position through a pointer as `cavalry_replay_ended`
does, `_fault` with the subscription's `cavalry_fault_t`, and `_constants` with the
member id and the recording id. `CAVALRY_SERVICE_LOG_STREAM_ID`, 104, is the Rust
constant.

The rules are the subscription's where the thing is the same. A poll holds the log
for the run of the handler, and the page's table says what each call from inside it
answers; the handler may use any other handle, a publication for the service's
answers among them. The open and the close count as calls on the client as well as on
the log, since each sends commands and waits; every other call is on the log alone,
so a service polls in one thread while another uses the client. The close stops the
replay through the client. A null client is allowed and is the Rust handle's drop:
the subscription is closed, the handle freed, and the replay left to the driver's
replay connect timeout. A log outlives its client's `cavalry_close` as a
subscription does, and `cavalry_close` frees the client handle, so the null client is
the one way to free such a log. The close frees the handle whatever it returns, with
one exception the page names: a close refused from inside the log's own poll frees
nothing.

The five generated clients bind the functions when their packages are regenerated
from the header, as they bind every function, and refuse a library of version 1.
Each gains a committed log object over them, whose poll hands the caller's handler
the body, the header and the position as the Rust one does, in a change of its own
after this one: a client's poll is that client's per-message path, with its own
check and record (ADR 0046). Until then the Node addon does not bind
`cavalry_committed_log_poll`, since its bindings marshal no handler and its one poll
is written by hand.

The bounded poll, the armed recording, a recording's stop position, the sync of a
recording and a client's counter stay out, and "Out of this version" names each.

## Alternatives considered

- **A second handler type with the position as its fifth argument.** Lost. It is the
  Rust signature in C, and it costs a second trampoline in each of five clients:
  Python's callback type, Node's C function in the addon, the JVM's upcall stub,
  .NET's unmanaged entry point and Go's exported function, each a per-message path
  with its own allocation check (ADR 0046). The position in the header costs one
  field in a struct the poll already builds on its stack for each fragment.
- **`cavalry_header_position`, Aeron's name, for every header.** Lost for this
  version. Aeron's function returns the position after the frame, and for a
  subscription's header it needs the image's position context, which the Rust
  subscription's poll does not hand its handler; giving it would change the path
  every subscriber's message takes. A function under Aeron's name that failed for a
  subscription's header would compile in a ported program and fail when it ran. The
  page keeps the function under "Out of this version".
- **The committed log's constants carrying `initial_term_id` and
  `position_bits_to_shift`, as an image's do, and the caller computing the position.**
  Lost: the handle learns them when the replay's image joins, after the open has
  returned, so the constants could not be filled "from a copy the handle took when it
  was made", and every service would repeat arithmetic the library has done.
- **No position for an entry: the service reads `cavalry_committed_log_position`
  after each poll.** Lost: a service that stops partway through a batch needs the
  position after the last entry it applied, and it cannot add up frame lengths to
  find it, since the padding at the end of a term moves the position and is never
  handed to the handler.
- **An error for a driver with no member.** Lost: a generated client would raise an
  exception for the ordinary state of a service started first, and the Rust API says
  `None`.
- **The whole of card 227's client API in this version: the armed recording, the stop
  position, the counter, the bounded poll, the sync.** Lost: each would be bound,
  wrapped and tested in five clients for a caller that does not exist. A consensus
  module is Rust. Each can come under a later version with the client that needs it,
  as the page's last section has always said.
- **The close without the client, `cavalry_committed_log_close(log)`.** Lost: it could
  only close the subscription, which leaves the replay to end by its connect timeout,
  5 s, and a service that opens the log again at once would hold two of the driver's
  replays for that time. The null client keeps that close for the case that needs
  it, and an error for a null client would leave a log that outlived its client with
  no close that succeeds.

## Evidence

- `spec/c-abi.md` at version 2 and `spec/vectors/c-abi.json`: 9 opaque types, 10
  structs, 70 functions and 73 defines, against which `cavalry-ffi`'s conformance
  test holds the generated header.
- `cavalry-ffi`, `tests/committed_log.rs`, beside three cluster members in the test's
  process. `a_service_reads_the_committed_log_through_the_c_handler_with_each_entrys_position`:
  a service on a follower's driver is handed the term's first entry and a hundred
  more in order, entry n at position 96 × n from the header, the handle's position
  and bound at the end of the last, and nothing more while nothing more is
  committed; from inside the handler every call on the log is refused with
  `CAVALRY_ERR_POLLING` but the constants, the refused close frees nothing, and the
  refused poll delivers nothing; a subscription's header answers
  `CAVALRY_ERR_NO_POSITION`; a second handle opened at the first one's position reads
  the three entries committed after it and none before; after its client's
  `cavalry_close` that handle's poll answers the `closed` fault, and it is closed
  with a null client, which returns 0.
  `a_driver_with_no_member_answers_the_open_with_a_null_handle_and_no_error`, with
  every null argument that is required refused.
- `cavalry-ffi`, `tests/allocations.rs`: a second window, two thousand entries
  appended, committed by three members whose duty cycles run inside the window, and
  handed to the C handler with their header's values and positions, with no
  allocation and no reallocation in any thread of the process.
- `tests/conformance.rs` holds the header cbindgen writes to the vectors, and
  `clients/generate-test.py` holds the five generators' parse of it to them: 59 tests.
- The runners, `just conformance`'s, against the version 2 library on the laptop: C,
  with the null handle of a driver that hosts no member and a subscription's header
  refused its entry position, and Python, Node, Go, .NET and the JVM, each to `ok`.
  The JVM runner ran with Homebrew's Java 25 first on the path, which the laptop's
  path does not hold by itself.

## Consequences

Easier: a service is a program in C, Python, Node, Go, Java or C#.

Harder: the ABI's version is 2 before anything is released, and every package
generated before this change refuses the library until it is regenerated, as its
message says. A header now carries a position for one of the two polls that hand one
over, and the page has to say which.

Revisit: when a client in another language needs one of the things left out; when
the subscription's poll can hand over a position, which makes
`cavalry_header_position` possible for every header; at the first release, which
freezes the version (ADR 0003, ADR 0051).

## References

- `spec/c-abi.md`, "Handles", "Structs", "Functions", "Error codes", "Constants",
  "When a function cannot answer", "Calls during a claim or a poll", "Lifetimes",
  "Threads", "A frame's position", "Versions and what a reader rejects", "Out of this
  version"; `spec/cluster.md`, "The service".
- ADR 0046, ADR 0051, ADR 0052, ADR 0057, ADR 0066.
- Card 227.
