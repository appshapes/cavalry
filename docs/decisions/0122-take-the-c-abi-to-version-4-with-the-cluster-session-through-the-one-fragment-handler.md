# 0122. Take the C ABI to version 4 with the cluster session, through the one fragment handler

- **Status:** Accepted
- **Date:** 2026-11-14
- **Invariants affected:** 1, 2, 6
- **Supersedes:** none. Amends `spec/c-abi.md` to version 4 with one opaque type, two
  structs, the eleven functions of a new "cluster session" group and five error
  codes; every declaration of version 3 is unchanged (ADR 0051, ADR 0070, ADR 0080).
  It is the ABI's part of work item 229, the cluster client (ADR 0052, feature 5),
  for the session. The service's part is the next version's.

## Context

`cavalry-client` holds a session with a cluster: `Cavalry::connect_cluster` returns
a `ClusterSession`, whose `offer` sends the leader a message and whose `poll` hands
over the cluster's replies, each with a `ReplyHeader`, and is the session's duty
cycle (ADR 0117, ADR 0121). ADR 0052 asks for the session "in the client API, the
ABI and the five generated clients". The ABI at version 3 has none of it, and
`spec/c-abi.md` says of itself that it holds "every public item of `cavalry-client`
in a C form or named under Out of this version". Since ADR 0117 that sentence has
not been true of the page.

A session cannot be built from what the ABI has. It parses the session protocol's
messages, and its connect reads the response channel with a controlled poll, which
the ABI leaves out.

The Rust `ClusterSession::poll` hands its handler two things: a reply's payload and
its `ReplyHeader`, the reply's leadership term id and the cluster's time. It hands
over no frame header. The ABI has one handler type,
`cavalry_fragment_handler_t(clientd, buffer, length, header)`, and each of the five
generated clients has one trampoline for it, a per-message path with its own check
and record (ADR 0046). ADR 0070 met the same question for the committed log, whose
Rust handler takes a third argument, and kept the one handler: the entry's position
travels in the header, and a function reads it from inside the handler.

The service's poll is the larger half of what is missing: its handler returns an
action and answers sessions from inside the poll (ADR 0118). A change of the ABI is
reviewed whole, and the two halves together are more than a review holds
(`docs/how-to/implement-a-feature.md`, step 3.8). Nothing is released (ADR 0003), so
the version number is free to move twice.

Aeron has no C client for its cluster: `aeron-cluster` is Java, and its client,
`AeronCluster`, is a class of that module. So there are no C names to take. Aeron's
names for the things are `AeronCluster.connect`, `offer`, `pollEgress`,
`clusterSessionId`, `leadershipTermId`, `leaderMemberId` and `close`.

## Decision

Version 4 of the page adds the cluster session and nothing else.

A handle, `cavalry_cluster_session_t`, made by `cavalry_connect_cluster(session,
client, members, response_channel)` and freed by
`cavalry_cluster_session_close(session)`. The connect is the Rust call: it blocks
until the leader has opened the session, or for 20 s.

`cavalry_cluster_session_offer(session, buffer, length)` returns what a
publication's offer returns: the position after the message, or one of the six
result codes. A message longer than one frame is `CAVALRY_PUBLICATION_ERROR` with
`CAVALRY_ERR_TOO_LONG`, and a closed session is `CAVALRY_PUBLICATION_CLOSED`.

`cavalry_cluster_session_poll(session, handler, clientd, fragment_limit)` takes the
fragment handler the two other polls take. `buffer` and `length` are a reply's
payload. The `header` carries the reply's values, and
`cavalry_cluster_reply_values(header, values)` fills a
`cavalry_cluster_reply_values_t`, the leadership term id and the cluster's time,
from inside the handler.

**A header says what it carries, and a function asked for what it does not carry
refuses.** A header from a cluster session's poll carries no frame header, since
the Rust poll hands over none. `cavalry_header_values` on it is −1 with a new code,
139, `CAVALRY_ERR_WRONG_HEADER`, and so is `cavalry_cluster_reply_values` on a
header from a subscription's poll or a committed log's.
`cavalry_committed_log_entry_position` on it is −1 with `CAVALRY_ERR_NO_POSITION`,
as it is for a subscription's header, since it carries no position. No function
fills a struct for a header of another kind. The page has a table of the three
polls and the three functions, "What a header carries".

The reads are the Rust handle's: `_leadership_term_id`, `_leader_member_id`,
`_stale_replies`, `_fault` with the subscription's `cavalry_fault_t`, and
`_constants` with the session id and the longest message. `_closed(session)`
returns why the session is closed, a string valid for the handle's life, and null
with no error set while the session is open, which is the Rust `Option<&str>`.

