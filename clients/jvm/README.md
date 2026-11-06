# com.appshapes.cavalry, the JVM client

## What this does

A Java library over the C ABI of `spec/c-abi.md`, generated from the committed header
by `clients/generate.py jvm`: the package `com.appshapes.cavalry` reaches the shared
library `libcavalry_ffi` through Java's foreign function and memory API
(`java.lang.foreign`, final since Java 22; this library needs Java 25, the current
long-term release, and nothing else: no JNI, no build tool, no dependency), and
gives the handles the forms a Java user expects. `Abi` is the binding, a downcall
handle per ABI function and a layout per struct, which refuses a library of another
ABI version when it loads, before it binds any other function; the rest is the
client's own code. A `Client` closes with `close()` or `try`-with-resources; a
`Publication` claims a slice of the log as a `MemorySegment` over it or offers a
`MemorySegment`; a `Subscription` polls with a `FragmentHandler` that receives the
body as a `MemorySegment` into the log and the frame's `Header`; a `Recording` and a
`Replay` wrap the driver's archive. Every message the client sends or receives goes
through one `extern "C"` call on the caller's thread, as the ABI's page promises
(ADR 0046's boundary; ADR 0051). In Aeron, this is the Java client's `Publication`,
`Subscription` and `FragmentHandler`, which this one copies in shape: the handler
receives a buffer it must not keep, and `tryClaim` and `offer` return a position or a
code below zero.

The foreign function and memory API is the card's choice over JNI (ADR 0046, the JVM
client): JNI would need a C shim compiled per platform and a build tool to build it,
and its call-back into Java copies nothing less; the API binds the header's functions
from Java alone, at run time, so the library is `javac` and `java` and nothing else. The
cost is the Java release: 22 brought the API, and this library takes 25, the
long-term release that holds it.

## What it must never do

- Never run a message through anything but one ABI call: no lock, no `synchronized`
  block, no wait, no future, no thread, no copy of a fragment into a new array for the
  handler. `clients/lint.py` reads the per-message methods of `Publication`,
  `Subscription` and `Header`, the claim, the reserved value, the commit, the abort,
  the offer, the poll, the header's nine getters and the upcall that calls the
  handler, for Java's forms of each: `synchronized`, as a modifier or a block, the lock
  classes of `java.util.concurrent` and a lock taken on any receiver (`.lock(`,
  `.tryLock(`, `.acquire(`), a blocking wait (`wait`, `join`, `await`, `park`, `sleep`,
  `take`), a future or an executor, a thread started, `throw` and `new`. The one
  exception built on the path is the `HandlerException` that `poll` throws after its
  ABI call; a `throw` inside the `catch (Throwable)` that the compiler demands around
  `invokeExact`, a block that cannot run once the handle is bound, is not one; and
  `Header`'s guard, `ensure`, throws for a getter called outside its handler call,
  before the ABI is reached. `just lint` fails on any other, in the method's own body
  or in a method of the package it calls.
- Never throw for an `Offer` code: `tryClaim` and `offer` return the position after
  the frame or the code below zero (`Abi.PUBLICATION_NOT_CONNECTED`,
  `PUBLICATION_BACK_PRESSURED`, `PUBLICATION_ADMIN_ACTION`, `PUBLICATION_CLOSED`,
  `PUBLICATION_MAX_POSITION_EXCEEDED`, `PUBLICATION_ERROR`), as the ABI does; the
  caller branches.
- Never swallow the handler's exception, or a fragment: a fragment handler that throws
  keeps every later fragment of that poll reaching the handler, and `poll` throws a
  `HandlerException` once the ABI has returned, with the first throwable as its cause
  and the counts. An exception must not unwind through the ABI's frames, where the
  runtime would end the process, so the upcall catches every one.
- Never lose a handle the ABI keeps open. Every close frees its handle, whatever it
  returns, so `close()` forgets the handle once the ABI's close has returned. The one
  exception is a subscription's close from inside a handler of its own poll, which the
  ABI refuses with `Abi.ERR_POLLING`, leaving the subscription open (`spec/c-abi.md`,
  "Calls during a claim or a poll"). `close()` then throws `CavalryException` and keeps
  the handle, so the subscription can be polled and closed after the poll. A call
  after a close hands the ABI a null handle, as the Python and Go clients do, and the
  ABI refuses it with `Abi.ERR_NULL`: a poll answers -1, a claim
  `PUBLICATION_ERROR`, never a delivery and never a frame.
- Never hand the ABI a claim struct that names a freed publication. The one
  `cavalry_buffer_claim_t` a publication keeps names its handle in its `publication`
  field, and the ABI reads that field on a commit, an abort and a reserved value. So
  `Publication.close()` zeroes the struct before the ABI's close frees the handle. A
  commit, an abort or a reserved value after the close then names no publication, and
  the ABI answers -1 with `Abi.ERR_NO_CLAIM` without reading a handle (`spec/c-abi.md`,
  "Which claim a struct names"). A struct left as its last claim filled it would make
  the ABI read freed memory. A new publication can be allocated at the same address,
  so the stale struct could then commit that publication's claim before its body was
  written.
