# 0123. Take the C ABI to version 5 with the cluster service, answered from inside the one fragment handler

- **Status:** Accepted
- **Date:** 2026-11-14
- **Invariants affected:** 1, 2, 6
- **Supersedes:** none. Amends `spec/c-abi.md` to version 5 with one opaque type, one
  struct, the ten functions of a new "cluster service" group, fourteen defines and
  three error codes; every declaration of version 4 is unchanged (ADR 0051, ADR
  0070, ADR 0080, ADR 0122). It is the ABI's part of work item 229, the cluster
  client (ADR 0052, feature 5), for the service: the half that ADR 0122 left to the
  next version.

## Context

`cavalry-client` holds a cluster's service: `Cavalry::cluster_service` returns a
`ClusterService`, whose `poll(client, handler, fragment_limit)` reads the committed
log of the member on the client's driver and hands its handler each entry (ADR
0118). ADR 0052 asks for it "in the client API, the ABI and the five generated
clients". The ABI at version 4 names it under "Out of this version", and ADR 0122
decided its shape in one paragraph: the service's poll takes the one fragment
handler, the entry's values are read from the header, and the reply and the abort
are calls on the service from inside the handler.

The Rust handler is `FnMut(&Committed, &mut Replies) -> Action`, and the one C
handler, `cavalry_fragment_handler_t(clientd, buffer, length, header)`, differs
from it in three ways. It is handed bytes and a header, where the Rust handler is
handed the entry as what it is, a `Committed` with an `Entry`. It is handed no
`Replies` to answer a session through. And it returns nothing, where the Rust
handler returns the `Action` that says whether the entry is taken.

The Rust poll also borrows the client, `&mut Cavalry`, for its whole run. It sends
commands with it: it opens the member's log, and adds an egress publication for
each open session when the service begins to answer. No function of the ABI held a
client across a C handler before. The committed log's poll takes no client, so the
client handle has no state word, as every handle with a poll has. A handler that
called a function of that client would make a second exclusive reference to one
Rust `Cavalry`, which is undefined behaviour and not a wrong answer.

Aeron has no C client for its cluster and no C service (ADR 0122). Its Java service
is `ClusteredService.onSessionMessage`, which answers through
`ClientSession.offer`. Its C client has the actions of a controlled poll,
`aeron_controlled_fragment_handler_action_t`: `AERON_ACTION_ABORT` 1, `_BREAK` 2,
`_COMMIT` 3 and `_CONTINUE` 4.

Two of the five generators refused a type the ABI had nowhere else when version 4
was written (ADR 0122). A signature is settled only once all five have a form for
it.

## Decision

Version 5 of the page adds the cluster service and nothing else.

A handle, `cavalry_cluster_service_t`, made by `cavalry_cluster_service(service,
client)` and freed by `cavalry_cluster_service_close(service, client)`. The close
takes the client as the committed log's does, and a null client is the Rust
handle's drop.

`cavalry_cluster_service_poll(service, client, handler, clientd, fragment_limit)`
takes the client as an argument, as the Rust poll does, and the fragment handler
the three other polls take. It returns the entries the handler took.

**The handler answers by calling the service.** The `header` carries the entry's
values, and `cavalry_cluster_entry_values(header, values)` fills a
`cavalry_cluster_entry_values_t`: the entry's position, its leadership term id, the
cluster's time, its type as one of five defines, its session id, the leader of a
new term and whether a closed session timed out, with −1 and `false` where an
entry has none. `buffer` and `length` are a session message's payload.
`cavalry_cluster_service_reply(service, session_id, buffer, length)` is the Rust
`Replies::reply`. It returns one of six reply codes, the Rust `Reply` in order from
1, and −1 when it cannot answer. `cavalry_cluster_service_action(service, action)`
is the `Action` the Rust handler returns, with Aeron's names on Aeron's values:
`CAVALRY_ACTION_ABORT` 1, `CAVALRY_ACTION_BREAK` 2 and `CAVALRY_ACTION_CONTINUE` 4.
A handler that does not call it continues, and the last call for an entry counts.
Both functions are for the handler: from anywhere else each is −1 with a new code,
145, `CAVALRY_ERR_CLUSTER_NO_ENTRY`.

**The poll holds its client.** The client handle gets a state word, set for the
run of a service's poll. From inside the handler every function that takes that
client answers `CAVALRY_ERR_POLLING`: the client's close and its reads, every add
and command, the open of a committed log, a cluster session's connect, the making
of a service, and the close or the poll of another handle with that client. A
refused close frees nothing. `cavalry_client_id` and `cavalry_dir` answer, from a
copy the handle keeps, as every handle's constants do during its poll. A call on
any other handle is allowed, a publication's offer among them. The service's own
functions answer `CAVALRY_ERR_POLLING` from inside its handler, as every handle's
do, except the two that are made for the handler.

A header is now of four kinds. A service's carries its entry's values and no
frame, since the Rust poll hands over none. `cavalry_header_values` and
`cavalry_cluster_reply_values` refuse it with `CAVALRY_ERR_WRONG_HEADER`, and
`cavalry_cluster_entry_values` refuses the three other kinds the same way.
`cavalry_committed_log_entry_position` answers it, with the position that is one
of its values. The page's table, "What a header carries", has the fourth row and
the fourth column.

