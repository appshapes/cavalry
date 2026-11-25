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
read of a cluster's committed log beside a member; a `ClusterSession` is a client's
session with a cluster; a `ClusterService` is a cluster's service beside a member,
handed each committed entry and answering the sessions (all three below). Every message the client sends or
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

### The cluster session

A cluster session is a client's connection to a cluster: it sends the leader messages,
which the leader appends to the cluster's log, and reads the replies of the service
beside the leader (`spec/cluster.md`, "Sessions"). `client.connectCluster(members,
responseChannel)` opens one and returns a `ClusterSession`. `members` is the members
string the cluster's members were started with (`docs/how-to/run-a-cluster.md`), and
`responseChannel` a channel of this host for the cluster's answers,
`cavalry:udp?endpoint=<host>:<port>`. The call returns once the leader has opened the
session and waits for that for 20 s at most; it throws `CavalryError` with
`CAVALRY_ERR_CLUSTER_MEMBERS`, `CAVALRY_ERR_CLUSTER_REFUSED`,
`CAVALRY_ERR_CLUSTER_VERSION` or `CAVALRY_ERR_CLUSTER_CONNECT_TIMEOUT`.

`session.offer(message)` sends the leader a `Buffer`, or another typed array, of at most
`session.maxMessageLength` bytes, and returns the position after the message on the
session's publication to the leader, or a publication's result code below zero:
`cavalry.CLOSED` for a session that is closed, and `cavalry.ERROR` with
`CAVALRY_ERR_TOO_LONG` for a message that is too long. A message taken is on its way to
the leader and is not committed. `session.poll(handler, fragmentLimit)` calls
`handler(body, reply)` for each reply: `body` is the reply's payload, a `Buffer` into
the log for the call, and `reply` carries `reply.leadershipTermId`, the term of the
entry the reply answers, and `reply.timestamp`, the cluster's time when its leader
appended that entry, in milliseconds since 1970-01-01T00:00:00Z. Both are numbers.
`reply` is one object the session keeps, valid for the call: once the handler has
returned, both read `undefined` and a kept `body` reads as empty, so copy what the
program keeps. The poll returns the count of replies handed over, or −1 when it stopped
on a fault, which `session.fault()` gives as `{ code, sessionId, position }` until the
next poll. The poll is the session's duty cycle as well: it sends the keepalive,
follows a new leader and closes the session when the cluster says so. Call it at
least every 500 ms.

A reply is no delivery. The terms of the replies a handler is given never fall: a
reply of a term below one already handed over is stale, is passed over, and is counted
in `session.staleReplies()` (ADR 0121). A message whose leader was lost before it
committed is answered by nobody. A program that needs an answer offers again when
none comes. `session.closed()` is `null` while the session is open, and then the
reason: `"TIMEOUT"`, `"NO LEADER"` or the detail of the cluster's closed event.
`session.leaderMemberId()` and `session.leadershipTermId()` say whom the session
sends to, and `session.sessionId` is the id the leader gave it. `session.close()`
offers the leader a session close for at most 500 ms and frees the session; a second
close does nothing. Close a session before its client (`spec/c-abi.md`, "Functions",
cluster session; ADR 0122).

### The cluster service

