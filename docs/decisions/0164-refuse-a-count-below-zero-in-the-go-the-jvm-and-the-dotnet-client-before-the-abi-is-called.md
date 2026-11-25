# 0164. Refuse a count below zero in the Go, the JVM and the .NET client, before the ABI is called

- **Status:** Accepted
- **Date:** 2026-11-25
- **Invariants affected:** 1, 2, 6
- **Supersedes:** none. It is ADR 0162's rule in the three clients that ADR left to
  work item 465, and it extends to their per-message methods the one point on which
  ADR 0162 superseded ADR 0046, "no raise": a per-message method of the JVM or the
  .NET client may refuse a count below zero by a throw in a reader the lint names
  with its callers, before its ABI call. It adds one sentence to `spec/c-abi.md`,
  "Error codes", with no version moved: no code of the ABI is below zero.

## Context

ADR 0162 decided that a generated client never passes the ABI another integer than
its caller gave, and held the Python and the Node client to it. It said of the other
three that they keep the rule for an int32 and an int64, which their compilers hold to
the parameter's type, and break it in one place.

That place is a count or a length. The ABI takes one as a `size_t`. Go, Java and C#
give a caller a signed integer for it, as `len`, an array's length and a span's
`Length` are signed, and each client converts it with a cast that keeps the bits:
`C.size_t(length)` in Go, `(long) length` for the foreign call in Java, `(nuint)
length` in C#. A count below zero reaches the ABI as a count near 2^64. Six functions
of each client take such a count from a caller: the claim's length, the fragment limit
of each of the four polls, and, off a message's path, the index of the image at an
index.

Run against a driver, through the Python client's plain types, the ABI answered:

- a claim of 2^64 − 1 bytes: `CAVALRY_PUBLICATION_ERROR` with `CAVALRY_ERR_TOO_LONG`,
  and the message "a message of 18446744073709551615 bytes, this publication allows
  1376". Nothing is claimed;
- a poll with a fragment limit of 2^64 − 1 and five messages ready: 5, with all five
  handed over in order. The ABI reads a limit above `INT_MAX` as `INT_MAX`;
- the image at index 2^64 − 1: none.

So each is wrong and none is harmful: nothing is lost, handed over twice or read past
its end. The caller of a claim is told of a length it never gave, and the caller of a
poll that asked for −1 fragments is handed every fragment that is ready.

Aeron's own clients, read from its sources:

- Java: `tryClaim` calls `checkPayloadLength` first, which throws
  `IllegalArgumentException("invalid length: " + length)` for a length below zero.
  `poll` checks nothing: its loops run `while fragmentsRead < fragmentLimit`, so a
  limit below zero reads nothing and returns 0.
- C: both parameters are `size_t`.
- The C++ wrapper's `poll` takes an `int` for the limit and casts it to
  `std::size_t` unchecked, so for a limit it has the fault this ADR is about; its
  `tryClaim` casts its length the same way.

## Decision

**A count or a length below zero is refused before the ABI is called**, in the claim,
the four polls and the image at an index of the Go, the JVM and the .NET client.

1. **A claim's length below zero** is refused, as Aeron's Java client refuses it.
2. **A fragment limit below zero** is refused too, and here Cavalry does not copy
   Aeron's Java client, which reads nothing and returns 0. A poll that takes nothing
   and says nothing leaves a poll loop that never receives, with no error to find; a
   limit of 0 is still in range and reads nothing; and the Python and the Node client
   refuse a negative limit already, under ADR 0162. A caller that passed −1 to mean no
   limit, as the unchecked cast made it, is told to pass the count it means.
3. **The refusal is each language's own form.**
   - JVM: `IllegalArgumentException`, thrown by `Abi.negative`.
   - .NET: `ArgumentOutOfRangeException`, thrown by `Abi.Negative`, with the
     parameter as its `ParamName`.
   - Go: no panic. The function fails as it fails for any other reason: `TryClaim`
     returns the ABI's code for an error, a poll returns −1, and `Err` gives an
     `*Error`. `ImageAt`, which returns an error, returns it. The count crosses to the
     client's C shim as an int64, which holds every Go `int`, and the shim refuses it
     there and writes the error the Go function already hands back.
4. **The Go client's code for it is its own, −1, `ErrNegative`.** The ABI was not
   called, so no code of the ABI's is true. `spec/c-abi.md` now says that no ABI code
   is below zero, in this version or a later one, and that a value below zero is a
   client's to use; that sentence is what keeps a later ABI code from being −1.