Three error codes. 144, `CAVALRY_ERR_CLUSTER_ENTRY`, is the Rust
`ClusterError::Entry`, a committed entry that is no entry of this version's
protocol. 145 is above. 146, `CAVALRY_ERR_CLUSTER_ACTION`, is an action that is
none of the three. `cavalry_cluster_service_fault` fills the subscription's
`cavalry_fault_t` with what the last poll stopped on, whichever code it was: a
fault's fields, the position of the entry for 144, or the client's or the driver's
code alone.

The five generated clients bind the functions when their packages are regenerated
from the header, and refuse a library of version 4. Each gains a service object
over them in a change of its own, with a cluster round in its runner (ADR 0046).
Until then the Node addon does not bind `cavalry_cluster_service_poll`, as it does
not bind a cluster session's.

## Alternatives considered

- **A handler type of the service's own,** which is handed the values and a reply
  handle and returns the action. It is the Rust signature in C. Lost with the
  reason of ADR 0070 and ADR 0122: a second trampoline in each of five clients,
  each a per-message path with its own allocation check. It would also be the
  first handler that returns a value, and every runtime's upcall has another form
  for one that does.
- **A service that keeps its client from its making,** so that the poll takes
  none. A shorter call. Lost: the poll would use a handle its call does not name,
  and the thread rule, one thread at a time for each handle, would be broken by a
  call that shows no client. `cavalry_close` frees the client handle, so the
  service would hold a pointer to freed memory, where the Rust borrow makes that
  impossible. The Rust poll takes the client, and so does the committed log's
  close.
- **Calls on the client from inside the handler left undefined,** as
  `cavalry_image_*` on an image is during its subscription's poll. Lost: a handler
  that reads an entry and adds a publication for it is the mistake a program is
  most likely to make, and it would be two exclusive references to one Rust
  client. A state word costs one load on each call that takes a client, and none
  of those is per message.
- **A handler that may use the client.** It needs a Rust poll that lends its
  handler the client, or gives its borrow back around the handler. That is a
  change to `cavalry-client` with its own evidence, since ADR 0118 chose the
  signature, and the ABI holds nothing the Rust API lacks (ADR 0051). The Revisit
  clause names it.
- **The reply through the header,** `cavalry_cluster_reply(header, session_id,
  buffer, length)`, so that a handler needs no pointer to its service. Lost: a
  header is what a poll hands over to be read, it is `const`, and four functions
  read it. A function that sends through it would make it a handle that lives for
  one call. A handler has its service through its `clientd`.
- **An abort that the reply makes by itself,** when it answers retry. One call
  fewer. Lost: the Rust handler may go on past a retry and give the reply up (ADR
  0118), and may abort or break for a reason of its own. The ABI holds what the
  Rust API has.
- **Two functions with no argument,** an abort and a break. No value could be
  refused, so there would be no code 146. Lost: Aeron's C client names the actions
  as values, and a program ported from it writes them. Each generated client's
  handler will return one value, which its wrapper passes on: one function is one
  binding, and a fourth action would need no new function.
- **Aeron's `COMMIT`, 3.** Lost: a read here stores its position once, when it
  ends (ADR 0114), and the Rust `Action` has no such variant. The value stays
  free, so the three keep Aeron's numbers.
- **`cavalry_committed_log_entry_position` refusing a service's header,** so that
  each kind of header has one reader. Lost: its refusal is `NO_POSITION`, which
  says that the header carries no position, and this one carries one.
- **`cavalry_header_values` answering for a service's header with the entry's
  frame.** Lost as in ADR 0122: the Rust poll hands its handler no frame header.
- **A fault that holds faults alone,** with a code of 0 after a poll that failed on
  an entry or on a command. Lost: 0 there says that the last poll returned a
  count. A client's wrapper reads one struct after a −1, whatever stopped the
  poll.
- **The two codes of the handler's calls in the ABI's own range.** That range, 130
  to 139, is full. They follow the cluster's, and the page's table says which
  codes are the Rust error's and which the ABI's own.
- **`cavalry_client_id` and `cavalry_dir` refused from inside the handler too.**
  One rule, every function of the client. Lost: both are constants of the handle,
  and a handle's constants answer during its poll everywhere else on the page.

## Evidence

- `spec/c-abi.md` at version 5 and `spec/vectors/c-abi.json`: 11 opaque types, 13
  structs, 93 functions and 95 defines, against which `cavalry-ffi`'s conformance
  test holds the generated header, and `clients/generate-test.py`, 65 tests, the
  five generators' parse of it. Every generator has a form for every signature.