The Rust `ClusterError` has four kinds that are a session's own. Each gets a code
in a range of the cluster's, 140 to 143: `CLUSTER_MEMBERS`, `CLUSTER_REFUSED`,
`CLUSTER_VERSION` and `CLUSTER_CONNECT_TIMEOUT`. Its `Client` kind keeps the
client's or the driver's code, and its `Egress` kind a fault's.

The rules are the subscription's where the thing is the same. A poll holds the
session for the run of the handler: from inside it the session's own functions
answer `CAVALRY_ERR_POLLING`, its offer among them, as the Rust borrow refuses
them, and the handler may use any other handle. The connect counts as a call on the
client. Every other function is a call on the session alone. A session outlives its
client's `cavalry_close` as a subscription does and still needs its own close.

The service, `Cavalry::cluster_service` with `ClusterService`, `Replies` and the
rest, is named under "Out of this version" and comes with version 5, in the next
change. It takes this handler too. Its entry's values are read from the header,
and its reply and its abort are calls on the service from inside the handler, so
that no client needs a second trampoline for a handler that returns a value.

The five generated clients bind the functions when their packages are regenerated
from the header, and refuse a library of version 3. Each gains a session object
over them in a change of its own, with a cluster round in its runner (ADR 0046).
Until then the Node addon does not bind `cavalry_cluster_session_poll`, as it did
not bind the committed log's: its bindings marshal no handler, and each of its
polls is written by hand.

## Alternatives considered

- **A handler type of the session's own,** with the term and the time as its
  fourth and fifth arguments. It is the Rust signature in C. Lost with ADR 0070's
  reason: a second trampoline in each of five clients, Python's callback type,
  Node's C function in the addon, the JVM's upcall stub, .NET's unmanaged entry
  point and Go's exported function, each a per-message path with its own
  allocation check. The values in the header cost two fields in a struct the poll
  builds on its stack.
- **`cavalry_header_values` answering for a session's header with the frame of the
  reply.** No function would refuse a header. Lost: the Rust poll does not hand its
  handler the frame's header, and the ABI holds nothing the Rust API lacks (ADR
  0051). The frame's length would also not be the payload's, since the session
  message header lies between, and a program that computed with it would be wrong.
- **Zeros for what a header does not carry.** Lost: a term of 0 and a time of 0 are
  answers. A subscription's handler that asked for a reply's values by mistake
  would read a reply of term 0, and nothing would say so.
- **One error code for each thing a header may lack,** `NO_FRAME` and `NO_REPLY`
  beside version 2's `NO_POSITION`. Lost: the service's header would add a third,
  and each client's exception type would grow with them. One code says the header
  is of another kind, and the message names the poll whose header the function
  reads. `NO_POSITION` keeps its number and its meaning.
- **The term and the time as two functions,** each an `int64_t`. Lost: −1 would
  have to mean "cannot answer", and the cluster's time is a leader's wall clock,
  which the page cannot promise is never negative. A struct filled through a
  pointer, with a status returned, has no such value. It is also one call across
  the boundary where two functions are two.
- **The closed reason as a code,** `cavalry_closed`'s shape. Lost: the reason is the
  detail of the cluster's closed event, bytes that a later version of the cluster
  protocol may add to, and a code per detail is a new version of this page for
  each. The Rust handle gives a string.
- **The closed reason through a pointer, with `true` returned,** as
  `cavalry_replay_ended` writes its position and reason. The first form of this
  change. Lost: a `const char **` is a type the ABI has nowhere else, and two of
  the five generators had no form for it. A returned string is a type every client
  already binds, for `cavalry_dir`. Null for an open session is an answer and sets
  no error, as `cavalry_subscription_image_at_index` answers past the count.
- **The session and the service in one version.** Lost on size: the service's
  functions, its reply codes and its rules from inside the handler are as much
  again as the session's. Each half passes the gates alone.
- **An offer allowed from inside the session's own handler.** Aeron's client allows
  it, and a program that answers a reply with its next request would use it. Lost
  for this version: the Rust poll borrows the whole session, so the ABI could
  allow it only by holding two references to one thing. It needs the Rust session
  to split into what offers and what polls, which is a change to `cavalry-client`
  with its own evidence. The Revisit clause names it.
- **The egress and ingress stream ids as defines,** beside
  `CAVALRY_SERVICE_LOG_STREAM_ID`. Lost: the Rust client exports neither, and a
  program beside a session has no reason to avoid them on its own channels, since
  a session's streams are on the members' endpoints and its own response channel.

## Evidence