- Never edit a generated file by hand: `just lint` regenerates the library and fails
  when the committed files differ. `example/Example.java` is the one file written by
  hand, and it is this README's program.

## Example

The ten-minute path's program (the README's `cavalry run`, the command that builds
the library from the same checkout, and `javac`), as `conformance/jvm/Quickstart.java`
runs it with its checks. The program is `clients/jvm/example/Example.java`;
`conformance/jvm/Readme.java` checks that this page shows that file and that it
prints the line below:

```sh
cargo build --release -p cavalry --bin cavalry                          # the driver
cargo build --release -p cavalry-ffi --lib                              # libcavalry_ffi, which Abi loads
javac --release 25 -d clients/jvm/build/classes clients/jvm/src/com/appshapes/cavalry/*.java
target/release/cavalry run &                                            # one driver per host
java --enable-native-access=ALL-UNNAMED -cp clients/jvm/build/classes clients/jvm/example/Example.java
kill -TERM %1
```

```java
import com.appshapes.cavalry.Client;
import com.appshapes.cavalry.FragmentHandler;
import com.appshapes.cavalry.Publication;
import com.appshapes.cavalry.Subscription;

import static java.lang.foreign.ValueLayout.JAVA_INT_UNALIGNED;

public class Example {
    public static void main(String[] args) {
        int[] received = new int[1000];
        int[] count = {0};
        try (Client client = new Client()) { // the driver at CAVALRY_DIR, or its default
            Subscription subscription = client.addSubscription("cavalry:ipc", 1001);
            Publication publication = client.addPublication("cavalry:ipc", 1001);
            FragmentHandler onFragment = (body, header) -> // body: a view into the log, for the call
                received[count[0]++] = body.get(JAVA_INT_UNALIGNED, 0);
            int sent = 0;
            while (sent < 1000) {
                if (publication.tryClaim(32) < 0) { // the position after the frame, or a code below 0
                    subscription.poll(onFragment, 10); // back-pressured or not yet connected: drain, retry
                    continue;
                }
                publication.claim().set(JAVA_INT_UNALIGNED, 0, sent); // write into the log
                publication.commit();
                sent++;
            }
            while (count[0] < 1000) {
                subscription.poll(onFragment, 10);
            }
            for (int i = 0; i < 1000; i++) {
                if (received[i] != i) {
                    throw new IllegalStateException("every message once, in order");
                }
            }
            System.out.println(count[0] + " messages over cavalry:ipc, position " + publication.position());
        }
    }
}
```

You will see `1000 messages over cavalry:ipc, position 64000`.

## The runtime's side of the boundary

What the foreign function and memory API does per message, and what
`conformance/jvm/Quickstart.java`'s report measures of it. The figures below are one
run's, on the laptop (OpenJDK 25.0.5, macOS 26.6 on arm64, unpinned); every CI run
prints its runners'.

### The call

Each per-message method is one downcall through a `MethodHandle` the binding made
once, invoked with `invokeExact`, so that the runtime checks nothing at the call and
boxes nothing: the handle as the `MemorySegment` the ABI returned, the claim struct as
the one native segment the publication keeps, the offered bytes as the caller's
segment, a native one or a heap one over an array. The compiler demands a
`catch (Throwable)` around every `invokeExact`; a handle that was bound cannot fail at
run time, and the catch rethrows what it would never see as `CavalryException`.

What the runtime does around a downcall: it moves the thread's state from Java to
native before the call and back after it, two stores, and on the way back the thread
polls for a safepoint, one load. When the runtime is at a safepoint as the call
returns, for a collection or a deoptimization, the thread waits in that poll until the
safepoint ends: the one wait the runtime can add to a crossing. No lock and no system
call otherwise; the runtime frees nothing later for a call, since a downcall allocates
nothing. The offer's handle is bound with `Linker.Option.critical(true)`: no
transition at all, and the runtime pins a heap segment where it is while the ABI
copies the bytes and returns, calling nothing back and blocking on nothing, so what a
critical call adds to a collection that wants to start is the copy's length. A
downcall costs 2.9 ns on this laptop for a function that returns at once
(`cavalry_abi_version`, 5,000,000 calls after 100,000 that warm the compiler). An add,
a command or a close waits on the driver for up to 10 s (`spec/c-abi.md`, "Threads");
the thread blocks in the call.

### Threads

