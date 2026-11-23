# cavalry, the Go client

## What this does

A Go module over the C ABI of `spec/c-abi.md`, generated from the committed header by
`clients/generate.py go`: the package `cavalry` (module
`github.com/appshapes/cavalry/clients/go`) reaches the ABI through cgo, compiled
against the header and linked with `cavalry-ffi`'s static library, and gives the
handles the forms a Go user expects. A `Client` closes with `Close`; a `Publication`
claims a slice of the log as a `[]byte` over it or offers a `[]byte`; a `Subscription`
polls with a fragment handler function that receives the body as a `[]byte` into the
log and the frame's `Header`; a `Recording` and a `Replay` wrap the driver's archive; a
`CommittedLog` is a service's read of a cluster's committed log beside a member; a
`ClusterSession` is a client's session with a cluster; a `ClusterService` is a
cluster's service beside a member, handed each committed entry and answering the
sessions (all three below).
Every message the client sends or receives goes through one `extern "C"` call on the
caller's goroutine, as the ABI's page promises (ADR 0046's boundary; ADR 0051). The
module has no dependency beyond Go's standard library: cgo is part of the Go toolchain,
and the C compiler it needs is the one `cavalry-ffi`'s own build needs.

cgo's pointer rules decide what crosses the boundary (ADR 0046, the Go client). The ABI
keeps the `clientd` it is given for the length of a poll and hands it back to the
handler, and C must not hold a Go pointer, so the client passes the subscription's
index in a bounded table of 4,096 entries, and the trampoline reads the subscription
back from the table with one atomic load. A slice over the log is made with
`unsafe.Slice` from the address the ABI gives, which allocates nothing; a slice of the
caller's bytes is passed to `cavalry_publication_offer` as the address of its first
element, which cgo allows because the bytes hold no Go pointer and the ABI copies them
before it returns.

### The committed log

A service is a program beside a cluster member that applies the entries the cluster
has committed (`docs/how-to/read-the-committed-log.md`). `client.CommittedLog(position)`
opens the log of the member on the client's driver at a position, 0 or the `Position`
an earlier run reached, and returns `nil` and no error while the driver hosts no
member with a log, so a service started before its member asks again. `log.Poll(handler,
fragmentLimit)` calls `handler(body, header, position)` for each committed entry, in
order, none at or past the member's commit position; `position` is the entry's
position in the log. It returns the entry count and an error. The count is −1 when the
ABI's poll stopped on a fault, and the error is then `nil`: the error `Poll` returns
is a handler's panic alone, so check the count. After a −1, `log.Err()` is the
failure, an `*Error` whose `Code` is the fault's, kept until the log's next failure,
and `log.Fault()` gives the fault's code, session and position until the next poll.
`log.Position()` is the position after the last entry delivered, the one a service
keeps for its next start, and `log.CommitPosition()` the bound of the last poll.
`log.IsEnded()` says that no poll will deliver another entry: stop reading when it is
true, not when a poll returns 0, then close the log and open another from
`log.Position()`. `log.Close()` stops the driver's replay and is idempotent; a log
closed after its client is freed, and the driver ends the replay by itself. When the
ABI could not stop the replay, `log.Close()` returns the `*Error` after it has freed
the log and its slot, and a second `Close` returns `nil`. `log.MemberID` and
`log.RecordingID` say whose log it is (`spec/c-abi.md`, "Functions", committed log;
ADR 0070).

An ended log delivered every committed entry it may read only when three things hold:

- `log.FailedAt()` is 0 and `false`. A position and `true` is where the driver's
  replay of the member's recording failed, and no entry from it on was delivered.
- `log.WithdrawnAt()` is 0 and `false`. A position and `true` is where the entries
  begin that the log was not given, because the driver withdrew the replay, 5 s after
  it ended, before the service had read it.
- `log.Faulted()` is zeros and `false`. A code, a session, a position and `true` is
  where this client's read of the replay's image, the log's own, stopped: the code is
  `ErrFaultImage` when a frame of the image failed a check, or when the image was gone
  before this client read it, and `ErrFaultImagesFull` when the image could not
  join. No entry from the fault on was delivered.

Read the three reports once `log.IsEnded()` is true. The driver withdraws a replay
that failed as it does any other, so the first two can both be `true`: the position of
`log.WithdrawnAt()` is then the lower, and the entries from it on, those below the
position of `log.FailedAt()` among them, were not delivered. The poll that meets the
image's fault returns −1 once, `log.Fault()` answers it until the next poll, which
returns 0 and clears it there, and `log.Err()` until the log's next failure.
`log.Faulted()` answers it from that poll on, for as long as the log is held
(ADR 0080). `log.WithdrawnAt()` is `false` for such a log, and `log.FailedAt()` is
`true` only when the driver's replay failed as well, which is then the cause to look
at first. In every case what the log delivered ends at `log.Position()`.

A committed log is found by its poll's call-back as a subscription is, by its slot in
the table, so an open log holds one slot of the table of 4,096 ("Threads"), which its
`Close` frees, and `Registered` counts it; its trampoline is its own ("The call-back,
by construction"). A handler's panic goes where a subscription's goes ("The
panic"): every entry the ABI delivers reaches the handler, and `Poll` returns a
`*HandlerError` once the ABI has returned, with the poll's count as its `Fragments`,
the −1 of a fault included. An entry whose handler call panicked was delivered and is
not delivered again: `log.Position()` is past it, and past every entry the same poll
delivered after it. A handler cannot stop a poll: the ABI goes on handing it entries
up to the fragment limit. So a service that must apply every entry, in order, does
this:

1. The handler recovers its own panics and handles its own errors. When it cannot
   apply an entry, it keeps that entry's `position` and sets a flag.
2. While the flag is set the handler applies nothing and returns at once, for the rest
   of that poll: the poll still hands it the entries after the one that failed.
3. After the poll, the service closes the log and opens a new one at the position it
   kept.

With entries at 0, 96 and 192 and a failure at 96, opening again at `log.Position()`
would lose the entry at 96, and applying 192 before opening again at 96 would apply
192 twice. A `fragmentLimit` of 1 is the simple form: a poll then delivers at most one
entry, and the service decides after each.

### The cluster session

A cluster session is a client's connection to a cluster: it sends the leader messages,
which the leader appends to the cluster's log, and reads the replies of the service
beside the leader (`spec/cluster.md`, "Sessions"). `client.ConnectCluster(members,
responseChannel)` opens one and returns a `*ClusterSession`. `members` is the members
string the cluster's members were started with (`docs/how-to/run-a-cluster.md`), and
`responseChannel` a channel of this host for the cluster's answers,
`cavalry:udp?endpoint=<host>:<port>`. The call returns once the leader has opened the
session and waits for that for 20 s at most; its `*Error` has `ErrClusterMembers`,
`ErrClusterRefused`, `ErrClusterVersion` or `ErrClusterConnectTimeout`.

`session.Offer(message)` sends the leader a `[]byte` of at most
`session.MaxMessageLength` bytes, and returns the position after the message on the
session's publication to the leader, or a publication's result code below zero:
`PublicationClosed` for a session that is closed, and `PublicationError`, with
`ErrTooLong` in `session.Err()`, for a message that is too long. A message taken is on
its way to the leader and is not committed. `session.Poll(handler, fragmentLimit)`
calls `handler(body, reply)` for each reply: `body` is the reply's payload, a `[]byte`
into the log for the call, and `reply` is a `*ClusterReplyValues` with
`reply.LeadershipTermID`, the term of the entry the reply answers, and
`reply.Timestamp`, the cluster's time when its leader appended that entry, in
milliseconds since 1970-01-01T00:00:00Z. `reply` is the one struct the session keeps,
written for each reply, so copy a field out to keep it. The poll returns the count of
replies handed over, or −1 when it stopped on a fault, with `session.Err()` saying why
and `session.Fault()` giving the fault until the next poll. The poll is the session's
duty cycle as well: it sends the keepalive, follows a new leader and closes the
session when the cluster says so. Call it at least every 500 ms.

A reply is no delivery. The terms of the replies a handler is given never fall: a
reply of a term below one already handed over is stale, is passed over, and is counted
in `session.StaleReplies()` (ADR 0121). A message whose leader was lost before it
committed is answered by nobody. A program that needs an answer offers again when
none comes. `session.Closed()` is `""` and `false` while the session is open, and then
the reason and `true`: `"TIMEOUT"`, `"NO LEADER"` or the detail of the cluster's closed
event. `session.LeaderMemberID()` and `session.LeadershipTermID()` say whom the
session sends to, and `session.SessionID` is the id the leader gave it.
`session.Close()` offers the leader a session close for at most 500 ms and frees the
session; it is idempotent. Close a session before its client (`spec/c-abi.md`,
"Functions", cluster session; ADR 0122).

### The cluster service

A cluster's service is the program beside each member that applies the committed
entries and answers the sessions (`spec/cluster.md`, "The service" and "Replies").
`client.ClusterService()` makes one beside the member on the client's driver and
returns a `*ClusterService`; it sends nothing. `service.Poll(handler, fragmentLimit)`
opens the member's log when the member has one, and calls `handler(body, entry,
service)` for each committed entry, in order:

- `body` is a session message's payload, a `[]byte` into the log for the call. It is
  `nil` for every other entry.
- `entry` is a `*ClusterEntryValues`: `entry.Type`, one of `ClusterEntrySessionMessage`,
  `ClusterEntrySessionOpen`, `ClusterEntrySessionClose`, `ClusterEntryNewLeadershipTerm`,
  `ClusterEntryOther` and `ClusterEntrySnapshot`; `entry.Position`;
  `entry.LeadershipTermID`;
  `entry.Timestamp`, the cluster's time, the one clock a service reads, so that every
  member's service reaches the same state; `entry.SessionID`, −1 where the entry has
  no session; `entry.LeaderMemberID`, for a new leadership term; and `entry.TimedOut`,
  for a session close. It is the one struct the service keeps, written for each
  entry, so copy a field out to keep it.
  `ClusterEntrySnapshot` is the leader's request for a snapshot: the poll ends after
  it, and the program gives its state to `service.Snapshot` before its next poll ("A
  service's snapshot and its start", below).
- `service` is the service that was polled, for `service.Reply`.

What the handler returns says what becomes of the entry. `ActionContinue` takes it,
and the poll goes on. `ActionBreak` takes it and ends the poll. `ActionAbort` leaves it
and ends the poll, and the next poll hands it over again. The poll returns the count
of entries taken, or −1 when it stopped on a fault, with `service.Err()` saying why
and `service.Fault()` giving the fault until the next poll.

`service.Reply(sessionID, message)` answers a session from inside the handler and
returns what became of the reply: `ClusterReplySent`; `ClusterReplyNotAnswering`, from
a service whose member did not lead the entry's term, which has nothing to do;
`ClusterReplyRetry`, when the session's response channel did not take the reply now,
for which the handler returns `ActionAbort`; `ClusterReplyDropped`;
`ClusterReplyNoSession`; or `ClusterReplyTooLong`. Every member's service makes the
same calls for the same entry, and one of them sends: the one beside the leader of the
entry's term. So a handler does this, in this order:

1. It replies. When the reply answers `ClusterReplyRetry` it returns `ActionAbort`
   and changes nothing.
2. It changes the service's state.
3. It returns.

An entry that was aborted is handed over again, and a reply that an earlier handling
of the same entry sent is not sent twice: the second answers `ClusterReplySent` and
sends nothing. A handler that panics, or returns a value that is no action, has its
entry aborted too, and `Poll` returns a `*HandlerError` ("The panic"): an entry is
taken only by a handler that finished with it, so `service.Position()`, the position
after the last entry taken, never passes an entry whose handler failed.

The service's `Poll` and its `Close` are calls on the client the service was made on
("Threads"). `service.Close()` is idempotent; close a service before its client
(`spec/c-abi.md`, "Functions", cluster service; ADR 0123; ADR 0124).
`conformance/go/cluster` is a program that runs three services and a session through
every call above.

### A service's snapshot and its start

A service's program holds the cluster's state, and a snapshot is that state as the
program's own bytes at one position of the log, the same beside every member. A
service that starts again takes its state from its last snapshot and reads the log
after it, where it would read the log from position 0 (`spec/c-abi.md`, "A service's
snapshot and its start"; ADR 0146; ADR 0149). Three calls, none of them from inside
the handler:

- `service.Restore()`, before the first poll, returns `restored`, `state` and an
  error. With `ClusterRestoredState`, `state` is the program's state as it gave it, a
  copy the caller owns, never `nil`; the polls read the log from the snapshot's
  position. With `ClusterRestoredFromStart`, `state` is `nil`, and the polls read from
  position 0. With `ClusterRestoredNoMember`, `state` is `nil`; the driver hosts no
  member with a log yet, and the program calls again before it polls.
- `service.SnapshotDue()`, after a poll, returns the snapshot's log position and
  `true` when the poll ended at the leader's request, an entry whose `Type` is
  `ClusterEntrySnapshot`, and `false` otherwise.
- `service.Snapshot(state)`, before the next poll, writes the snapshot and tells the
  member, which lists it. `state` is a `[]byte`, 1 GiB at the most. It returns `true`,
  or `false` when the member lists one at or past that position already.

So a program's loop is this, where `load`, `dump`, `report`, `onEntry` and `running`
are the program's own:

```go
func run(service *cavalry.ClusterService) error {
	restored, state, err := service.Restore()
	for err == nil && restored == cavalry.ClusterRestoredNoMember {
		time.Sleep(time.Millisecond) // no member with a log yet: ask again
		restored, state, err = service.Restore()
	}
	if err != nil {
		return err // the service has not started: call Restore again before any poll, or stop
	}
	if state != nil {
		load(state) // bytes to the program's state
	}
	for running {
		count, err := service.Poll(onEntry, 10)
		if err != nil {
			return err // a *HandlerError: the handler failed, and its entry was not taken
		}
		if count < 0 {
			return service.Err() // the poll stopped on a fault, which service.Fault() gives
		}
		if _, due := service.SnapshotDue(); due {
			if _, err := service.Snapshot(dump()); err != nil { // the program's state to bytes
				report(err) // nothing committed is lost: poll on, and the next request asks again
			}
		}
	}
	return nil
}
```

The state must be a function of the entries the handler was handed, and of nothing
else: every member's service gives its own, and a start from any of them must reach
what the others hold. A program that never calls `Restore` reads the log from
position 0 at every start, and its member's log can then not be purged.

`Snapshot` and `Restore` wait for the driver under the client's lock ("Threads"): a
second at the most for each step of a snapshot's own, the response timeout, 10 s, for an
add or a command that a driver does not answer, and for as long as the snapshot's
recording takes to play back at a start. Each returns an `*Error` with the ABI's code:
`ErrClusterNoSnapshotDue`, `ErrClusterSnapshotTooLong`, `ErrClusterSnapshotFailed`
with the step in the message, `ErrClusterRestoreTooLate` once the service has begun,
and `ErrPolling` from inside a handler. After an error of `Snapshot` the snapshot is
still due, and the program calls again or polls on. After an error of `Restore` the
service has not started, and the program calls again or stops, as the loop does: it
never polls, because a poll reads the log from position 0, and every `Restore` after
it returns `ErrClusterRestoreTooLate` (`spec/c-abi.md`, "A service's snapshot and its
start", item 4). `conformance/go/snapshot` is a program that runs every call above
beside three members.

## What it must never do

- Never run a message through anything but one ABI call: no lock, no goroutine, no
  channel, no allocation, no copy of a fragment for the handler. `clients/lint.py`
  reads the per-message functions of `cavalry.go` and `trampoline.go`, the claim, the
  reserved value, the commit, the abort, the offer, a subscription's and a committed
  log's poll, the header's values and the trampoline, a cluster session's offer and
  poll, a cluster service's poll and reply, the two functions that call their
  handlers and the two exported functions the C trampolines call, for Go's forms of each (`sync`, a lock or a wait through a variable
  or a field, such as `registryMu.Lock()`, which names no `sync.`, a `go` statement, a
  channel send or receive, `select`, `panic`, and `make`, `new` and `append`), and
  those of `shim.c`, the C trampoline and the poll, for C's (`pthread_mutex_*`,
  `pthread_rwlock_*`, `mtx_*`). `just lint` fails on any of them, in the function's
  own body or in a function of the package it calls.
- Never return an error for an `Offer` code: `TryClaim` and `Offer` return the position
  after the frame or the code below zero (`PublicationNotConnected`,
  `PublicationBackPressured`, `PublicationAdminAction`, `PublicationClosed`,
  `PublicationMaxPositionExceeded`, `PublicationError`), as the ABI does; the caller
  branches. A cluster session's `Offer` returns the same codes, and a cluster
  service's `Reply` returns its `ClusterReply*` code, or −1 from outside the handler.
- Never swallow the handler's panic, or a fragment: a fragment handler that panics
  keeps every later fragment of that poll reaching the handler, and `Poll` returns a
  `*HandlerError` once the ABI has returned, with the first panic's value as its
  `Cause` and the counts. A panic must not unwind through the ABI's frames, so the
  trampoline recovers every one.
- Never take an entry for a cluster service's handler that failed. The handler's
  return value is the entry's action, so the client answers for a handler that
  panicked, with `ActionAbort`, and the C trampoline aborts an entry whose action the
  ABI refused: the poll ends there, and `Poll` returns a `*HandlerError`. The entry is
  handed over again; it is never skipped.
- Never make a slice at an address that holds no byte. For an entry that is no
  session's message the ABI's buffer is an address of 1 with a length of 0. Go ends
  the process when it copies a goroutine's stack and finds a pointer below 4,096 on
  it, so the trampolines of a cluster session and of a cluster service hand the
  handler `nil` for a body of no bytes and make no slice.
- Never read the ABI's error on another thread than the failing call's. The error is
  the calling OS thread's and Go moves goroutines between threads, so a call that is
  not a message holds its goroutine's thread from the call to the read, and a
  per-message call's error is copied in the C call that failed ("The thread's
  error"). `just lint` fails a function that reads the error without the lock.
- Never lose a handle the ABI keeps open. Every close frees its handle, whatever it
  returns, so `Close` forgets the handle once the ABI's close has returned. The one
  exception is a close that the ABI refuses with `ErrPolling`, which frees nothing
  and leaves the handle open (`spec/c-abi.md`, "Calls during a claim or a poll"). The
  ABI refuses three kinds of close. The first is the close of a subscription, of a
  committed log, of a cluster session or of a cluster service from inside a handler
  of its own poll. The second is a cluster service's close from inside the handler of
  another service whose poll holds its client. The third is a `Client`'s close from
  inside the handler of a cluster service whose poll holds that client. `Close` then
  returns the `*Error` and keeps the handle, so that the program can still use the
  handle, and close it, after the poll has returned. A `Client`'s `Close` does not
  close the handles it made: each `Publication`, `Subscription`, `Recording`,
  `Replay`, `CommittedLog`, `ClusterSession` and `ClusterService` is freed by
  its own `Close` and by nothing else, and a publication or a subscription answers as
  closed from the client's close until then (`spec/c-abi.md`, "Lifetimes"). An
  unclosed `Subscription`, `CommittedLog`, `ClusterSession` or `ClusterService` also
  keeps its slot in the table
  ("Threads"), so a program that closes only its clients fills the table, and
  `AddSubscription`, `CommittedLog`, `ConnectCluster` and `ClusterService` return
  `ErrSubscriptionsFull`. A publication's
  close zeroes the claim struct too, so `Commit`, `Abort` and `SetReservedValue`
  after it answer -1 with `ErrNoClaim` in `Err` and the ABI reads no freed handle
  through the struct's `publication` (`spec/c-abi.md`, "Which claim a struct names").
- Never read a refusal as an answer. A `Poll` from inside a handler of the same
  subscription returns −1 with `ErrPolling` in `Err`, and the poll in progress goes
  on. A committed log's handler is held to the same rule. From inside it, a `Poll` of
  the same log, `log.Position()` and `log.CommitPosition()` return −1, `log.IsEnded()`
  is `false`, `log.FailedAt()`, `log.WithdrawnAt()` and `log.Faulted()` are zeros and
  `false`, and `log.Fault()` and `log.Close()` return an `*Error`, each with
  `ErrPolling`, and the log stays open. The four `false` are refusals there, not
  answers: read the four after the poll has returned. The handler has each entry's own
  `position`. A cluster session's handler and a cluster service's are held to it as
  well. From inside a session's handler, `Offer` returns `PublicationError`, a `Poll`
  of the same session, `LeadershipTermID()`, `LeaderMemberID()` and `StaleReplies()`
  return −1, `Closed()` is `""` and `false`, a refusal there, and `Fault()` and
  `Close()` return an `*Error`, each with `ErrPolling`: a handler that answers a reply
  with a message keeps what it needs and offers after the poll has returned. From
  inside a service's handler, a `Poll` of the same service, `Position()` and
  `Sessions()` return −1, `Answers()` is `false`, a refusal there, and `Fault()` and
  `Close()` return an `*Error`. The service's poll holds its client too, so every call
  on that client from inside the handler is refused the same way: `client.IsClosed()`
  is `false`, a refusal, `client.Closed()` is −1, and an add, a command,
  `client.CommittedLog`, `client.ConnectCluster`, `client.ClusterService` and
  `client.Close()` return an `*Error` with `ErrPolling`. A refused `client.Close()`
  frees nothing, and the client stays open. Make these calls after the poll has
  returned.
- Never edit a generated file by hand: `just lint` regenerates the module and fails
  when the committed files differ. `example/main.go` is the one file written by hand,
  and it is this README's program.

## Example

The ten-minute path's program (the README's `cavalry run`, the command that builds
the library from the same checkout, and `go run`), as `conformance/go/quickstart`
runs it with its checks. The program is `clients/go/example/main.go`;
`conformance/go/readme` checks that this page shows that file and that it prints the
line below:

```sh
cargo build --release -p cavalry --bin cavalry   # the driver
cargo build --release -p cavalry-ffi --lib       # libcavalry_ffi.a, which cgo links
target/release/cavalry run &                     # one driver per host
(cd clients/go && go run ./example)              # builds the package with cgo, then runs it
kill -TERM %1
```

```go
package main

import (
	"encoding/binary"
	"fmt"

	cavalry "github.com/appshapes/cavalry/clients/go"
)

func main() {
	client, err := cavalry.NewClient("") // the driver at CAVALRY_DIR, or its default
	if err != nil {
		panic(err)
	}
	defer client.Close()
	subscription, err := client.AddSubscription("cavalry:ipc", 1001)
	if err != nil {
		panic(err)
	}
	publication, err := client.AddPublication("cavalry:ipc", 1001)
	if err != nil {
		panic(err)
	}
	received := make([]uint32, 0, 1000)
	onFragment := func(body []byte, header *cavalry.Header) { // body: a slice into the log, for the call
		received = append(received, binary.LittleEndian.Uint32(body))
	}
	for sent := uint32(0); sent < 1000; {
		if publication.TryClaim(32) < 0 { // the position after the frame, or a code below 0
			subscription.Poll(onFragment, 10) // back-pressured or not yet connected: drain, retry
			continue
		}
		binary.LittleEndian.PutUint32(publication.Claim, sent) // write into the log
		publication.Commit()
		sent++
	}
	for len(received) < 1000 {
		subscription.Poll(onFragment, 10)
	}
	for i, value := range received {
		if value != uint32(i) {
			panic("every message once, in order")
		}
	}
	fmt.Printf("%d messages over cavalry:ipc, position %d\n", len(received), publication.Position())
}
```

You will see `1000 messages over cavalry:ipc, position 64000`.

## The runtime's side of the boundary

What cgo does per message, and what `conformance/go/quickstart`'s report measures of
it. The figures below are one run's, on the laptop (Go 1.27.0, macOS 26.6 on arm64,
unpinned); every CI run prints its runners'.

### The call

Each per-message function is one cgo call, to a function of `shim.c` that makes the
one ABI call with the arguments as they are: the handle as the C pointer the ABI
returned, the claim struct as the address of the one `cavalry_buffer_claim_t` the
publication keeps, the offered bytes as the address of the slice's first element. The
shim's last argument is the address of the one `cavalry_go_error_t` the handle keeps,
which the shim writes only when the call failed ("The thread's error"); a call that
succeeds costs one C function call and one comparison more than the ABI's own. A cgo
call switches the goroutine to the system stack and back, which Go's runtime does in
14 ns on this laptop for a call that returns at once (`cavalry_abi_version`, 5,000,000
calls), and cgo's pointer check, on by default, reads each pointer argument once. The
ABI's reply comes back as the Go integer of the C type. Around the call the runtime
marks the goroutine as in a system call, `entersyscall`, and back, `exitsyscall`; on
their fast path these take no lock and make no syscall, and "The call-back, by
construction" says when their slow path runs and what it costs. An add, a command or a
close waits on the driver for up to 10 s (`spec/c-abi.md`, "Threads"); the goroutine
blocks in the call and the scheduler runs the program's other goroutines meanwhile.
`client.ConnectCluster` waits until the leader has opened the session, 20 s at most.

A cluster service's `Poll` is the one per-message function that can wait for the
driver. It sends commands with the service's client when it opens the member's log,
at a session's opening, where a term's leader changes and at a recording's end, and
it waits for each answer in the call, as an add does; a session message costs no
command. The runner's cluster round reports the waits. On this laptop (Apple M5 Pro),
in each of 25 runs, 4 of about 17,900 polls of three services took over 1 ms. The
longest, in 23 of the runs a poll that opened a member's log, took 7.2 to 11.5 ms.
The program's other goroutines run meanwhile.

### Threads

The ABI runs one call on a handle at a time, and a read of a client, such as
`cavalry_is_closed`, never runs beside an add or a command (`spec/c-abi.md`,
"Threads"); it serialises nothing itself. So a `Client` takes its own `sync.Mutex`
around every call on its handle, as the Python client takes its lock, and goroutines
may share it. A call on a shared client waits for the call another goroutine is
making on it, an add or a command for up to 10 s; no message passes through a
`Client`, so the mutex is never on a message's path. `conformance/go/quickstart`
shares one client among eight goroutines that add and close at once.

A `Publication`, `Subscription`, `CommittedLog`, `ClusterSession`, `ClusterService`,
`Recording`, `Replay` or `Image` is
used from one goroutine at a time, as the Rust API's handles are: the client adds no lock to them.
A program with several goroutines gives each its own `Publication` and
`Subscription`. Two handles may be used from two goroutines at once, a client and its
publications and subscriptions among them. `Client.AddDestination` and
`RemoveDestination` are calls on the client, not on the `Publication` they name: they
may run beside that publication's `Offer` or `TryClaim` in another goroutine, but not
beside its `Close` (`spec/c-abi.md`, "Threads"; ADR 0057).

A `ClusterService` changes that for one client. Its `Poll` is a call on its client as
well as on the service: the ABI's poll sends its commands with the client and holds it
for the run of the handler. `Poll` is on every entry's path, so it takes no lock, the
client's mutex among them. So from `client.ClusterService()` until the service's
`Close`, that client is used by the goroutine that polls the service and by no other.
A program whose other goroutines add or command gives them a `Client` of their own,
as `spec/c-abi.md`, "Threads", says for a program in C. A mutex in `Poll` would not
make the client safe to share. A handler that calls its client must reach the ABI,
which refuses the call with `ErrPolling`; behind a mutex that its own poll held, the
same call would stop the goroutine for ever, since a `sync.Mutex` is not taken twice
(ADR 0124, Alternatives). The service's open and its close take the client's mutex,
as a committed log's do, and so do its `Snapshot` and its `Restore`: each sends
commands with the client and waits for the driver (`spec/c-abi.md`, "Threads"; ADR
0149), and the goroutine that polls makes them between its polls ("A service's
snapshot and its start"). A `ClusterSession`'s connect is a call on the client and
takes its mutex; nothing else of a session is.

The table the trampoline reads subscriptions from holds 4,096 subscriptions, committed
logs, cluster sessions and cluster services together: `AddSubscription`,
`CommittedLog`, `ConnectCluster` or `ClusterService` above that returns an
`*Error` with `ErrSubscriptionsFull`, and each `Close` frees a slot. The table is written under a lock when a
subscription is added or closed, never on a message's path, and read with one atomic
load per fragment. The package's own test, `cavalry_test.go`, which
`conformance/go/run.sh` runs with `go test` and which needs no driver, fills the
table to its bound. It checks the refusal past the bound and its message, that a freed
slot is the next one taken, and that the trampoline calls no handler for a slot of 0,
a slot past the table or an empty slot.

### The thread's error

The ABI keeps one error per OS thread: `cavalry_errcode` and `cavalry_errmsg` describe
the last call that failed on the calling thread, and a success clears nothing
(`spec/c-abi.md`, "Error codes"). Go runs a goroutine on whichever OS thread its
scheduler picks, and may run the goroutine's next cgo call on another thread than its
last. A read of the error in a second cgo call, after the call that failed, could then
read another thread's failure, or none. The client never reads the error that way:

- A call that is not a message (`NewClient`, an add, a command, a close, `Fault`,
  `ImageAt`) holds the goroutine to its OS thread with `runtime.LockOSThread` from
  before its first ABI call until it returns. The `*Error` it returns is read on the
  thread of the call that failed. The lock nests, so it holds inside a fragment
  handler too. `clients/lint.py` fails a function of `cavalry.go` that reads the
  error through `check` or `lastError` without `runtime.LockOSThread()` and a
  deferred `runtime.UnlockOSThread()` before its first ABI call.
- A per-message call goes through a function of `shim.c`. When the ABI call failed,
  the shim copies the code, the message and the ABI function's name into the one
  `cavalry_go_error_t` the handle keeps, in the same C call, so on the same thread.
  `Publication.Err`, `Subscription.Err` and `CommittedLog.Err` return that copy as an
  `*Error`, kept until the handle's next failure, as the ABI keeps a thread's; they
  return `nil` before the first failure. The copy is bounded by the struct, 48 bytes
  of function name and 512 bytes of message, the size of the ABI's own buffer; it
  allocates nothing, and a call that succeeds copies nothing. `Err` builds the
  `*Error` when it is called.
- `Errcode` and `Errmsg` are the ABI's two functions as they are: they read the
  calling thread's error. They describe the goroutine's own failure in two places
  only. One is inside a fragment handler, where cgo holds the goroutine to the poll's
  thread for the call, as after a `Header.Values` that returned `false`. The other is
  between `runtime.LockOSThread` and `runtime.UnlockOSThread` around both the failing
  call and the read.

### The call-back, by construction

The ABI calls the C trampoline in `shim.c` with the body's address and length and the
frame's header. That function calls the exported Go function `cavalryGoFragment`, a
cgo call-back: Go's runtime switches from the system stack to the goroutine that
called `Poll`, which is what the call costs, and nothing is allocated for it. The Go
trampoline reads the subscription from the table by the index the ABI handed back as
`clientd`, which the C trampoline passes as a `uintptr_t`, never as a pointer, since
Go's stack copier ends the process on a value below 4,096 in a pointer's place. It
makes the body's `[]byte` over the log's bytes with `unsafe.Slice`, no copy and no
allocation, points the subscription's one `Header` at the frame, and calls the
handler inside a function that recovers a panic ("The panic"). By construction, no
object per received message. A committed log's poll has a C trampoline of its own in
`shim.c`. It reads the entry's position from the header with
`cavalry_committed_log_entry_position`, a C call inside the ABI's call-back, and
calls the exported Go function `cavalryGoEntry` with it, so an entry crosses into Go
once, as a fragment does, and the Go side makes no cgo call for the position. Until
card 260 the Go trampoline made that call itself, one cgo call more for each entry,
at what "The call" says one costs.

Each call-back crosses the boundary twice more, in Go's runtime (`cgocallbackg` in
`runtime/cgocall.go`). The runtime first holds the goroutine to the polling thread
for the call-back, since the C frames below it are on that thread's stack: a count on
the thread, not a lock and not a syscall. It then calls `exitsyscall` to take back the
goroutine's P, the runtime's processor, which Go code needs in order to run and which
the poll's cgo call gave up in `entersyscall`. After the handler, `reentersyscall`
gives the P up again for the rest of the poll. A poll of n fragments is therefore one
`entersyscall` and `exitsyscall` pair for the call and n more for the call-backs. A
committed log's poll of n entries is the same: the position is read in C, inside the
call-back, and crosses nothing. The client's own `runtime.LockOSThread` ("The
thread's error") is on none of them.

`exitsyscall` has a fast path and a slow one (`runtime/proc.go`):

- The fast path takes back the P the call gave up, with a compare-and-swap of the P's
  status and one of the goroutine's. It takes no lock and makes no syscall, and it is
  part of the 14 ns of "The call". `entersyscall` stores the two statuses and takes no
  lock either.
- The slow path runs when that P is gone. Go's monitor thread, sysmon, retakes the P
  of a call that is still in C at two of its looks, 20 µs apart at the shortest and
  10 ms apart once sysmon has had nothing to do for a while (`retake`). It does so
  only while the P has goroutines waiting in its run queue, or no other P is idle, or
  sysmon has seen the call for 10 ms. The returning goroutine then takes the
  scheduler's lock, `sched.lock`, to find an idle P. With none, it waits in the
  global run queue while its thread parks, on a futex on Linux and a semaphore on
  macOS, until another thread runs it; inside a call-back the thread is held, so it
  parks until another thread hands it a P (`stoplockedm`). That is a lock, a sleep
  and a wake: two context switches and the wait for a free P, all in that message's
  latency. The retake itself may wake or start a thread to run the P meanwhile, on
  sysmon's thread, not the caller's.

A per-message call returns in nanoseconds, so the slow path needs the OS to stop the
thread inside the ABI for at least one of sysmon's intervals, by preemption or by a
cgroup's CPU throttling, while other goroutines wait for the P. The report's latency
round runs one goroutine with idle Ps, so sysmon leaves the P with the call unless it
has seen the call for 10 ms. Two more steps take `sched.lock` with no message as their
cause: the first cgo call after every P was idle wakes sysmon, once per idle period,
and a call that begins while the collector is stopping the world gives its P to the
collector, once per stop. A call that is not a message waits on the driver for long
enough to take the slow path when other goroutines are runnable; under its
`runtime.LockOSThread` its thread then parks in `stoplockedm` until a P is handed to
it, on the control plane.

The `Header`'s `Values` reads `cavalry_header_values` once per fragment, on the first
call, into the one `cavalry_header_values_t` the subscription keeps, and returns the
`Frame` by value, a struct of nine integers that lives on the caller's stack. Outside
the handler call `Values` returns `false`, since the header is valid for the call
alone. The body's slice is valid for the call alone too: it points into the log,
which the publisher writes over once the subscription's position has moved past it.
Go has no view that can be detached, so a slice kept past the call reads whatever the
log holds later; copy what must outlive the handler.

A cluster session's poll and a cluster service's each have a C trampoline of their
own in `shim.c`. It reads the reply's values or the entry's from the header, with one
ABI call, `cavalry_cluster_reply_values` or `cavalry_cluster_entry_values`, and hands
them to the exported Go function as integers, so the Go side makes no cgo call of its
own for them. The Go function writes them into the one `ClusterReplyValues` or
`ClusterEntryValues` the session or the service keeps, and the handler is handed that
struct's address and, for a service, the service: nothing is allocated. A service's
Go function returns the handler's action to the C trampoline, which passes it to the
ABI with `cavalry_cluster_service_action`, unless it is `ActionContinue`, which the
ABI does with no call. For a service the poll's `clientd` is the address of a small C
struct on the stack of the shim's poll, which holds the table index, the service's
handle and where an aborted entry is told; it is C memory and holds no Go pointer.
The package's own test calls both exported functions as the C trampolines do, and
`clients/generate-test.py` compiles `shim.c` against a stand-in for the ABI and for
the Go side and checks each action.

### The view over a claimed slice, by construction

`publication.Claim` is a `[]byte` over the bytes the ABI reserved, made with
`unsafe.Slice` from the pointer the ABI filled into the one `cavalry_buffer_claim_t`
the publication keeps, no allocation; `Commit`, `Abort` and `Close` set it to `nil`.
A slice kept past the commit points into the log, as a body does.

### The count, measured

The runner's allocation round sends 10,000 messages whose handler only counts and whose
publisher copies one constant, with `runtime.ReadMemStats` read before and after the
round with nothing between, which counts every allocation Go's heap made. On the
laptop: 0 allocations and 0 bytes per message over 9,999 messages, so a message
allocates nothing on either side of the boundary; a `runtime.GC` before the round
and the reads are the round's own cost.

### The latency, measured

The runner's latency round sends 100,000 messages over `cavalry:ipc`, one at a time on
one goroutine. The publisher writes the monotonic clock, `time.Since` a start, into
the claim before its commit, and the handler reads the clock first. On the laptop,
unpinned: p50 83 ns, p99 125 ns, p99.99 3,583 ns (10 samples above it, nearest
rank), max 5,500 ns. That is two cgo calls and one call-back per message
over the ABI's own one-way latency, which `just bench`'s `one_way_32_ffi` row
measures. The laptop's run came before the per-message calls went through `shim.c`,
which adds one C call and one comparison to each. One run with the shim, on a CI
runner (Go 1.24.13, Linux on amd64, unpinned), printed p50 111 ns, p99 210 ns,
p99.99 6,442 ns and max 21,771 ns, and 0 allocations and 0 bytes per message.

### The panic

A panic in the handler must not unwind through the ABI's frames: Go's runtime would
end the process at the first C frame it met. The Go trampoline calls the handler
inside a function that recovers, keeps the first panic's value, counts every one, and
goes on calling the handler for every later fragment of the poll, so that no fragment
is consumed unseen; `Poll` returns the count and a `*HandlerError` once the ABI has
returned, with `Fragments`, `Raised`, the first value as `Cause`, and a message that
says what to do. `Fragments` is the poll's count, −1 when the poll also stopped on a
fault. A committed log's `*HandlerError` has `CommittedLog` true and speaks of entries,
and when `Fragments` is −1 it names the log's `Fault` and `Err`, to read before the
next poll ("The committed log"). The value is kept as it was panicked, an `error`, a
string or anything else, and stays the first when an `error` follows it in the same
poll. A cluster session's handler is held as a subscription's is: every reply the poll
hands over reaches it, and the `*HandlerError`, whose `Cluster` is `"session"`, speaks
of replies and names the session's `Fault` and `Err`.

A cluster service's handler is the one whose failure stops its poll, because what it
returns is read: it is the entry's action. When the handler panics, the client
recovers the panic, keeps its value, and answers `ActionAbort` for the entry, so the
ABI ends the poll there with the entry not taken. `Poll` then returns a
`*HandlerError`, whose `Cluster` is `"service"`: `Fragments` is the count of entries
the poll took before that entry, `Raised` is 1, and `Cause` is the panic's value. A
handler that returns a value that is no action, such as 3, is treated the same way.
The ABI refuses the value, the C trampoline keeps the ABI's error and aborts the
entry, and `Cause` is that `*Error`, with `ErrClusterAction` and a message that names
the value. The next poll hands the aborted entry over again, and returns the error
again until the handler takes it: the service does not skip an entry, since every
member's service must apply the same entries. A reply the failed handling sent is not
sent twice. What the handler changed of its own state before it panicked is the
handler's to guard, which is why it replies first and changes its state last ("The
cluster service"). The runner checks it: a handler that replies and then panics is
handed its entry twice at one position, and the session reads one reply.
`runtime.Goexit` in a handler is not a panic and is not recovered; a program that
calls it from a handler ends the goroutine that called `Poll` inside the ABI, which
the ABI's page does not allow.

## What to read next

- `spec/c-abi.md`: the page the module wraps, function for function.
- `clients/README.md`: how a client is generated and what every client passes.
- `clients/python/README.md`: the first client, whose shape this one copies.
- `conformance/go/`: the runner and its report.
- `docs/decisions/0046-build-the-c-abi-and-the-clients-fourth.md`: the slice this
  client is the fourth feature of.
- `docs/decisions/0124-give-each-client-a-session-object-and-a-service-object-whose-handlers-failure-aborts-its-entry.md`:
  the shape of the cluster session and the cluster service in every client, why a
  failed handler's entry is aborted, and why a service's poll takes no lock.
- `docs/how-to/run-a-cluster.md`: how to start the members a session connects to.
