# Cavalry, the .NET client

## What this does

A .NET library over the C ABI of `spec/c-abi.md`, generated from the committed header
by `clients/generate.py dotnet`: the namespace `Cavalry`, project
`clients/dotnet/Cavalry.csproj`, reaches the shared library `libcavalry_ffi` through
P/Invoke with the source-generated marshalling (`LibraryImport`, .NET 7 and later;
this library takes .NET 10, the current long-term release, and nothing else: no
package reference, no native shim, no build step beyond `dotnet build`), and gives the
handles the forms a .NET user expects. `Abi` is the binding, a `LibraryImport` per
ABI function and a struct per struct, which refuses a library of another ABI version
when it loads, before any other function; the rest is the client's own code. A
`Client` closes with `Close()` or `using`; a `Publication` claims a slice of the log
as a `Span<byte>` over it or offers a `ReadOnlySpan<byte>`; a `Subscription` polls
with a `FragmentHandler` that receives the body as a `ReadOnlySpan<byte>` into the
log and the frame's `Header`; a `Recording` and a `Replay` wrap the driver's archive; a
`CommittedLog` is a service's read of a cluster's committed log beside a member; a
`ClusterSession` is a client's session with a cluster; a `ClusterService` is a
cluster's service beside a member, handed each committed entry and answering the
sessions (all three below).
Every message the client sends or receives goes through one `extern "C"` call on the
caller's thread, as the ABI's page promises (ADR 0046's boundary; ADR 0051). In Aeron,
this is Aeron.NET's `Publication`, `Subscription` and `FragmentHandler`, which this
one copies in shape: the handler receives a buffer it must not keep, and `TryClaim`
and `Offer` return a position or a code below zero.

The source-generated marshalling is the card's choice over the runtime's
(`DllImport`): a `LibraryImport` is compiled to a stub at build time, so the call
needs no IL stub generated at run time and nothing the compiler cannot see, and a
blittable parameter, a handle as `nint`, a struct by `ref`, a function pointer, a
byte pointer, is passed as it is, copied nowhere (ADR 0046, the .NET client). The
cost is the .NET release: 7 brought `LibraryImport`, and this library takes 10, the
long-term release that holds it.

### The committed log

A service is a program beside a cluster member that applies the entries the cluster
has committed (`docs/how-to/read-the-committed-log.md`). `client.CommittedLog(position)`
opens the log of the member on the client's driver at a position, 0 or the
`Position()` an earlier run reached, and returns `null` while the driver hosts no
member with a log, so a service started before its member asks again. `log.Poll(handler,
fragmentLimit)` calls the `EntryHandler`, `handler(body, header, position)`, for each
committed entry, in order, none at or past the member's commit position; `position` is
the entry's position in the log. It returns the entry count, or −1 when the ABI's poll
stopped on a fault, which `log.GetFault()` gives, its `Code`, `SessionId` and
`Position`, until the next poll. `log.Position()` is the position after the last entry
delivered, the one a service keeps for its next start, and `log.CommitPosition()` the
bound of the last poll. `log.IsEnded()` says that no poll will deliver another entry:
stop reading when it is true, not when a poll returns 0, then close the log and open
another from `log.Position()`. `log.Close()`, or `using`, stops the driver's replay and
is idempotent; a log closed after its client is freed, and the driver ends the replay
by itself. When the ABI could not stop the replay, `log.Close()` throws
`CavalryException` after it has freed the log, and a second close does nothing.
`log.MemberId` and `log.RecordingId` say whose log it is (`spec/c-abi.md`,
"Functions", committed log; ADR 0070).

An ended log delivered every committed entry it may read only when three things hold:

- `log.FailedAt()` is `null`. A position is where the driver's replay of the member's
  recording failed, and no entry from it on was delivered.
- `log.WithdrawnAt()` is `null`. A position is where the entries begin that the log
  was not given, because the driver withdrew the replay, 5 s after it ended, before
  the service had read it.
- `log.Faulted()` is `null`. A `Subscription.Fault` is where this client's read of the
  replay's image, the log's own, stopped: its `Code` is `Abi.ERR_FAULT_IMAGE` when a
  frame of the image failed a check, or when the image was gone before this client
  read it, and `Abi.ERR_FAULT_IMAGES_FULL` when the image could not join. No entry
  from the fault on was delivered.

Read the three reports once `log.IsEnded()` is true. The driver withdraws a replay
that failed as it does any other, so the first two can both be positions:
`log.WithdrawnAt()` is then the lower, and the entries from it on, those below
`log.FailedAt()` among them, were not delivered. The poll that meets the image's fault
returns −1 once, `log.GetFault()` answers it until the next poll, and the next poll
returns 0 and clears it there. `log.Faulted()` answers it from that poll on, for as
long as the log is held (ADR 0080). `log.WithdrawnAt()` is `null` for such a log, and
`log.FailedAt()` is a position only when the driver's replay failed as well, which is
then the cause to look at first. In every case what the log delivered ends at `log.Position()`.

A committed log has an `UnmanagedCallersOnly` trampoline of its own, as a subscription
has. A handler's exception goes where a subscription's goes ("The exception"): every
entry the ABI delivers reaches the handler, and `Poll` throws `HandlerException` once
the ABI has returned, with the poll's result as its `Fragments`, the −1 of a fault
included. An entry whose handler call threw was delivered and is not delivered again:
`log.Position()` is past it, and past every entry the same poll delivered after it. A
handler cannot stop a poll: the ABI goes on handing it entries up to the fragment
limit. So a service that must apply every entry, in order, does this:

1. The handler catches its own exceptions. When it cannot apply an entry, it keeps
   that entry's `position` and sets a flag.
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
responseChannel)` opens one and returns a `ClusterSession`. `members` is the members
string the cluster's members were started with (`docs/how-to/run-a-cluster.md`), and
`responseChannel` a channel of this host for the cluster's answers,
`cavalry:udp?endpoint=<host>:<port>`. The call returns once the leader has opened the
session and waits for that for 20 s at most; it throws `CavalryException` with
`Abi.ERR_CLUSTER_MEMBERS`, `ERR_CLUSTER_REFUSED`, `ERR_CLUSTER_VERSION` or
`ERR_CLUSTER_CONNECT_TIMEOUT`.

`session.Offer(message)` sends the leader a `ReadOnlySpan<byte>` of at most
`session.MaxMessageLength` bytes, and returns the position after the message on the
session's publication to the leader, or a publication's result code below zero:
`Abi.PUBLICATION_CLOSED` for a session that is closed, and `Abi.PUBLICATION_ERROR`,
with `Abi.ERR_TOO_LONG` as `Abi.Errcode()`, for a message that is too long. A message
taken is on its way to the leader and is not committed. `session.Poll(handler,
fragmentLimit)` calls `handler(body, reply)` for each reply: `body` is the reply's
payload, a `ReadOnlySpan<byte>` into the log for the call, and `reply` is a
`ClusterReplyValues` with `reply.LeadershipTermId`, the term of the entry the reply
answers, and `reply.Timestamp`, the cluster's time when its leader appended that
entry, in milliseconds since 1970-01-01T00:00:00Z. `reply` is the one object the
session keeps, over a struct filled for each reply, so copy a value out to keep it.
The poll returns the count of replies handed over, or −1 when it stopped on a fault,
which `session.GetFault()` gives until the next poll. The poll is the session's duty
cycle as well: it sends the keepalive, follows a new leader and closes the session
when the cluster says so. Call it at least every 500 ms.

A reply is no delivery. The terms of the replies a handler is given never fall: a
reply of a term below one already handed over is stale, is passed over, and is counted
in `session.StaleReplies()` (ADR 0121). A message whose leader was lost before it
committed is answered by nobody. A program that needs an answer offers again when
none comes. `session.Closed()` is `null` while the session is open, and then the
reason: `"TIMEOUT"`, `"NO LEADER"` or the detail of the cluster's closed event.
`session.LeaderMemberId()` and `session.LeadershipTermId()` say whom the session
sends to, and `session.SessionId` is the id the leader gave it. `session.Close()`
offers the leader a session close for at most 500 ms and frees the session; it is
idempotent, and a `ClusterSession` is an `IDisposable`. Close a session before its
client (`spec/c-abi.md`, "Functions", cluster session; ADR 0122).

### The cluster service

A cluster's service is the program beside each member that applies the committed
entries and answers the sessions (`spec/cluster.md`, "The service" and "Replies").
`client.ClusterService()` makes one beside the member on the client's driver and
returns a `ClusterService`; it sends nothing. `service.Poll(handler, fragmentLimit)`
opens the member's log when the member has one, and calls `handler(body, entry,
service)` for each committed entry, in order:

- `body` is a session message's payload, a `ReadOnlySpan<byte>` into the log for the
  call. It is empty for every other entry.
- `entry` is a `ClusterEntryValues`: `entry.Type`, one of
  `Abi.CLUSTER_ENTRY_SESSION_MESSAGE`, `_SESSION_OPEN`, `_SESSION_CLOSE`,
  `_NEW_LEADERSHIP_TERM` and `_OTHER`; `entry.Position`; `entry.LeadershipTermId`;
  `entry.Timestamp`, the cluster's time, the one clock a service reads, so that every
  member's service reaches the same state; `entry.SessionId`, −1 where the entry has
  no session; `entry.LeaderMemberId`, for a new leadership term; and `entry.TimedOut`,
  for a session close. It is the one object the service keeps, over a struct filled
  for each entry, so copy a value out to keep it.
- `service` is the service that was polled, for `service.Reply`.

What the handler returns says what becomes of the entry. `Abi.ACTION_CONTINUE` takes
it, and the poll goes on. `Abi.ACTION_BREAK` takes it and ends the poll.
`Abi.ACTION_ABORT` leaves it and ends the poll, and the next poll hands it over again.
The poll returns the count of entries taken, or −1 when it stopped on a fault, which
`service.GetFault()` gives until the next poll.

`service.Reply(sessionId, message)` answers a session from inside the handler and
returns what became of the reply: `Abi.CLUSTER_REPLY_SENT`;
`CLUSTER_REPLY_NOT_ANSWERING`, from a service whose member did not lead the entry's
term, which has nothing to do; `CLUSTER_REPLY_RETRY`, when the session's response
channel did not take the reply now, for which the handler returns `Abi.ACTION_ABORT`;
`CLUSTER_REPLY_DROPPED`; `CLUSTER_REPLY_NO_SESSION`; or `CLUSTER_REPLY_TOO_LONG`. Every
member's service makes the same calls for the same entry, and one of them sends: the
one beside the leader of the entry's term. So a handler does this, in this order:

1. It replies. When the reply answers `CLUSTER_REPLY_RETRY` it returns
   `Abi.ACTION_ABORT` and changes nothing.
2. It changes the service's state.
3. It returns.

An entry that was aborted is handed over again, and a reply that an earlier handling
of the same entry sent is not sent twice: the second answers `CLUSTER_REPLY_SENT` and
sends nothing. A handler that throws, or returns a value that is no action, has its
entry aborted too, and `Poll` throws `HandlerException` ("The exception"): an entry is
taken only by a handler that finished with it, so `service.Position()`, the position
after the last entry taken, never passes an entry whose handler failed.

The service's `Poll` and its `Close` are calls on the client the service was made on
("Threads"). `service.Close()` is idempotent, and a `ClusterService` is an
`IDisposable`; close a service before its client (`spec/c-abi.md`, "Functions",
cluster service; ADR 0123; ADR 0124). `conformance/dotnet/Cluster/Program.cs` is a
program that runs three services and a session through every call above.

## What it must never do

- Never run a message through anything but one ABI call: no lock, no `lock`
  statement, no wait, no task, no thread, no copy of a fragment into a new array for
  the handler. `clients/lint.py` reads the per-message members of `Publication`,
  `Subscription`, `CommittedLog`, `ClusterSession`, `ClusterService` and `Header`, the
  claim, its span, the reserved value, the commit, the abort, the offer, a session's
  offer and a service's reply, each poll, the header's nine getters, the getters of a
  reply's values and of an entry's, and each
  trampoline that calls a handler, for C#'s forms of each: `lock`, `Monitor`, `Mutex`, `Semaphore`,
  `SpinLock` and the other waits of `System.Threading`, `async`, `await`, `Task`,
  `ValueTask`, `.Wait(`, `.Result`, `Thread.Sleep`, a thread started, `throw`, `new`,
  and an interpolated string. The one exception built on the path is the
  `HandlerException` that `Poll` throws after its ABI call; a `Span<byte>` or
  `ReadOnlySpan<byte>` made with `new` over bytes the ABI handed over is a view, on the
  stack, not an allocation; and `Header`'s guard, `Ensure`, throws for a getter
  called outside its handler call, before the ABI is reached. `just lint` fails on
  any other, in the member's own body or in a method of the package it calls.
- Never throw for an `Offer` code: `TryClaim` and `Offer` return the position after
  the frame or the code below zero (`Abi.PUBLICATION_NOT_CONNECTED`,
  `PUBLICATION_BACK_PRESSURED`, `PUBLICATION_ADMIN_ACTION`, `PUBLICATION_CLOSED`,
  `PUBLICATION_MAX_POSITION_EXCEEDED`, `PUBLICATION_ERROR`), as the ABI does; the
  caller branches. A cluster session's `Offer` returns the same codes, and a cluster
  service's `Reply` returns its `Abi.CLUSTER_REPLY_*` code, or −1 from outside the
  handler.
- Never write into a received body: the handler's `body` is a `ReadOnlySpan<byte>`, so
  the compiler refuses a write, which is what the log allows, since the subscriber maps
  it read-only and a write through it would end the process.
- Never swallow the handler's exception, or a fragment: a fragment handler that throws
  keeps every later fragment of that poll reaching the handler, and `Poll` throws a
  `HandlerException` once the ABI has returned, with the first exception as its inner
  exception and the counts. An exception must not unwind through the ABI's frames,
  where the runtime would end the process, so the trampoline catches every one.
- Never take an entry for a cluster service's handler that failed. The handler's
  return value is the entry's action, so the method the trampoline calls answers for a
  handler that threw, with `Abi.ACTION_ABORT`, and aborts an entry whose action the
  ABI refused: the poll ends there, and `Poll` throws. The entry is handed over again;
  it is never skipped.
- Never lose a handle the ABI keeps open. Every close frees its handle, whatever it
  returns, so `Close()` forgets the handle once the ABI's close has returned. The one
  exception is the close of a subscription, of a committed log, of a cluster session
  or of a cluster service from inside a handler of its own poll, which the
  ABI refuses with `Abi.ERR_POLLING`, leaving it open (`spec/c-abi.md`,
  "Calls during a claim or a poll"); a cluster service's close is refused in the same
  way from inside the handler of another service whose poll holds its client.
  `Close()` then throws `CavalryException` and keeps
  the handle, so that it can be polled and closed after the poll. A call
  after a close hands the ABI a null handle, as the Python and Go clients do, and the
  ABI refuses it with `Abi.ERR_NULL`: a poll answers -1, a claim
  `PUBLICATION_ERROR`, never a delivery and never a frame. The close clears the claim
  struct too, so `Commit`, `Abort` and `SetReservedValue` after it answer -1 with
  `Abi.ERR_NO_CLAIM` and the ABI reads no freed handle through the struct's
  `publication` (`spec/c-abi.md`, "Which claim a struct names").
- Never read a refusal as an answer. A `Poll` from inside a handler of the same
  subscription returns −1 with `Abi.ERR_POLLING`, and the poll in progress goes on. A
  committed log's handler is held to the same rule. From inside it, a `Poll` of the
  same log, `log.Position()` and `log.CommitPosition()` return −1, `log.IsEnded()` is
  `false`, `log.FailedAt()`, `log.WithdrawnAt()` and `log.Faulted()` are `null`, and
  `log.GetFault()` and `log.Close()` throw `CavalryException`, each with
  `Abi.ERR_POLLING`, and the log stays open. The `false` and the three `null` are
  refusals there, not answers: read the four after the poll has returned. The handler
  has each entry's own `position`. A cluster session's handler and a cluster
  service's are held to it as well. From inside a session's handler, `Offer` returns
  `Abi.PUBLICATION_ERROR`, a `Poll` of the same session, `LeadershipTermId()`,
  `LeaderMemberId()` and `StaleReplies()` return −1, `Closed()` is `null`, a refusal
  there, and `GetFault()` and `Close()` throw `CavalryException`, each with
  `Abi.ERR_POLLING`: a handler that answers a reply with a message keeps what it needs
  and offers after the poll has returned. From inside a service's handler, a `Poll` of
  the same service, `Position()` and `Sessions()` return −1, `Answers()` is `false`, a
  refusal there, and `GetFault()` and `Close()` throw `CavalryException`. The
  service's poll holds its client too, so every call on that client from inside the
  handler is refused the same way: `client.IsClosed()` is `false`, a refusal, and an
  add, a command, `client.CommittedLog`, `client.ConnectCluster` and
  `client.ClusterService` throw `CavalryException` with `Abi.ERR_POLLING`. Make them
  after the poll has returned.
- Never edit a generated file by hand: `just lint` regenerates the library and fails
  when the committed files differ. `example/Program.cs` and `example/Example.csproj`
  are the files written by hand, and the program is this README's.

## Example

The ten-minute path's program (the README's `cavalry run`, the command that builds
the library from the same checkout, and `dotnet run`), as
`conformance/dotnet/Quickstart` runs it with its checks. The program is
`clients/dotnet/example/Program.cs`; `conformance/dotnet/Readme` checks that this page
shows that file and that it prints the line below:

```sh
cargo build --release -p cavalry --bin cavalry   # the driver
cargo build --release -p cavalry-ffi --lib       # libcavalry_ffi, which Abi loads
target/release/cavalry run &                     # one driver per host
dotnet run --project clients/dotnet/example      # builds the library and the program, then runs it
kill -TERM %1
```

```csharp
using System.Buffers.Binary;
using Cavalry;

int[] received = new int[1000];
int count = 0;
using Client client = new(); // the driver at CAVALRY_DIR, or its default
Subscription subscription = client.AddSubscription("cavalry:ipc", 1001);
Publication publication = client.AddPublication("cavalry:ipc", 1001);
FragmentHandler onFragment = (body, header) => // body: a span over the log, for the call
    received[count++] = BinaryPrimitives.ReadInt32LittleEndian(body);
for (int sent = 0; sent < 1000;)
{
    if (publication.TryClaim(32) < 0) // the position after the frame, or a code below 0
    {
        subscription.Poll(onFragment, 10); // back-pressured or not yet connected: drain, retry
        continue;
    }
    BinaryPrimitives.WriteInt32LittleEndian(publication.Claim, sent); // write into the log
    publication.Commit();
    sent++;
}
while (count < 1000)
{
    subscription.Poll(onFragment, 10);
}
for (int i = 0; i < 1000; i++)
{
    if (received[i] != i)
    {
        throw new InvalidOperationException("every message once, in order");
    }
}
Console.WriteLine($"{count} messages over cavalry:ipc, position {publication.Position()}");
```

You will see `1000 messages over cavalry:ipc, position 64000`.

## The runtime's side of the boundary

What P/Invoke does per message, and what `conformance/dotnet/Quickstart`'s report
measures of it. The figures below are one run's, on the laptop (.NET 10.0.9, SDK
10.0.301, macOS 26.6 on arm64, unpinned); every CI run prints its runners'.

### The call

Each per-message method is one call through a `LibraryImport` stub the compiler
generated at build time, with its arguments as they are: the handle as the `nint` the
ABI returned, the claim struct as a `ref` to the one `cavalry_buffer_claim_t` the
publication keeps, which the stub pins for the call and copies nowhere, the offered
bytes as the address of the span's first byte, pinned by `fixed` for the call. No
marshalling code runs, since every parameter is blittable; no IL stub is made at run
time.

What the runtime does around the call: it moves the thread from cooperative to
preemptive mode before the call, one store, so that a collection need not wait for
the call, and back after it, one store and one check of whether a collection asked
the threads to stop; when one did, the thread waits in that check until the
collection ends, the one wait the runtime can add to a crossing. No lock and no
system call otherwise; the runtime frees nothing later for a call, since a call
allocates nothing. A call costs 0.8 ns on this laptop for a function that
returns at once (`cavalry_abi_version`, 5,000,000 calls after 100,000 that warm the
compiler). An add, a command or a close waits on the driver for up to 10 s
(`spec/c-abi.md`, "Threads"); the thread blocks in the call. `client.ConnectCluster`
waits until the leader has opened the session, 20 s at most.

A cluster service's `Poll` is the one per-message method that can wait for the
driver. It sends commands with the service's client when it opens the member's log,
at a session's opening, where a term's leader changes and at a recording's end, and
it waits for each answer in the call, as an add does; a session message costs no
command. The runner's cluster round reports the waits. On this laptop (Apple M5 Pro,
.NET 10.0.9), in each of 25 runs, 5 or 6 of about 3,800 polls of three services took
over 1 ms. The longest took 8.0 to 11.5 ms. The round sleeps 1 ms between its duty
cycles, the shortest sleep `Thread.Sleep` makes, so it counts fewer polls than another
client's round, which sleeps 200 µs.

### Threads

A `Client`'s methods hold the client's lock, a `System.Threading.Lock`, so threads may
share one: an add, a command or a close holds it for its one ABI call, which the ABI
serialises anyway, and the lock is never on a message's path. A `Publication`, a
`Subscription` or a `CommittedLog` and its `Header` are used from one thread at a time, the ABI's rule
for every handle; a program with several threads gives each its own, as the Rust
API's are used. `Client.AddDestination` and `RemoveDestination` are calls on the
client, not on the `Publication` they name: they may run beside that publication's
`Offer` or `TryClaim` in another thread, but not beside its `Close` or `Dispose`
(`spec/c-abi.md`, "Threads"; ADR 0057). A `ClusterSession` and a `ClusterService` are
used from one thread at a time too.

A `ClusterService` changes what holds for one client. Its `Poll` is a call on its
client as well as on the service: the ABI's poll sends its commands with the client
and holds it for the run of the handler. `Poll` is on every entry's path, so it takes
no lock: it reads the client's handle through a property that takes none. So from
`client.ClusterService()` until the service's `Close()`, that client is used by the
thread that polls the service and by no other. A program whose other threads add or
command gives them a `Client` of their own, as `spec/c-abi.md`, "Threads", says for a
program in C. The lock would be on a message's path, where another thread's add could
hold it for up to 10 s (ADR 0124, Alternatives). The service's open and its close
take the client's lock, as a committed log's do. A `ClusterSession`'s connect is a
call on the client and takes its lock; nothing else of a session is.

The ABI's error, `Abi.Errcode()` and `Abi.Errmsg()`, is per operating-system thread,
and a .NET thread, a pool thread included, is one operating-system thread for its
life, so the error waits as long as the program likes. This library reads it on the
line after the call that failed, and a program reads it the same way after a
`PUBLICATION_ERROR` from `TryClaim` or `Offer`; an `await` between the call and the
read may resume on another thread, whose error is another one. Read the error first.

### The call-back, by construction

The ABI calls the one trampoline, a static method marked `UnmanagedCallersOnly`
whose address the poll passes as the handler, with the body's address and length and
the frame's header, and with the `clientd` the poll handed it: the subscription's
`GCHandle`, allocated once with the subscription and freed by its close. The
runtime's reverse P/Invoke moves the thread from preemptive to cooperative mode, one
store and the same check for a collection in progress, and the trampoline reads the
subscription back from the handle, one lookup in the handle table and one cast, no
allocation, then calls `Subscription.Deliver`, which makes a `ReadOnlySpan<byte>` over
the log's bytes, a struct on the stack, no copy and no allocation, points the
subscription's one `Header` at the frame and calls the handler, a delegate
invocation, inside a `try`/`catch (Exception)` ("The exception"). A committed log's
trampoline does the same through `CommittedLog.Deliver`, which makes one call more
per entry before the handler: `cavalry_committed_log_entry_position` on the header,
for the position it hands the handler, a `long`, no object. By construction, no
object per received message. There is no one view per subscription reused across
messages, as the card asked: a span is made per fragment, on the stack, which costs
nothing a collection would see.

The `Header`'s getters read `cavalry_header_values` once per fragment, on the first
getter, into the one struct the subscription keeps, pinned for that call, and return
the field as a primitive, no object. Outside the handler call every getter throws
`InvalidOperationException`, since the header is valid for the call alone. The body's
span is valid for the call alone too: it points into the log, which the publisher
writes over once the subscription's position has moved past it. A span cannot leave
the handler, since a `ReadOnlySpan<byte>` lives on the stack and the compiler refuses
to store it in a field or capture it; what must outlive the handler is copied out.

A cluster session's trampoline and a cluster service's make the body's span in the
same way, and one ABI call more for each reply or entry, `cavalry_cluster_reply_values`
or `cavalry_cluster_entry_values`, which fills the one struct the session's
`ClusterReplyValues` or the service's `ClusterEntryValues` holds, pinned for that
call. The handler is handed that object, whose properties read a field as a
primitive, and a service's handler the service itself: no object is made for them. A
service's trampoline then makes one ABI call more for a handler that returned
`Abi.ACTION_BREAK` or `Abi.ACTION_ABORT`, `cavalry_cluster_service_action`;
`Abi.ACTION_CONTINUE` makes none. An entry that is no session's message has an empty
body: the ABI's address for it is 1, and the span made there has a length of 0, so
nothing is read through it.

### The view over a claimed slice, by construction

`publication.Claim` is a `Span<byte>` over the bytes the ABI reserved, made on each
read of the property from the address and length the ABI filled into the one
`cavalry_buffer_claim_t` the publication keeps, a struct on the stack, no allocation;
`Commit`, `Abort` and `Close` leave it empty. A span cannot be kept past the commit,
as a body cannot leave its handler.

### The count, measured

The runner's allocation round sends 10,000 messages whose handler only counts and whose
publisher copies one constant, with the thread's allocated bytes read from
`GC.GetAllocatedBytesForCurrentThread` before and after the round with nothing
between, which counts every object the thread allocated, freed or not. On the laptop:
0.0 bytes per message over 9,999, so a message allocates nothing on either side of the
boundary; and 0 collections during the round.

### The latency, measured

The runner's latency round sends 100,000 messages over `cavalry:ipc`, one at a time on
one thread. The publisher writes `Stopwatch.GetTimestamp()` into the claim before its
commit, and the handler reads the clock first. On the laptop, unpinned: p50 42 ns, p99
125 ns, p99.99 1,166 ns (10 samples above it, nearest rank), max 87,458 ns. That is two calls and one call-back per message over the ABI's own one-way
latency, which `just bench`'s `one_way_32_ffi` row measures. The round warms nothing
first, so it holds the compiler's work on the first messages, from the first tier
through the second, each replacement installed as the method is next called; a
program that has run for long measures the ABI's tail plus the transitions of "The
call".

### The exception

An exception in the handler must not unwind through the ABI's frames: an exception
that leaves an `UnmanagedCallersOnly` method ends the process. `Subscription.Deliver`
calls the handler inside a `try`/`catch (Exception)`, keeps the first exception,
counts every one, and goes on calling the handler for every later fragment of the
poll, so that no fragment is consumed unseen; `Poll` throws `HandlerException` once
the ABI has returned, with `Fragments`, `Raised`, the first exception as its
`InnerException`, and a message that says what to do. `Fragments` is the poll's
result: the count, or −1 when the poll also stopped on a fault, which `GetFault()`
reads. A committed log's `HandlerException` has `CommittedLog` true and speaks of
entries, and when `Fragments` is −1 it names `log.GetFault()`, to read before the next
poll ("The committed log"). The exception is kept as thrown and stays the first when
another follows it in the same poll. A cluster session's handler is held as a
subscription's is: every reply the poll hands over reaches it, and the
`HandlerException`, whose `Cluster` is `"session"`, speaks of replies and names
`session.GetFault()`.

A cluster service's handler is the one whose failure stops its poll, because what it
returns is read: it is the entry's action. When the handler throws,
`ClusterService.Deliver` keeps the exception and answers `Abi.ACTION_ABORT` for the
entry, so the ABI ends the poll there with the entry not taken. `Poll` then throws
`HandlerException`, whose `Cluster` is `"service"`: `Fragments` is the count of
entries the poll took before that entry, `Raised` is 1, and the inner exception is
what the handler threw. A handler that returns a value that is no action, such as 3,
is treated the same way. The ABI refuses the value, `Deliver` reads the thread's
error where the call failed and aborts the entry, and the inner exception is a
`CavalryException` with `Abi.ERR_CLUSTER_ACTION` and the ABI's message, which names
the value. The next poll hands the aborted entry over again, and throws again until
the handler takes it: the service does not skip an entry, since every member's
service must apply the same entries. A reply the failed handling sent is not sent
twice. What the handler changed of its own state before it threw is the handler's to
guard, which is why it replies first and changes its state last ("The cluster
service"). The runner checks it: a handler that replies and then throws is handed its
entry twice at one position, and the session reads one reply.

## What to read next

- `spec/c-abi.md`: the page the library wraps, function for function.
- `clients/README.md`: how a client is generated and what every client passes.
- `clients/python/README.md`: the first client, whose shape this one copies.
- `conformance/dotnet/`: the runner and its report.
- `docs/decisions/0046-build-the-c-abi-and-the-clients-fourth.md`: the slice this
  client is the sixth feature of.
- `docs/decisions/0124-give-each-client-a-session-object-and-a-service-object-whose-handlers-failure-aborts-its-entry.md`:
  the shape of the cluster session and the cluster service in every client, why a
  failed handler's entry is aborted, and why a service's poll takes no lock.
- `docs/how-to/run-a-cluster.md`: how to start the members a session connects to.