5. **A function tests its argument in line**, one comparison with zero, and calls its
   reader only for a count below zero. In Go the comparison is in the shim, which the
   call already enters. A count in range costs no call and no allocation.
6. **A refusal names the parameter as the language names it**, the name the caller
   wrote: `length`, `fragmentLimit`, `index`. This is the rule for every client. The
   Python client's names are the ABI's already. The Node client's generated wrappers
   name the ABI's parameter, `recording_id` for `recordingId`; they change to the
   language's name in the second pull request of work item 465, which changes the
   Node generator.
7. **The lint.** The JVM and the .NET rules gain a reader rule as narrow as ADR 0162's
   for Python. `Abi.negative` and `Abi.Negative` may throw; the rule names each with
   the claim and the four polls; a call of the reader by one of them is allowed on a
   line before the method's first call of the ABI. A reader called by another
   per-message method, by an upcall, or not before the ABI call, a poll's among them,
   and any other form in the reader or in a function it calls, is a finding. ADR
   0046 gives a poll one raise, of its handler's exception, once its ABI call has
   returned; the lint allows a raise there, by the poll or by a function it calls,
   as it did before this ADR, and does not ask whether a reader is what throws.
   The lint reads a method's own text for its ABI call, `Abi.cavalry_*`. The image at
   an index is not per-message and is not the lint's. Go's rules do not change: its
   functions gain no panic, and its shim gains no lock.

## Alternatives considered

- **An unsigned type for the parameter.** The JVM has none. In Go and C# it moves the
  cast into the caller's code, `uint(n)` and `(uint) n`, where the same wrap happens
  out of the client's sight, and `len` and `Length` stay signed.
- **Read a count below zero as zero**, as Aeron's Java poll does in effect. It is
  another count than the caller gave, which is what ADR 0162 forbids, and it hides the
  mistake where a refusal names it.
- **Leave it, and say per function that the conversion is harmless.** The work item
  allows it, and by the run above the three answers are harmless. But the claim's
  error names a length of 18446744073709551615 bytes to a caller that passed −1, and
  the poll does the opposite of what −1 most likely meant. Each costs a stranger an
  hour; a refusal costs a comparison.
- **A panic in Go**, as the standard library panics for a negative count in
  `bytes.Repeat` and `make`. The Go client returns every failure of a per-message
  function as a value, and its lint holds its functions to no panic; a caller that
  checks `Err` after `TryClaim` already has the place to learn of this one.
- **A returned code in the JVM and .NET**, with no throw. A per-message method of
  those clients reports a failure by a result code and the thread's ABI error, and
  the ABI's error cannot be set by a client; a code with no error to read would be
  worse than an exception that says everything.
- **An ABI code for the Go client's refusal**, `CAVALRY_ERR_TOO_LONG` or a new one.
  The first is untrue of a length below zero. A new one would be a code the ABI
  never gives.
- **Refuse in the ABI.** It cannot: it is handed a `size_t` in range.

## Evidence

All of it is from one laptop, macOS on Apple silicon, on 2026-11-25; none is from the
bench hosts.

**What the ABI answers to such a count** is in the Context: run against a driver at
`master`'s library, through the Python client's plain `ctypes` types for the
per-message functions.

**That each client refuses, through its real runtime.** Of the six refusals, the
quickstart runner of each of the three clients makes three: a claim of −1 bytes, a poll with a fragment limit of −1
whose handler fails the run if it is called, and a lookup of the image at index −1,
against a driver, with a publication and a subscription that have just carried 1,000
messages. The Go runner reads the ABI's code for an error from the claim and −1 from
the poll, each with an `Err` whose code is `ErrNegative` and whose message begins with
the parameter's name and ends with the README's section, and the same error from
`ImageAt`. The JVM runner catches `IllegalArgumentException` three times and the .NET
runner `ArgumentOutOfRangeException` with the parameter as its `ParamName`. Each
runner then goes on to its other rounds and passes.

**That the shim refuses before the ABI.** `clients/generate-test.py` compiles the Go
client's `shim.c` against a stand-in for the ABI that counts its calls. For each of
the five functions and each of −1, −2^31 and the least int64 the shim answers the
error code, calls the ABI no time, and keeps the client's code, the ABI function's
name and the whole message; for each of 0, 5, 2^31 − 1 and the greatest int64 it calls
the ABI once with that count.

**That the lint holds the allowance.** `clients/lint-test.py` has one test for each of
the JVM and the .NET client: the committed package passes; the reader called by
another per-message method, by an upcall, after the claim's ABI call, on its line or
the next, and after a poll's, a lock in the reader and a lock in a function the reader
calls, are each a finding; a renamed reader is a stale rule; and the rule's list of
callers is the claim and the four polls. Eight changes to the rule, each of which
should fail a test, each did.