A cluster's service is the program beside each member that applies the committed
entries and answers the sessions (`spec/cluster.md`, "The service" and "Replies").
`client.clusterService()` makes one beside the member on the client's driver and
returns a `ClusterService`; it sends nothing. `service.poll(handler, fragmentLimit)`
opens the member's log when the member has one, and calls `handler(body, entry,
service)` for each committed entry, in order:

- `body` is a session message's payload, a `Buffer` into the log for the call. It is
  empty for every other entry.
- `entry` carries the entry's values: `entry.type`, one of
  `constants.CAVALRY_CLUSTER_ENTRY_SESSION_MESSAGE`, `_SESSION_OPEN`,
  `_SESSION_CLOSE`, `_NEW_LEADERSHIP_TERM`, `_OTHER`, `_SNAPSHOT` and `_TIMER`, a timer
  of the program's that is due, with its id in `entry.correlationId`, a BigInt, which
  is `0n` for every other entry ("A service's timers", below); `entry.position`;
  `entry.leadershipTermId`; `entry.timestamp`, the cluster's time, the one clock a
  service reads, so that every member's service reaches the same state;
  `entry.sessionId`, −1 where the entry has no session; `entry.leaderMemberId`, for a
  new leadership term; and `entry.timedOut`, for a session close. `entry` is one
  object the service keeps, valid for the call: once the handler has returned, every
  one of them reads `undefined`.
  `_SNAPSHOT` is the leader's request for a snapshot: the poll ends after it, and
  the program gives its state to `service.snapshot` before its next poll ("A
  service's snapshot and its start", below).
- `service` is the service that was polled, for `service.reply`,
  `service.scheduleTimer` and `service.cancelTimer`.

What the handler returns says what becomes of the entry. Nothing, or
`cavalry.CONTINUE`, takes it, and the poll goes on. `cavalry.BREAK` takes it and ends
the poll. `cavalry.ABORT` leaves it and ends the poll, and the next poll hands it over
again. The poll returns the count of entries taken, or −1 when it stopped, on a fault
or at a client its driver closed, which `service.fault()` gives until the next poll.
A poll with a client its driver closed, as when the member's driver has stopped, is −1
with `CAVALRY_ERR_CLOSED` as the fault's code, at every poll, so a program's loop ends
on it (ADR 0155). A poll the ABI refuses before it reads is −1 too, and leaves `service.fault()` as it
was: a poll after the program's own `client.close()` and a poll from inside a
handler of the same service among them.

`service.reply(sessionId, message)` answers a session from inside the handler and
returns what became of the reply: `CAVALRY_CLUSTER_REPLY_SENT`;
`CAVALRY_CLUSTER_REPLY_NOT_ANSWERING`, from a service whose member did not lead the
entry's term, which has nothing to do; `CAVALRY_CLUSTER_REPLY_RETRY`, when the
session's response channel did not take the reply now, for which the handler returns
`cavalry.ABORT`; `CAVALRY_CLUSTER_REPLY_DROPPED`; `CAVALRY_CLUSTER_REPLY_NO_SESSION`;
or `CAVALRY_CLUSTER_REPLY_TOO_LONG`. Every member's service makes the same calls for
the same entry, and one of them sends: the one beside the leader of the entry's term.
So a handler does this, in this order:

1. It replies. When the reply answers `CAVALRY_CLUSTER_REPLY_RETRY` it returns
   `cavalry.ABORT` and changes nothing.
2. It changes the service's state.
3. It returns.

An entry that was aborted is handed over again, and a reply that an earlier handling
of the same entry sent is not sent twice: the second answers
`CAVALRY_CLUSTER_REPLY_SENT` and sends nothing. A handler that throws, or returns
anything that is no action, has its entry aborted too, and `poll` throws `HandlerError`
("The exception"): an entry is taken only by a handler that finished with it, so
`service.position()`, the position after the last entry taken, never passes an entry
whose handler failed.

The handler is a plain function, which has finished with its entry when it returns.
An `async` function returns a promise before it has finished, and a promise is no
action: the entry is aborted and `poll` throws. A handler whose entry needs work that
is not done yet returns `cavalry.ABORT`, and the next poll hands the entry over again.
A boolean is no action either: a handler that returns `true` to say that it handled
its entry has it aborted, and `poll` throws. The addon reads a number alone as an
action, so `true` is never taken for 1, the value of `cavalry.ABORT`.

The service's poll and its close are calls on the client the service was made on, and
the poll holds that client for its run ("What it must never do"). `service.close()`
frees the service, and a second close does nothing; close a service before its client
(`spec/c-abi.md`, "Functions", cluster service; ADR 0123; ADR 0124).
`conformance/node/cluster.js` is a program that runs three services and a session
through every call above.

### A service's timers

A program that must act at a time, a session it gives up on or an order that
expires, asks for a timer. It never reads a clock of its own: every member's
service must reach the same state, and the cluster's time is the one clock they
share. From inside the handler:

- `service.scheduleTimer(correlationId, deadline)` schedules the timer
  `correlationId`, any int64 the program chooses, for `deadline`, in the cluster's
  time, the clock of `entry.timestamp`, in milliseconds. It returns
  `constants.CAVALRY_CLUSTER_TIMER_SCHEDULED`: the timer is pending with that
  deadline, whether the id was pending before or not. Or it returns
  `constants.CAVALRY_CLUSTER_TIMER_REFUSED`: the service holds its 1,024 pending
  timers, the id is none of them, and nothing changed.
- `service.cancelTimer(correlationId)` makes the timer pending no more, and returns
  0, whether it was pending or not.

When the deadline has come, the handler is handed an entry whose `entry.type` is
`constants.CAVALRY_CLUSTER_ENTRY_TIMER` and whose `entry.correlationId` is the
timer's id, with an empty `body`. Every member's service is handed it once, at one
position of the log, never before its deadline and never after its cancel.
`entry.timestamp` is the leader's clock when it appended the timer's entry, at or
after the deadline: later by as long as the leader's member took to be told of the
timer and to reach it, and by more where the cluster had no leader meanwhile. The
handler is called later again, once the entry is committed and read. A timer that
repeats is scheduled again by the handling of its own entry. A deadline at or
before the entry's time is due at once.

```js
function onEntry(body, entry, service) {
  if (entry.type === constants.CAVALRY_CLUSTER_ENTRY_SESSION_OPEN) {
    // Give the session a minute, by the cluster's clock, under an id of its own.
    service.scheduleTimer(entry.sessionId, entry.timestamp + 60000);
  } else if (entry.type === constants.CAVALRY_CLUSTER_ENTRY_SESSION_CLOSE) {
    service.cancelTimer(entry.sessionId);
  } else if (entry.type === constants.CAVALRY_CLUSTER_ENTRY_TIMER) {
    expire(Number(entry.correlationId)); // the program's own: the minute is over
  }
}
```

The block is a fragment, and nothing runs it: `expire` is the program's own. A whole
program, in Python, which `just conformance` runs, is in [Schedule a timer in a
cluster's service](../../docs/how-to/schedule-a-services-timer.md).

Four rules come with them (`spec/cluster.md`, "The service", "Timers"; ADR 0151;
ADR 0157):

- **Only inside the handler.** The timers are then a function of the entries
  taken, as the program's state is, and the same beside every member. From
  outside the handler both calls return −1, as `service.reply` does there, and
  change nothing. A handler that returns `cavalry.ABORT`, or throws, is handed its
  entry again with every timer as it was before that handling. The
  timers are part of what `service.snapshot` writes and `service.restore` reads,
  with no call of the program's.
- **An id and a deadline are int64, and exact.** Each is a BigInt, any int64, or a
  number that is an integer from −(2^53 − 1) to 2^53 − 1, the integers a number
  carries exactly, as `entry.timestamp + 60000` is. A number with a fraction or
  outside that range, a `NaN` among them, and a BigInt outside int64, throw a
  `RangeError` before the ABI is called: N-API would pass such a number or BigInt
  as another value, another timer or another time, and say nothing. A value that
  is neither a number nor a BigInt throws a `TypeError` there. Every other integer
  argument of the package is read as exactly ("What it must never do"). Thrown in the
  handler, it aborts the entry as any
  exception of the handler's does ("The exception"). `entry.correlationId` comes
  back as a BigInt whatever the id was scheduled as: `77n === 77` is false, so a
  program compares it with a BigInt, or reads `Number(entry.correlationId)` where
  its ids are safe integers.
- **At most 1,024 pending.** A program with more deadlines than that keeps them
  in its own state and schedules one timer, for the earliest: when it is handed
  that timer it acts on every deadline that has come and schedules the timer
  again for the earliest that is left.
- **Every member at version 2 first.** A program schedules its first timer only
  once every member of its cluster, with the service beside it, runs version 2 of
  the cluster protocol, and from that entry on no member is stepped back to
  version 1 (`docs/how-to/upgrade-a-cluster.md`): a service of version 1 hands its
  program no timer, so its program would compute another state from the same log.

Both calls are one ABI call each, which allocates nothing and waits for nothing.
What they changed is told to the service's member by the poll, once the handler
has taken the entry, and the poll in which a program first schedules a timer
sends six commands more, once in a service's life ("The call").
`conformance/node/cluster.js` schedules timers and cancels one beside three
members; what it holds, and what rests on the library's own tests, is in
`conformance/README.md`.

### A service's snapshot and its start

A service's program holds the cluster's state, and a snapshot is that state as the
program's own bytes at one position of the log, the same beside every member. A
service that starts again takes its state from its last snapshot and reads the log
after it, where it would read the log from position 0 (`spec/c-abi.md`, "A service's
snapshot and its start"; ADR 0146; ADR 0149). Three calls, none of them from inside
the handler:

- `service.restore()`, before the first poll, returns `{ restored, state }`.
  `constants.CAVALRY_CLUSTER_RESTORED_STATE`: `state` is the program's state as it
  gave it, a `Buffer` that is a copy the caller owns; the polls read the log from the
  snapshot's position. `CAVALRY_CLUSTER_RESTORED_FROM_START`: `state` is `null`, and
  the polls read from position 0. `CAVALRY_CLUSTER_RESTORED_NO_MEMBER`: `state` is
  `null`; the driver hosts no member with a log yet, and the program calls again
  before it polls.
- `service.snapshotDue()`, after a poll, is the snapshot's log position when the
  poll ended at the leader's request, an entry whose `type` is
  `constants.CAVALRY_CLUSTER_ENTRY_SNAPSHOT`, and `null` otherwise.
- `service.snapshot(state)`, before the next poll, writes the snapshot and tells the
  member, which lists it. `state` is a `Buffer` or a `Uint8Array`, 1 GiB at the
  most. It returns `true`, or `false` when the member lists one at or past that position
  already.

So a program's loop is this:

```js
function start() {
  const found = service.restore();
  if (found.restored === constants.CAVALRY_CLUSTER_RESTORED_NO_MEMBER) {
    setTimeout(start, 1);                        // no member with a log yet: ask again
    return;
  }
  if (found.state !== null) load(found.state);   // the program's own: bytes to its state
  turn();
}
function turn() {
  if (service.poll(onEntry, 10) < 0) {
    report(service.fault());                     // the poll stopped: the fault's code
    return;                                      // says on what
  }
  if (service.snapshotDue() !== null) {
    try {
      service.snapshot(dump());                  // the program's own: its state to bytes
    } catch (failed) {
      report(failed);                            // nothing committed is lost: poll on,
    }                                            // and the next request asks again
  }
  setImmediate(turn);
}
start();
```

A poll that returns −1 has stopped, and the loop ends on it: code 102,
`CAVALRY_ERR_CLOSED`, is the close of the service's client, which every poll after it
answers again, as when the member's driver has stopped (ADR 0155). A loop that does
not look at what the poll returned polls a stopped driver for ever.

The state must be a function of the entries the handler was handed, and of nothing
else: every member's service gives its own, and a start from any of them must reach
what the others hold. A program that never calls `restore` reads the log from
position 0 at every start, and its member's log can then not be purged.

`snapshot` and `restore` wait for the driver and hold the thread while they do, as an
add does: a second at the most for each step of a snapshot's own, the response timeout,
10 s, for an add or a command that a driver does not answer, and for as long as the
snapshot's recording takes to play back at a start. Each throws `CavalryError` with
the ABI's code: `CAVALRY_ERR_CLUSTER_NO_SNAPSHOT_DUE`,
`CAVALRY_ERR_CLUSTER_SNAPSHOT_TOO_LONG`, `CAVALRY_ERR_CLUSTER_SNAPSHOT_FAILED` with
the step in the message, `CAVALRY_ERR_CLUSTER_RESTORE_TOO_LATE` once the service has
begun, and `CAVALRY_ERR_POLLING` from inside a handler. After an error of `snapshot`
the snapshot is still due, and the program calls again or polls on.
`conformance/node/snapshot.js` is a program that runs every call above beside three
members.

## What it must never do

- Never run a message through anything but one ABI call: no lock, no `await`, no
  promise, no copy of a fragment into a new array for the handler. `clients/lint.py`
  reads the per-message functions of `index.js` for JavaScript's forms of each
  (`async`, `await`, `Promise`, `.then`, `throw`, `Atomics.wait`), and those of
  `src/addon.c`, the trampoline, the Buffers' finalizer, `poll`, `committedLogPoll`,
  `headerField`, `view`, `detach` and the wrappers of the claim, the reserved value, the commit, the abort
  and the offer, and for a cluster session and a cluster service the trampoline of
  their polls, `clusterSessionPoll`, `clusterServicePoll`, `replyField`, `entryField`
  and the wrappers of the session's offer and of the service's reply, its action and
  the schedule and the cancel of its timers, for
  C's (`pthread_mutex_*`, `pthread_rwlock_*`, `mtx_*`,
  `uv_mutex_*`, `uv_rwlock_*`, `napi_*threadsafe_function*`, and `napi_throw*` and
  `napi_fatal_*` outside the argument readers). `just lint` fails on any of them, in
  the function's own body or in a function of the package it calls. An argument
  reader throws to refuse an argument, so the lint holds where one is called too: by
  a function N-API calls with the caller's arguments, before its call of the ABI,
  and never by a trampoline or a function one calls.
- Never pass the ABI another integer than the caller gave
  ([ADR 0162](../../docs/decisions/0162-refuse-in-a-generated-client-an-integer-argument-its-parameter-cannot-hold-before-the-abi-is-called.md)).
  N-API's own conversions
  change a number that is no integer its parameter holds, and say nothing:
  `napi_get_value_int64` drops a fraction, reads a `NaN` or an infinity as 0 and
  holds a number past int64 to the nearest int64, and `napi_get_value_int32` drops
  a fraction, reads the same two as 0 and keeps the low 32 bits of the rest. A truncate
  at `NaN` would be a truncate at 0, which empties a recording, and a subscription
  to stream `2 ** 32 + 1001` one to stream 1001. So the addon calls neither, and
  its readers refuse such a value with a `RangeError` before the ABI is called:
  - An int64 parameter (a position, a recording's id, a replay's length and its
    session, a service's session, the reserved value, a timer's id and its
    deadline) takes a number that is an integer from −(2^53 − 1) to 2^53 − 1, the
    integers a number carries exactly, or a BigInt from −2^63 to 2^63 − 1.
  - A count or a length (a fragment limit, a claim's length, an image's index)
    takes a number that is an integer from 0 to 2^53 − 1, or a BigInt from 0 to
    2^63 − 1. A count below zero was a `TypeError` before; it is a `RangeError`.
  - An int32 parameter (a stream id) takes a number that is an integer from −2^31
    to 2^31 − 1, and no BigInt.

  A value of another type throws a `TypeError`. Each message names the parameter
  as the method names it, `recordingId`, `fragmentLimit`, `message`
  ([ADR 0164](../../docs/decisions/0164-refuse-a-count-below-zero-in-the-go-the-jvm-and-the-dotnet-client-before-the-abi-is-called.md)),
  and its range, says what to pass and names this section; a timer's names "A service's timers". The types
  of `index.d.ts` say `number | bigint` for each int64 and each count, so the
  follow length, `constants.CAVALRY_FOLLOW_LENGTH`, a BigInt, is a length
  `startReplay` takes: as the number `2 ** 63` it is refused, where N-API held it
  to the follow length. The readers are the same for a per-message function and
  for a command, and cost a number and a BigInt the N-API calls their conversions
  cost ("The call"). The action a service's handler returned is held to an int32
  the same way in the trampoline, where one outside it is no action and aborts the
  entry. `clients/generate-test.py` names every integer the addon reads from an
  argument with its reader, compiles the readers against a stand-in for N-API and
  runs them over each kind of value, and the conformance runner makes sixteen such
  calls through the built addon, truncates, purges, adds, polls and claims,
  against a recording that the replay then reads whole, and offers a
  `Float16Array` where the runtime has one.
- Never give the ABI another length than the bytes the caller offered. `offer`, a
  session's `offer`, a service's `reply` and its `snapshot` take a `Buffer`, an
  `ArrayBuffer`, a `DataView` or a typed array, and the length is the bytes it
  holds: the addon multiplies a typed array's element count by the width of its
  kind. A kind the addon does not name is refused with a `TypeError` that says to
  pass a `Uint8Array` over the same bytes. `Float16Array`, which Node has from
  version 24, is such a kind: before, the addon took its elements for 8 bytes wide
  and handed the ABI four times its length. The refusal does not wait for N-API to
  name the kind, which Node 24.0 to 24.13 do not: a kind N-API leaves unsaid is
  refused as one the addon does not name.
- Never throw for an `Offer` code: `tryClaim` and `offer` return the position after
  the frame or the code below zero (`NOT_CONNECTED`, `BACK_PRESSURED`, `ADMIN_ACTION`,
  `CLOSED`, `MAX_POSITION_EXCEEDED`, `ERROR`), as the ABI does; the caller branches.
  A cluster session's `offer` returns the same codes, and a cluster service's `reply`
  returns its `CAVALRY_CLUSTER_REPLY_*` code, `scheduleTimer` its
  `CAVALRY_CLUSTER_TIMER_*` code and `cancelTimer` 0, or −1 from outside the handler.
- Never swallow the handler's exception, or a fragment: a fragment handler that throws
  keeps every later fragment of that poll reaching the handler, a fragment the runtime
  keeps from the handler is counted, and `poll` throws a `HandlerError` once the ABI
  has returned, with the first exception or failure as its `cause` and the counts, the
  one throw the per-message path allows. Never hand a committed log's handler an entry
  after one the runtime kept from it: the rest of that poll is counted too, and
  `HandlerError.undeliveredAt` is where the service opens the log again.
- Never take an entry for a cluster service's handler that failed. The handler's
  return value is the entry's action, so the addon's trampoline answers for a handler
  that threw or returned no action, and for an entry the runtime kept from the
  handler: it aborts the entry, the poll ends there, and `poll` throws. The entry is
  handed over again; it is never skipped.
- Never lose a handle the ABI keeps open. Every close frees its handle, whatever it
  returns, so `close()` forgets the handle once the ABI's close has returned. There
  are four exceptions: the close of a subscription, of a committed log, of a cluster
  session and of a cluster service, each from inside a handler of its own poll; a
  cluster service's close is refused from inside the handler of another service whose
  poll holds its client, too. The ABI refuses
  each with `CAVALRY_ERR_POLLING` and leaves the handle open (`spec/c-abi.md`, "Calls
  during a claim or a poll"). `close()` then throws `CavalryError` and keeps the
  handle, so that it can be polled and closed after the poll. A publication's close zeroes the claim struct too, so `commit`, `abort` and `setReservedValue` after it answer -1 with
  `CAVALRY_ERR_NO_CLAIM` and the ABI reads no freed handle through the struct's
  `publication` (`spec/c-abi.md`, "Which claim a struct names").
- Never read a refusal as an answer. A `poll` from inside a handler of the same
  subscription returns −1 with `CAVALRY_ERR_POLLING`, `subscription.imageCount()`
  returns −1 there and `subscription.images()` throws `CavalryError` with that code,
  where it returned an empty array, and the poll in progress goes on. A committed log's handler is held to the same rule. From inside it, a `poll` of
  the same log, `log.position()` and `log.commitPosition()` return −1, `log.isEnded`
  is `false`, `log.failedAt()`, `log.withdrawnAt()` and `log.faulted()` are `null`,
  and `log.fault()` and `log.close()` throw `CavalryError`, each with
  `CAVALRY_ERR_POLLING`, and the log stays open. The `false` and the three `null` are
  refusals there, not answers: read the four after the poll has returned. The handler
  has each entry's own `position`. A cluster session's handler and a cluster
  service's are held to it as well. From inside a session's handler, `offer` returns
  `cavalry.ERROR`, a `poll` of the same session, `leadershipTermId()`,
  `leaderMemberId()` and `staleReplies()` return −1, `closed()` is `null`, a refusal
  there, and `fault()` and `close()` throw `CavalryError`, each with
  `CAVALRY_ERR_POLLING`: a handler that answers a reply with a message keeps what it
  needs and offers after the poll has returned. From inside a service's handler, a
  `poll` of the same service, `position()` and `sessions()` return −1, `answers()` is
  `false`, a refusal there, and `fault()` and `close()` throw `CavalryError`. The
  service's poll holds its client too, so every call on that client from inside the
  handler is refused the same way: `client.isClosed` is `false`, a refusal, and an
  add, a command, `client.committedLog`, `client.connectCluster` and
  `client.clusterService` throw `CavalryError` with `CAVALRY_ERR_POLLING`. Make them
  after the poll has returned.
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
since N-API would otherwise keep its low 64 bits, another value. Every integer
argument is read as strictly, `int64_read` and `int_arg` in the addon: a number must
be an integer that a number carries exactly and its parameter holds, or it is refused
the same way ("What it must never do"). A reader makes one N-API call for a number,
`napi_get_value_double`, where the conversion it replaces made one,
`napi_get_value_int64` or `napi_get_value_int32`. For a BigInt that call answers
that the value is no number, and the reader makes a second,
`napi_get_value_bigint_int64`: two calls, as the type's call and the BigInt's were
two before.
Node has one
thread for JavaScript, so a call holds it for the
call, and an add, a command or a close, which waits on the driver for up to 10 s
(`spec/c-abi.md`, "Threads"), holds it as long: the client is synchronous, and a
program that must not block its event loop runs the client on a worker thread.
`client.connectCluster` holds it until the leader has opened the session, 20 s at
most.

A cluster service's poll is the one per-message function that can wait for the driver.
It sends commands with the service's client when it opens the member's log, at a
session's opening, where a term's leader changes and at a recording's end, and six
more once in a service's life, at the most, its five counters and its publication
to its member, in the poll in which its program first schedules a timer or the
service first passes a timer event over. It
waits for each answer on the calling thread; a session message costs no command. A
driver that refuses one of the six is that poll's −1, after the entries its handler
took are taken, and the service adds them again at a later poll (ADR 0159). What a
service tells its member of its timers after that, each pending timer again every
second among it, is offered and waits for nothing. The
runner's cluster round reports the waits. On an Apple M5 Pro laptop (macOS 26.6.1,
Node v24.16.0), in each of 10 runs, 7 of about 42,500 polls of three services took
over 1 ms, where 4 of about 18,500 did in each of 25 runs before the round scheduled
a timer. The report names the longest poll alone: in every run it was a poll in which
a service's program first scheduled a timer, 23.6 to 36.9 ms with its six
commands, where the longest before, a poll that opened a member's log, took 7.2 to
10.4 ms. A
program whose event loop cannot stop for that long runs its service on a
`worker_threads` worker, with a `Client` of its own.

### Threads

A `Client` and the handles it makes are used from the thread that made them, one call
at a time; the client adds no lock, and the ABI runs one call on a handle at a time.
A program with several threads gives each `worker_threads` worker its own `Client`:
the addon's per-call state, the flags its argument readers set, is per thread, and its
per-subscription state belongs to the subscription. A `ClusterService`'s poll and its
close are calls on its client as well as on the service, so the service is used from
the thread that made its client.

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

A cluster session's poll and a cluster service's have a trampoline of their own. For a
reply it makes the same eight N-API calls, and before them one ABI call,
`cavalry_cluster_reply_values`, which fills the struct the addon keeps for the session.
For a service's entry that ABI call is `cavalry_cluster_entry_values`. The handler is
handed the service as a third argument, which is the value `poll` was called on and
costs no N-API call. The trampoline then makes a ninth N-API call, `napi_typeof`, for
what the handler returned, and a tenth, `napi_get_value_double`, when that is a
number. `cavalry.BREAK` and `cavalry.ABORT` cost one ABI call more,
`cavalry_cluster_service_action`; nothing and `cavalry.CONTINUE` cost none. The
getters of `reply` and `entry` each call the addon's `replyField` or `entryField`,
which returns one value of that struct: a number, a boolean for `entry.timedOut`,
or a BigInt for `entry.correlationId`, which V8 makes on its heap at each read, so
a program reads it once for an entry that is a timer. A timestamp is above 2^31, so each read of `reply.timestamp` or `entry.timestamp`
makes a `HeapNumber` on V8's heap, as the position of an entry past 2 GiB does. An
entry that is no session's message has a body of no bytes, and the trampoline makes
its `Buffer` all the same. `clients/generate-test.py` pins these calls too.

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

A cluster session's poll makes one `Buffer` for each reply, and a cluster service's one
for each entry, an aborted entry's each time it is handed over. They are freed in the
same way. The runner's cluster round counts the `Buffer`s its handlers were handed and
fails unless one turn of the event loop frees them all.

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

A cluster session's handler is held as a subscription's is: every reply the poll hands
over reaches it, and the `HandlerError`, whose `cluster` is `"session"`, speaks of
replies and names `session.fault()`. A reply the runtime kept from the handler is
counted in `undelivered`. The `Error` for it names the reply by its leadership term id
and the cluster's time, its `leadershipTermId` and `timestamp` properties too.

A cluster service's handler is the one whose failure stops its poll, because what it
returns is read: it is the entry's action. When the handler throws, the trampoline
keeps the exception and aborts the entry with `cavalry_cluster_service_action`, from
inside the ABI's call, so the ABI ends the poll there with the entry not taken. `poll`
then throws `HandlerError`, whose `cluster` is `"service"`: `fragments` is the count
of entries the poll took before that entry, `raised` is 1, and `cause` is the
handler's exception. A handler that returns a value that is no action is treated the
same way. That is a number the ABI refuses, such as 3, and anything else that is not
`undefined`, `null` or a whole number, such as `true` or the promise an `async`
handler returns.
The cause is then a `CavalryError` with `CAVALRY_ERR_CLUSTER_ACTION` that names the
value. An entry the runtime kept from the handler is aborted too: `undelivered` is 1,
and the cause is the addon's `Error`, which names the entry by its position and its
leadership term id, its `position` and `leadershipTermId` properties too. So a
service's `HandlerError` has no `undeliveredAt`: no entry was consumed unseen. The
next poll hands the aborted entry over again, and throws again until the handler
takes it: the service does not skip an entry, since every member's service must apply
the same entries. A reply the failed handling sent is not sent twice. What the handler
changed of its own state before it threw is the handler's to guard, which is why it
replies first and changes its state last ("The cluster service"). The runner checks
it: a handler that replies and then throws is handed its entry twice at one position,
and the session reads one reply.

`clients/generate-test.py` compiles the generated trampoline against a stand-in for
N-API that fails each of its calls in turn, and checks that every fragment the handler
did not return from is counted once, with the first failure kept. For a committed log
it checks that the position kept is the one the ABI gave, and that a later entry of
the same poll makes no N-API call and does not reach the handler. It compiles the
addon's `failure_error` and `report` too, and checks the position each reports and the
cause's message. It compiles the cluster polls' trampoline against the same stand-in.
There it checks that every way a service's entry does not get through its handler
makes one call that aborts it, that an entry handed over after it in the same poll
makes no N-API call and is aborted unseen, and what each kind of value a handler
returns does.

## What to read next

- `spec/c-abi.md`: the page the package wraps, function for function.
- `clients/README.md`: how a client is generated and what every client passes.
- `clients/python/README.md`: the first client, whose shape this one copies.
- `conformance/node/`: the runner and its report.
- `docs/decisions/0046-build-the-c-abi-and-the-clients-fourth.md`: the slice this
  client is the third feature of.
- `docs/decisions/0124-give-each-client-a-session-object-and-a-service-object-whose-handlers-failure-aborts-its-entry.md`:
  the shape of the cluster session and the cluster service in every client, and why a
  failed handler's entry is aborted.
- `docs/how-to/run-a-cluster.md`: how to start the members a session connects to.