A `Client`'s methods are `synchronized`, so threads may share one: an add, a command
or a close holds the client's lock for its one ABI call, which the ABI serialises
anyway, and the lock is never on a message's path. A `Publication`, a `Subscription`
and its `Header` are used from one thread at a time, the ABI's rule for every handle;
a program with several threads gives each its own, as the Rust API's are used. The
upcall stub a subscription makes is bound to the subscription itself, so the ABI's
`clientd` is unused and no table is read per fragment.

The ABI's error, `Abi.errcode()` and `Abi.errmsg()`, is per operating-system thread.
This library reads it on the line after the call that failed, with nothing between,
and a program reads it the same way after a `PUBLICATION_ERROR` from `tryClaim` or
`offer`. A platform thread is one operating-system thread for its life, so there the
error waits as long as the program likes. A virtual thread is mounted on a carrier
thread and moves to another carrier when it blocks: the downcall itself pins the
carrier, since the runtime never unmounts a thread in native code, and a read on the
next line has no blocking between, so the error read is the carrier's that made the
call; a program that blocks between the call and the read, on a lock or a socket,
reads another carrier's error, or none. Read the error first.

### The call-back, by construction

The ABI calls the subscription's upcall stub, which `Linker.upcallStub` made once from
a method handle bound to the subscription, with the body's address and length and the
frame's header. The runtime's upcall makes a `MemorySegment` of length zero for each
of the three addresses, three objects, and calls `Subscription.onFragment`, which
reinterprets the body's segment to its length, one more object, points the
subscription's one `Header` at the frame and calls the handler inside a
`try`/`catch (Throwable)` ("The exception"). So four objects per received message,
each a few dozen bytes, each dead when the handler returns, counted by the allocation
round below. The body is never copied. There is no one view per subscription reused
across messages, as the card asked and as the Rust API's slice is: a `MemorySegment`
cannot be pointed somewhere else once made, and the one alternative, a segment over
the whole address space with the body as an offset into it, would hand the handler
every byte of the process; the objects cost what "The count" says.

The `Header`'s getters read `cavalry_header_values` once per fragment, on the first
getter, into the one native struct the subscription keeps, and return the field as a
primitive, no object. Outside the handler call every getter throws
`IllegalStateException`, since the header is valid for the call alone. The body's
segment is valid for the call alone too: it points into the log, which the publisher
writes over once the subscription's position has moved past it. A segment kept past
the call reads whatever the log holds later; copy what must outlive the handler.

### The view over a claimed slice, by construction

`publication.claim()` is a `MemorySegment` over the bytes the ABI reserved, made from
the address the ABI filled into the one `cavalry_buffer_claim_t` the publication
keeps: `get` reads the address as a segment of length zero, one object, and
`reinterpret` makes the segment over the claimed length, a second; so two objects per
claim. `commit`, `abort` and `close` set it to `null`. A segment kept past the commit
points into the log, as a body does.

### The count, measured

The runner's allocation round sends 10,000 messages whose handler only counts and whose
publisher copies one constant, with the thread's allocated bytes read from
`com.sun.management.ThreadMXBean` before and after the round with nothing between,
which counts every object the thread allocated, freed or not. On the laptop: 206.5
bytes per message, about five objects' worth where the construction above makes six,
the claim's two and the call-back's four, since the compiler removes an object it can
prove dies inside the call and the count is what it left; and 0 collections during the
round.

### The latency, measured

The runner's latency round sends 100,000 messages over `cavalry:ipc`, one at a time on
one thread. The publisher writes `System.nanoTime()` into the claim before its commit,
and the handler reads the clock first. On the laptop, unpinned: p50 125 ns, p99
834 ns, p99.99 19,750 ns (10 samples above it, nearest rank), max 170,084 ns. That is
two downcalls and one upcall per message over the ABI's own one-way latency, which
`just bench`'s `one_way_32_ffi` row measures. The round warms nothing first, so it
holds the compiler's work on the first thousands of messages, from the interpreter
through both compilers, each replacement installed at a safepoint; a program that has
run for long measures the ABI's tail plus the transitions of "The call".

### The exception

An exception in the handler must not unwind through the ABI's frames: an uncaught
throwable in an upcall ends the process. `Subscription.onFragment` calls the handler
inside a `try`/`catch (Throwable)`, keeps the first throwable, counts every one, and
goes on calling the handler for every later fragment of the poll, so that no fragment
is consumed unseen; `poll` throws `HandlerException` once the ABI has returned, with
`fragments()`, `raised()`, the first throwable as its cause, and a message that says
what to do. The throwable is kept as thrown, an `Error` included, and stays the first
when another follows it in the same poll.

## What to read next

- `spec/c-abi.md`: the page the library wraps, function for function.
- `clients/README.md`: how a client is generated and what every client passes.
- `clients/python/README.md`: the first client, whose shape this one copies.
- `conformance/jvm/`: the runner and its report.
- `docs/decisions/0046-build-the-c-abi-and-the-clients-fourth.md`: the slice this
  client is the fifth feature of.
