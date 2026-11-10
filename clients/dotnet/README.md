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
`CommittedLog` is a service's read of a cluster's committed log beside a member (below).
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
committed entry, in order, none at or past the member's commit position, and returns
the entry count; `position` is the entry's position in the log. `log.Position()` is
the position after the last entry delivered, the one a service keeps for its next
start, and `log.CommitPosition()` the bound of the last poll. `log.IsEnded()` says that
the member closed or left its log: once a poll then returns 0, close the log and open
another from `log.Position()`. `log.FailedAt()` is `null` unless the driver's replay of
the member's recording failed. `log.Close()`, or `using`, stops the driver's replay
and is idempotent; a log closed after its client is freed, and the driver ends the
replay by itself. `log.MemberId` and `log.RecordingId` say whose log it is
(`spec/c-abi.md`, "Functions", committed log; ADR 0068).

A committed log has an `UnmanagedCallersOnly` trampoline of its own, as a subscription
has. A handler's exception goes where a subscription's goes ("The exception"): every
entry the ABI delivers reaches the handler, and `Poll` throws `HandlerException` once
the ABI has returned. An entry whose handler call threw was delivered and is not
delivered again, so a service that must apply every entry catches its own exceptions
and stops there, with the entry's `position` in hand to open the log at again.

## What it must never do

- Never run a message through anything but one ABI call: no lock, no `lock`
  statement, no wait, no task, no thread, no copy of a fragment into a new array for
  the handler. `clients/lint.py` reads the per-message members of `Publication`,
  `Subscription`, `CommittedLog` and `Header`, the claim, its span, the reserved value,
  the commit, the abort, the offer, each poll, the header's nine getters and each
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
  caller branches.
- Never write into a received body: the handler's `body` is a `ReadOnlySpan<byte>`, so
  the compiler refuses a write, which is what the log allows, since the subscriber maps
  it read-only and a write through it would end the process.
- Never swallow the handler's exception, or a fragment: a fragment handler that throws
  keeps every later fragment of that poll reaching the handler, and `Poll` throws a
  `HandlerException` once the ABI has returned, with the first exception as its inner
  exception and the counts. An exception must not unwind through the ABI's frames,
  where the runtime would end the process, so the trampoline catches every one.
- Never lose a handle the ABI keeps open. Every close frees its handle, whatever it
  returns, so `Close()` forgets the handle once the ABI's close has returned. The one
  exception is a subscription's close from inside a handler of its own poll, which the
  ABI refuses with `Abi.ERR_POLLING`, leaving the subscription open (`spec/c-abi.md`,
  "Calls during a claim or a poll"). `Close()` then throws `CavalryException` and keeps
  the handle, so the subscription can be polled and closed after the poll. A call
  after a close hands the ABI a null handle, as the Python and Go clients do, and the
  ABI refuses it with `Abi.ERR_NULL`: a poll answers -1, a claim
  `PUBLICATION_ERROR`, never a delivery and never a frame. The close clears the claim
  struct too, so `Commit`, `Abort` and `SetReservedValue` after it answer -1 with
  `Abi.ERR_NO_CLAIM` and the ABI reads no freed handle through the struct's
  `publication` (`spec/c-abi.md`, "Which claim a struct names").
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
(`spec/c-abi.md`, "Threads"); the thread blocks in the call.

### Threads

A `Client`'s methods hold the client's lock, a `System.Threading.Lock`, so threads may
share one: an add, a command or a close holds it for its one ABI call, which the ABI
serialises anyway, and the lock is never on a message's path. A `Publication`, a
`Subscription` or a `CommittedLog` and its `Header` are used from one thread at a time, the ABI's rule
for every handle; a program with several threads gives each its own, as the Rust
API's are used. `Client.AddDestination` and `RemoveDestination` are calls on the
client, not on the `Publication` they name: they may run beside that publication's
`Offer` or `TryClaim` in another thread, but not beside its `Close` or `Dispose`
(`spec/c-abi.md`, "Threads"; ADR 0057).

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
invocation, inside a `try`/`catch (Exception)` ("The exception"). By construction, no
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
`InnerException`, and a message that says what to do. The exception is kept as
thrown and stays the first when another follows it in the same poll.

## What to read next

- `spec/c-abi.md`: the page the library wraps, function for function.
- `clients/README.md`: how a client is generated and what every client passes.
- `clients/python/README.md`: the first client, whose shape this one copies.
- `conformance/dotnet/`: the runner and its report.
- `docs/decisions/0046-build-the-c-abi-and-the-clients-fourth.md`: the slice this
  client is the sixth feature of.