**That the spec's promise holds.** `clients/generate-test.py` reads every
`CAVALRY_ERR_*` of the header and fails on one below zero.

**An independent read** of the change found no count that still reaches the ABI
converted. It found the Go client's doc comments still saying that an `Error` holds
the ABI's code and message alone, sentences that claimed more than the lint and the
runners do, and edges of the lint's rule that no test held; each is as this ADR says
now. It also found, outside this change, that the Go client's `Images` panics when the
ABI answers −1 for the image count, as it does from inside a handler; that is work
item 468.

**What a message's path pays.** Each runner's latency round sends 100,000 messages
over `cavalry:ipc`, one at a time on one thread, unpinned, and reports the time from
the publisher's clock read to the handler's first. The publisher reads its clock
after the claim has returned, so the interval holds the commit, the poll and the
handler's call: the poll's comparison is in it, and the claim's is not measured here.
The clock steps by 41 or 42 ns.
`master`'s client and the changed one ran in turn, five runs each, the lowest and the
highest run of each:

| | p50 | p99 | p99.99 | max |
| --- | --- | --- | --- | --- |
| Go, `master` | 83 to 125 ns | 167 to 375 ns | 3,584 to 12,083 ns | 7,042 to 105,167 ns |
| Go, changed | 125 ns | 208 to 250 ns | 3,500 to 9,917 ns | 11,250 to 45,209 ns |
| JVM, `master` | 125 to 208 ns | 1,083 to 1,708 ns | 27,708 to 47,875 ns | 424,417 to 877,667 ns |
| JVM, changed | 125 to 167 ns | 958 to 2,208 ns | 22,625 to 47,000 ns | 411,958 to 825,208 ns |
| .NET, `master` | 42 ns | 125 to 209 ns | 3,042 to 6,500 ns | 123,792 to 375,750 ns |
| .NET, changed | 42 to 83 ns | 125 to 292 ns | 3,292 to 6,041 ns | 136,875 to 246,167 ns |

The Go runner's median read 83 or 84 ns in two of `master`'s five runs and 125 ns in
all five of the changed client's: one step of the clock, in a statistic that sits on a
step's edge. One .NET run of the changed client read a median one step higher. No
changed client's p99.99 or max is above the highest of `master`'s runs. The highest
p99 is the changed client's for the JVM, 2,208 against 1,708 ns, and for .NET, 292
against 209 ns, one run each. Five runs of a laptop do not tell a comparison with
zero from the clock's step, and no run's number is a bound. No row of the nightly
bench covers these clients; each runner's report is where its cost is seen.

## Consequences

- A program that passed a count below zero to one of the three clients is refused
  where it was answered. For a poll that is a breaking change for a program that
  passed −1 to mean no limit; `changelog.d/` says so and says what to pass.
- A claim and a poll of the JVM and the .NET client gain one comparison of an `int`
  with zero per call. The Go client's gain it in the shim. The Evidence has, for
  each runner, four quantiles of the interval that holds the poll's; the claim's is
  not measured.
- The JVM and the .NET client now throw from a per-message method for a caller's
  mistake, where before they threw from one only for a handler's exception, after the
  poll. The lint's rule keeps that to the one reader, before the ABI.
- `spec/c-abi.md` promises that no code of the ABI is below zero. A later ABI version
  cannot give a negative code without superseding this ADR.
- The Go client's `Error.Code` may now be below zero, for a failure the ABI did not
  report. Its doc comment and the README say so.
- The Node client's refusals name the ABI's parameter until the second pull request
  of work item 465 changes its generator.

## References

- Work item 465; ADR 0162, whose rule this is, and work item 460.
- ADR 0046, "The client's own code", for the check ADR 0162 superseded on one point.
- `spec/c-abi.md`, "Error codes".
- `clients/go/README.md`, `clients/jvm/README.md` and `clients/dotnet/README.md`,
  "What it must never do".
- `clients/lint.py` (the `readers` rules of `jvm` and `dotnet`, `typed_reader_findings`),
  `clients/lint-test.py` and `clients/generate-test.py` for their tests, and the
  quickstart runner of each client under `conformance/` for the refusals through the
  real runtime.
- Aeron's sources at its master, `aeron-client`: `Publication.java`,
  `ConcurrentPublication.java`, `ExclusivePublication.java`, `Subscription.java`,
  `Image.java`; `aeron_subscription.c`, `aeron_publication.c`; the C++ wrapper's
  `Subscription.h` and `Publication.h`.