- `spec/c-abi.md` at version 4 and `spec/vectors/c-abi.json`: 10 opaque types, 12
  structs, 83 functions and 78 defines, against which `cavalry-ffi`'s conformance
  test holds the generated header, and `clients/generate-test.py`, 65 tests, the
  five generators' parse of it.
- `cavalry-ffi`, `tests/cluster_session.rs`, against a driver in the test's process
  with the leader played by a Rust client in a thread.
  `a_session_offers_polls_reads_its_replies_values_and_closes`: the constants are
  the leader's session id and 1,344 bytes; a message's reply reaches the C handler
  with the session's term and the cluster's time; from inside the handler
  `cavalry_header_values` answers `WRONG_HEADER` and fills nothing,
  `cavalry_committed_log_entry_position` `NO_POSITION`, the constants 0, and every
  other call on the session `POLLING`, the offer with `CAVALRY_PUBLICATION_ERROR`
  and the close with nothing freed; a message of 1,345 bytes is
  `CAVALRY_PUBLICATION_ERROR` with `TOO_LONG`; a reply of the first term after one
  of the next is not handed over, and the count of stale replies is 1; a new
  leader event moves the term and the member; the cluster's closed event gives the
  reason `TIMEOUT`, at one address for the handle's life, and an offer then
  answers `CAVALRY_PUBLICATION_CLOSED`; the close of a closed session sends the
  leader nothing. `a_close_tells_the_leader_and_a_session_outlives_its_client`: the
  played leader reads an open session's close with its term and id, and after
  `cavalry_close` a session's offer answers closed, its poll the fault 122, and
  its own close 0. `the_connect_refuses_what_the_page_says_with_the_clusters_codes`:
  each null argument, a members string and a response channel that are not UTF-8,
  with nothing sent to any member, a members string of one endpoint, code 140, the
  leader's `concurrent session limit`, code 141 with the reason and the page's
  anchor in the message, and a cluster of version 2.0.0, code 142; each leaves a
  null handle. `a_subscriptions_header_carries_no_replys_values` and
  `a_null_session_is_refused_by_every_function`.
- `cavalry-ffi`, `src/error.rs`, `a_cluster_sessions_error_has_the_clusters_codes`:
  the four kinds on 140 to 143, the no-answer one among them, which the test above
  does not wait 20 s for, each with the Rust error's own words and anchor.
- `cavalry-ffi`, `tests/committed_log.rs`: every entry's header refuses
  `cavalry_cluster_reply_values` with `WRONG_HEADER`, beside three cluster members.
- `cavalry-ffi`, `tests/allocations.rs`, a third window: 2,000 messages offered
  through the ABI and their replies handed to the C handler, which reads each
  reply's values, and 4 stale replies passed over and counted, with no allocation
  and no reallocation in any thread of the process.
- ADR 0070's evidence for the one handler: the five runners read a committed log
  through the trampoline they read a subscription through.
- Aeron, read on 2026-11-14 at `master`, version 1.55.0-SNAPSHOT:
  `aeron-cluster/src/main` holds `java` and `resources` and no C source, and
  `AeronCluster.java` has the names above.

## Consequences

Easier: a program in C holds a session with a cluster, and the five clients can.
The page is whole again for the session: every public item of `ClusterSession` is
on it. The service's version has its shape decided.

Harder: the ABI's version is 4 before anything is released, and every package
generated before this change refuses the library until it is regenerated. A header
is now of three kinds, and three functions read it, each of which refuses two of
them; the page's table is the one place that says which. A handler cannot offer on
its own session. The reply's values cost a call from inside the handler for a
program that wants them.

Revisit: when a program must offer from inside its session's handler, for a Rust
session whose offer and poll are two borrows; when a client in another language
needs the frame of a reply; when the service is added, for whether its header's
values fit the table; at the first release, which freezes the version (ADR 0003,
ADR 0051).

## References

- `spec/c-abi.md`, "Handles", "Structs", "Functions", "Result codes", "Error codes",
  "Strings", "When a function cannot answer", "Calls during a claim or a poll",
  "Lifetimes", "Threads", "What a header carries", "Versions and what a reader
  rejects", "Out of this version"; `spec/cluster.md`, "Sessions" and "Replies"
- `crates/cavalry-ffi/README.md`; `docs/glossary.md`, cluster session and stale reply
- ADR 0003, ADR 0046, ADR 0051, ADR 0052, ADR 0070, ADR 0080, ADR 0114, ADR 0117,
  ADR 0118, ADR 0121
- Work item 229 of the Brigade Board
- Aeron, read 2026-11-14 at `master` (1.55.0-SNAPSHOT):
  `aeron-cluster/src/main/java/io/aeron/cluster/client/AeronCluster.java`
  (<https://github.com/aeron-io/aeron/tree/master/aeron-cluster>)