- `cavalry-ffi`, `tests/cluster_service.rs`, three tests, 25 of 25 runs on the
  laptop.
  `a_service_takes_each_entry_answers_its_session_and_holds_its_client_while_it_polls`
  runs three cluster members in the process, a service through the ABI beside each
  and a session through the ABI on a fourth driver. A message is handed to all
  three services with the same position, term, time and session; the leader's
  service answers `SENT` and each follower's `NOT_ANSWERING`; the session reads
  one reply, with the entry's term and time. Each service is handed the term's
  first entry with the leader's id, then the session's open event, in the log's
  order, and `cavalry_committed_log_entry_position` agrees with every entry's
  position. From inside the leader's handler, 45 calls answer as the page says:
  the header's three; the service's seven with `POLLING`; the client's two
  constants answered, and nineteen calls that take the client `POLLING`, each add
  leaving no handle; another service's poll and close with that client `POLLING`,
  and its reply and action `NO_ENTRY`; a third service's close with no client
  allowed, and an offer on a publication; a reply of 1,345 bytes `TOO_LONG` with
  both lengths in the message, one of 1,344 bytes sent and read by the session, a
  reply to session 999 `NO_SESSION`, and one with a null buffer refused; the
  actions 3 and 0 refused with code 146; and an abort followed by a continue,
  after which the entry is taken once. The second service, the committed log and the client
  that the refused closes named are closed afterwards, so none was freed. An entry
  aborted twice is handed over three times at one position, its reply answered
  `SENT` each time, and the session reads it once. A break ends its poll with one
  entry taken while the next is there, and the next poll takes that one.
  `a_service_refuses_what_the_page_says_and_outlives_its_client`: a service beside
  no member polls 0, a reply and an action from outside the handler are code 145,
  every function refuses a null handle or pointer, and a service whose client is
  closed still reads and closes with no client.
  `a_subscriptions_header_carries_no_entrys_values`.
- Two changes to the code, each made and undone, fail the first test: a break
  that continues, at the break's assertion with 2 entries taken; and
  `cavalry_is_closed` without its check, at the probe, with 131 where 134 is
  expected.
- `cavalry-ffi`, `src/cluster_service.rs`: a failed poll's fault for an entry, for
  two faults and for a client's error, and every `Entry` on its define.
  `src/error.rs`: the Rust `ClusterError::Entry` on code 144 with its own message
  and anchor. `tests/committed_log.rs`: every entry's header refuses
  `cavalry_cluster_entry_values`.
- `cavalry-ffi`, `tests/allocations.rs`, a window of its own: 2,000 messages
  offered on a session through the ABI, committed by the three members, each
  entry's values read, answered and continued by the service's C handler beside
  the leader, and 2,000 replies handed to the session's C handler, with no
  allocation and no reallocation in any thread of the process. The messages go
  forty at a time. One at a time the same window took 47 s, the time of 2,200
  commits across three members, and counted 5 allocations and 4 reallocations,
  all in a driver's conductor thread: `Conductor::close_if_done`, which removes a
  closed publication's log once the publication linger, 5 s, has passed. That is
  the control plane, 5 s after an earlier window closed its publication. The test
  now closes no handle until its last window has ended, so that no window's
  length decides whether it holds another's linger.
- Aeron, read on 2026-11-14 at `master`, version 1.55.0-SNAPSHOT: `aeronc.h` gives
  `aeron_controlled_fragment_handler_action_t` the four values above.

## Consequences

Easier: a program in C runs a cluster's service, and the five clients can. The page
is whole for the cluster client: every public item of `ClusterService`,
`Committed`, `Entry`, `Replies` and `Reply` is on it. A handler's mistake with its
client is an error code.

Harder: the ABI's version is 5 before anything is released, and every package
generated before this change refuses the library until it is regenerated. The
client handle has a state word, and every function that takes a client reads it
first. A header is of four kinds, and four functions read it. A handler cannot add
or command with its poll's client: it does so after the poll returns. A service's
poll counts as a call on its client, so a program whose other threads use a client
gives its service a client of its own. The Python binding holds the interpreter's
lock across every per-message function, and a service's poll, unlike the others,
can wait for the driver, at a session's opening: the Python client's change
decides how its service object polls.

Revisit: when a service must add or command from inside its handler, for a Rust
poll that lends its handler the client; when a client needs an action that stores
the position before the poll ends; when a client in another language needs the
frame of an entry; at the first release, which freezes the version (ADR 0003, ADR
0051).

## References

- `spec/c-abi.md`, "Handles", "Structs", "Functions", "Result codes", "Error codes",
  "Constants", "When a function cannot answer", "Calls during a claim or a poll",
  "Lifetimes", "Threads", "A frame's position", "What a header carries", "Versions
  and what a reader rejects", "Out of this version"; `spec/cluster.md`, "The
  service" and "Replies"
- `crates/cavalry-ffi/README.md`; `clients/README.md`
- ADR 0003, ADR 0046, ADR 0051, ADR 0052, ADR 0070, ADR 0080, ADR 0114, ADR 0118,
  ADR 0121, ADR 0122
- Work item 229 of the Brigade Board
- Aeron, read 2026-11-14 at `master` (1.55.0-SNAPSHOT):
  `aeron-client/src/main/c/aeronc.h`
  (<https://github.com/aeron-io/aeron/blob/master/aeron-client/src/main/c/aeronc.h>)
