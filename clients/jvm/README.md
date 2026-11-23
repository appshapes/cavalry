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
`Replay` wrap the driver's archive; a `CommittedLog` is a service's read of a cluster's
committed log beside a member; a `ClusterSession` is a client's session with a cluster;
a `ClusterService` is a cluster's service beside a member, handed each committed entry
and answering the sessions (all three below). Every message the client sends or receives goes
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

### The committed log

A service is a program beside a cluster member that applies the entries the cluster
has committed (`docs/how-to/read-the-committed-log.md`). `client.committedLog(position)`
opens the log of the member on the client's driver at a position, 0 or the
`position()` an earlier run reached, and returns `null` while the driver hosts no
member with a log, so a service started before its member asks again. `log.poll(handler,
fragmentLimit)` calls the `EntryHandler`, `handler.onEntry(body, header, position)`,
for each committed entry, in order, none at or past the member's commit position;
`position` is the entry's position in the log. It returns the entry count, or −1 when
the ABI's poll stopped on a fault, which `log.fault()` gives, its `code()`,
`sessionId()` and `position()`, until the next poll. `log.position()` is the position
after the last entry delivered, the one a service keeps for its next start, and
`log.commitPosition()` the bound of the last poll. `log.isEnded()` says that no poll
will deliver another entry: stop reading when it is true, not when a poll returns 0,
then close the log and open another from `log.position()`. `log.close()`, or
`try`-with-resources, stops the driver's replay and is idempotent; a log closed after
its client is freed, and the driver ends the replay by itself. When the ABI could not
stop the replay, `log.close()` throws `CavalryException` after it has freed the log,
and a second close does nothing. `log.memberId()` and `log.recordingId()` say whose
log it is (`spec/c-abi.md`, "Functions", committed log; ADR 0070).

An ended log delivered every committed entry it may read only when three things hold:

- `log.failedAt()` is empty. A position is where the driver's replay of the member's
  recording failed, and no entry from it on was delivered.
- `log.withdrawnAt()` is empty. A position is where the entries begin that the log was
  not given, because the driver withdrew the replay, 5 s after it ended, before the
  service had read it.
- `log.faulted()` is empty. A `Subscription.Fault` is where this client's read of the
  replay's image, the log's own, stopped: its `code()` is `Abi.ERR_FAULT_IMAGE` when a
  frame of the image failed a check, or when the image was gone before this client
  read it, and `Abi.ERR_FAULT_IMAGES_FULL` when the image could not join. No entry
  from the fault on was delivered.

Read the three reports, two `OptionalLong` and an `Optional<Subscription.Fault>`, once
`log.isEnded()` is true. The driver withdraws a replay that failed as it does any
other, so the first two can both be positions: `log.withdrawnAt()` is then the lower,
and the entries from it on, those below `log.failedAt()` among them, were not
delivered. The poll that meets the image's fault returns −1 once, `log.fault()`
answers it until the next poll, and the next poll returns 0 and clears it there.
`log.faulted()` answers it from that poll on, for as long as the log is held
(ADR 0080). `log.withdrawnAt()` is empty for such a log, and `log.failedAt()` is a
position only when the driver's replay failed as well, which is then the cause to
look at first. In every case what the log delivered ends at `log.position()`.

A committed log has an upcall stub of its own, as a subscription has. A handler's
exception goes where a subscription's goes ("The exception"): every entry the ABI
delivers reaches the handler, and `poll` throws `HandlerException` once the ABI has
returned, with the poll's result as its `fragments()`, the −1 of a fault included. An
entry whose handler call threw was delivered and is not delivered again:
`log.position()` is past it, and past every entry the same poll delivered after it. A
handler cannot stop a poll: the ABI goes on handing it entries up to the fragment
limit. So a service that must apply every entry, in order, does this:

1. The handler catches its own exceptions. When it cannot apply an entry, it keeps
   that entry's `position` and sets a flag.
2. While the flag is set the handler applies nothing and returns at once, for the rest
   of that poll: the poll still hands it the entries after the one that failed.
3. After the poll, the service closes the log and opens a new one at the position it
   kept.

With entries at 0, 96 and 192 and a failure at 96, opening again at `log.position()`
would lose the entry at 96, and applying 192 before opening again at 96 would apply
192 twice. A `fragmentLimit` of 1 is the simple form: a poll then delivers at most one
entry, and the service decides after each.

### The cluster session

A cluster session is a client's connection to a cluster: it sends the leader messages,
which the leader appends to the cluster's log, and reads the replies of the service
beside the leader (`spec/cluster.md`, "Sessions"). `client.connectCluster(members,
responseChannel)` opens one and returns a `ClusterSession`. `members` is the members
string the cluster's members were started with (`docs/how-to/run-a-cluster.md`), and
`responseChannel` a channel of this host for the cluster's answers,
`cavalry:udp?endpoint=<host>:<port>`. The call returns once the leader has opened the
session and waits for that for 20 s at most; it throws `CavalryException` with
`Abi.ERR_CLUSTER_MEMBERS`, `ERR_CLUSTER_REFUSED`, `ERR_CLUSTER_VERSION` or
`ERR_CLUSTER_CONNECT_TIMEOUT`.

`session.offer(message)` sends the leader a `MemorySegment` of at most
`session.maxMessageLength()` bytes, a native one or a heap one over an array, and
returns the position after the message on the session's publication to the leader, or
a publication's result code below zero: `Abi.PUBLICATION_CLOSED` for a session that is
closed, and `Abi.PUBLICATION_ERROR`, with `Abi.ERR_TOO_LONG` as `Abi.errcode()`, for a
message that is too long. A message taken is on its way to the leader and is not
committed. `session.poll(handler, fragmentLimit)` calls `handler.onReply(body, reply)`
for each reply: `body` is the reply's payload, a `MemorySegment` into the log for the
call, and `reply` is a `ClusterReplyValues` with `reply.leadershipTermId()`, the term
of the entry the reply answers, and `reply.timestamp()`, the cluster's time when its
leader appended that entry, in milliseconds since 1970-01-01T00:00:00Z. `reply` is the
one object the session keeps, over a struct filled for each reply, so copy a value out
to keep it. The poll returns the count of replies handed over, or −1 when it stopped on
a fault, which `session.fault()` gives until the next poll. The poll is the session's
duty cycle as well: it sends the keepalive, follows a new leader and closes the
session when the cluster says so. Call it at least every 500 ms.

A reply is no delivery. The terms of the replies a handler is given never fall: a
reply of a term below one already handed over is stale, is passed over, and is counted
in `session.staleReplies()` (ADR 0121). A message whose leader was lost before it
committed is answered by nobody. A program that needs an answer offers again when
none comes. `session.closed()` is `null` while the session is open, and then the
reason: `"TIMEOUT"`, `"NO LEADER"` or the detail of the cluster's closed event.
`session.leaderMemberId()` and `session.leadershipTermId()` say whom the session
sends to, and `session.sessionId()` is the id the leader gave it. `session.close()`
offers the leader a session close for at most 500 ms and frees the session; it is
idempotent, and a `ClusterSession` is an `AutoCloseable`. Close a session before its
client (`spec/c-abi.md`, "Functions", cluster session; ADR 0122).

### The cluster service

A cluster's service is the program beside each member that applies the committed
entries and answers the sessions (`spec/cluster.md`, "The service" and "Replies").
`client.clusterService()` makes one beside the member on the client's driver and
returns a `ClusterService`; it sends nothing. `service.poll(handler, fragmentLimit)`
opens the member's log when the member has one, and calls
`handler.onClusterEntry(body, entry, service)` for each committed entry, in order:

- `body` is a session message's payload, a `MemorySegment` into the log for the call.
  It has no bytes for every other entry.
- `entry` is a `ClusterEntryValues`: `entry.type()`, one of
  `Abi.CLUSTER_ENTRY_SESSION_MESSAGE`, `_SESSION_OPEN`, `_SESSION_CLOSE`,
  `_NEW_LEADERSHIP_TERM`, `_OTHER` and `_SNAPSHOT`; `entry.position()`;
  `entry.leadershipTermId()`;
  `entry.timestamp()`, the cluster's time, the one clock a service reads, so that
  every member's service reaches the same state; `entry.sessionId()`, −1 where the
  entry has no session; `entry.leaderMemberId()`, for a new leadership term; and
  `entry.timedOut()`, for a session close. It is the one object the service keeps,
  over a struct filled for each entry, so copy a value out to keep it.
  `_SNAPSHOT` is the leader's request for a snapshot: the poll ends after it, and
  the program gives its state to `service.snapshot` before its next poll ("A
  service's snapshot and its start", below).
- `service` is the service that was polled, for `service.reply`.

What the handler returns says what becomes of the entry. `Abi.ACTION_CONTINUE` takes
it, and the poll goes on. `Abi.ACTION_BREAK` takes it and ends the poll.
`Abi.ACTION_ABORT` leaves it and ends the poll, and the next poll hands it over again.
The poll returns the count of entries taken, or −1 when it stopped on a fault, which
`service.fault()` gives until the next poll.

`service.reply(sessionId, message)` answers a session from inside the handler and
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
entry aborted too, and `poll` throws `HandlerException` ("The exception"): an entry is
taken only by a handler that finished with it, so `service.position()`, the position
after the last entry taken, never passes an entry whose handler failed.

The service's `poll` and its `close` are calls on the client the service was made on
("Threads"). `service.close()` is idempotent, and a `ClusterService` is an
`AutoCloseable`; close a service before its client (`spec/c-abi.md`, "Functions",
cluster service; ADR 0123; ADR 0124). `conformance/jvm/ClusterRound.java` is a program
that runs three services and a session through every call above.

### A service's snapshot and its start

A service's program holds the cluster's state, and a snapshot is that state as the
program's own bytes at one position of the log, the same beside every member. A
service that starts again takes its state from its last snapshot and reads the log
after it, where it would read the log from position 0 (`spec/c-abi.md`, "A service's
snapshot and its start"; ADR 0146; ADR 0149). Three calls, none of them from inside
the handler:

- `service.restore()`, before the first poll, returns a `ClusterService.Restored`,
  with `restored()` and `state()`. With `Abi.CLUSTER_RESTORED_STATE`, the state is
  the program's state as it gave it, a `byte[]` that is a copy the caller owns; the
  polls read the log from the snapshot's position. With
  `Abi.CLUSTER_RESTORED_FROM_START`, the state is null, and the polls read from
  position 0. With `Abi.CLUSTER_RESTORED_NO_MEMBER`, the state is null; the driver
  hosts no member with a log yet, and the program calls again before it polls.
- `service.snapshotDue()`, after a poll, is an `OptionalLong`: the snapshot's log
  position when the poll ended at the leader's request, an entry whose `type()` is
  `Abi.CLUSTER_ENTRY_SNAPSHOT`, and empty otherwise.
- `service.snapshot(state)`, before the next poll, writes the snapshot and tells the
  member, which lists it. `state` is a `byte[]`, 1 GiB at the most, which the call
  copies to native memory for its run. It returns `true`, or `false` when the member
  lists one at or past that position already.

So a program's loop is this:

```java
ClusterService.Restored found = service.restore();
while (found.restored() == Abi.CLUSTER_RESTORED_NO_MEMBER) {
    Thread.sleep(1);                     // no member with a log yet: ask again
    found = service.restore();
}
if (found.state() != null) {
    load(found.state());                 // the program's own: bytes to its state
}
while (running) {
    service.poll(this::onEntry, 10);
    if (service.snapshotDue().isPresent()) {
        try {
            service.snapshot(dump());    // the program's own: its state to bytes
        } catch (CavalryException failed) {
            report(failed);              // nothing committed is lost: poll on, and the
        }                                // next request asks again
    }
}
```

The state must be a function of the entries the handler was handed, and of nothing
else: every member's service gives its own, and a start from any of them must reach
what the others hold. A program that never calls `restore` reads the log from
position 0 at every start, and its member's log can then not be purged.

`snapshot` and `restore` wait for the driver under the client's lock: a second at the
most for each step of a snapshot's own, the response timeout, 10 s, for an add or a
command that a driver does not answer, and for as long as the snapshot's recording
takes to play back at a start. Each throws `CavalryException` with the ABI's code:
`Abi.ERR_CLUSTER_NO_SNAPSHOT_DUE`, `Abi.ERR_CLUSTER_SNAPSHOT_TOO_LONG`,
`Abi.ERR_CLUSTER_SNAPSHOT_FAILED` with the step in the message,
`Abi.ERR_CLUSTER_RESTORE_TOO_LATE` once the service has begun, and `Abi.ERR_POLLING`
from inside a handler. After an exception of `snapshot` the snapshot is still due, and
the program calls again or polls on. `conformance/jvm/SnapshotRound.java` is a program
that runs every call above beside three members.

## What it must never do

- Never run a message through anything but one ABI call: no lock, no `synchronized`
  block, no wait, no future, no thread, no copy of a fragment into a new array for the
  handler. `clients/lint.py` reads the per-message methods of `Publication`,
  `Subscription`, `CommittedLog`, `ClusterSession`, `ClusterService` and `Header`, the
  claim, the reserved value, the commit, the abort, the offer, a session's offer and a
  service's reply, each poll, the header's nine getters, the getters of a reply's
  values and of an entry's, and each upcall
  that calls a handler, for Java's forms of each: `synchronized`, as a modifier or a block, the lock
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
  caller branches. A cluster session's `offer` returns the same codes, and a cluster
  service's `reply` returns its `Abi.CLUSTER_REPLY_*` code, or −1 from outside the
  handler.
- Never swallow the handler's exception, or a fragment: a fragment handler that throws
  keeps every later fragment of that poll reaching the handler, and `poll` throws a
  `HandlerException` once the ABI has returned, with the first throwable as its cause
  and the counts. An exception must not unwind through the ABI's frames, where the
  runtime would end the process, so the upcall catches every throwable the handler
  raises. The upcall's two lines before the handler's call, the body's `reinterpret`
  and the header's two field stores, are outside that catch: a throwable the runtime
  raises there, an `OutOfMemoryError` as the body's segment is made, ends the process.
- Never take an entry for a cluster service's handler that failed. The handler's
  return value is the entry's action, so the upcall answers for a handler that threw,
  with `Abi.ACTION_ABORT`, and aborts an entry whose action the ABI refused: the poll
  ends there, and `poll` throws. The entry is handed over again; it is never skipped.
- Never lose a handle the ABI keeps open. Every close frees its handle, whatever it
  returns, so `close()` forgets the handle once the ABI's close has returned. The one
  exception is a close that the ABI refuses with `Abi.ERR_POLLING`, which frees
  nothing and leaves the handle open (`spec/c-abi.md`, "Calls during a claim or a
  poll"). The ABI refuses three kinds of close. The first is the close of a
  subscription, of a committed log, of a cluster session or of a cluster service from
  inside a handler of its own poll. The second is a cluster service's close from
  inside the handler of another service whose poll holds its client. The third is a
  `Client`'s close from inside the handler of a cluster service whose poll holds that
  client. `close()` then throws `CavalryException` and keeps the handle, so that the
  program can still use the handle, and close it, after the poll has returned. A call
  after a close hands the ABI a null handle, as the Python and Go clients
  do, and the ABI refuses it with `Abi.ERR_NULL`: a poll answers -1, a claim
  `PUBLICATION_ERROR`, never a delivery and never a frame.
- Never read a refusal as an answer. A `poll` from inside a handler of the same
  subscription returns −1 with `Abi.ERR_POLLING`, and the poll in progress goes on. A
  committed log's handler is held to the same rule. From inside it, a `poll` of the
  same log, `log.position()` and `log.commitPosition()` return −1, `log.isEnded()` is
  `false`, `log.failedAt()`, `log.withdrawnAt()` and `log.faulted()` are empty, and
  `log.fault()` and `log.close()` throw `CavalryException`, each with
  `Abi.ERR_POLLING`, and the log stays open. The `false` and the three empty answers
  are refusals there, not answers: read the four after the poll has returned. The
  handler has each entry's own `position`. A cluster session's handler and a cluster
  service's are held to it as well. From inside a session's handler, `offer` returns
  `Abi.PUBLICATION_ERROR`, a `poll` of the same session, `leadershipTermId()`,
  `leaderMemberId()` and `staleReplies()` return −1, `closed()` is `null`, a refusal
  there, and `fault()` and `close()` throw `CavalryException`, each with
  `Abi.ERR_POLLING`: a handler that answers a reply with a message keeps what it needs
  and offers after the poll has returned. From inside a service's handler, a `poll` of
  the same service, `position()` and `sessions()` return −1, `answers()` is `false`, a
  refusal there, and `fault()` and `close()` throw `CavalryException`. The service's
  poll holds its client too, so every call on that client from inside the handler is
  refused the same way: `client.isClosed()` is `false`, a refusal, `client.closed()`
  is −1, and an add, a command, `client.committedLog`, `client.connectCluster`,
  `client.clusterService` and `client.close()` throw `CavalryException` with
  `Abi.ERR_POLLING`. A refused `client.close()` frees nothing, and the client stays
  open. Make these calls after the poll has returned.
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
`conformance/jvm/Quickstart.java`'s report measures of it. A figure below is one run's
on the laptop (OpenJDK 25.0.5, macOS 26.6 on arm64, unpinned) unless it names a CI
run; every CI run prints its runners'.

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
critical call adds to a collection that wants to start is the copy's length. A cluster
session's offer and a cluster service's reply are bound the same way: each is a
publication's offer underneath, and takes a message over a `byte[]` as it does. A
downcall costs 2.9 ns on this laptop for a function that returns at once
(`cavalry_abi_version`, 5,000,000 calls after 100,000 that warm the compiler). An add,
a command or a close waits on the driver for up to 10 s (`spec/c-abi.md`, "Threads");
the thread blocks in the call. `client.connectCluster` waits until the leader has
opened the session, 20 s at most.

A cluster service's `poll` is the one per-message method that can wait for the
driver. It sends commands with the service's client when it opens the member's log,
at a session's opening, where a term's leader changes and at a recording's end, and
it waits for each answer in the call, as an add does; a session message costs no
command. The runner's cluster round reports the waits. On this laptop (Apple M5 Pro,
Java 25.0.5), in each of 25 runs, 5 to 7 of about 17,900 polls of three services took
over 1 ms. The longest took 7.5 to 22.2 ms.

### Threads

A `Client`'s methods are `synchronized`, so threads may share one: an add, a command
or a close holds the client's lock for its one ABI call, which the ABI serialises
anyway, and the lock is never on a message's path. A `Publication`, a `Subscription`
or a `CommittedLog` and its `Header` are used from one thread at a time, the ABI's rule for every handle;
a program with several threads gives each its own, as the Rust API's are used.
`Client.addDestination` and `removeDestination` are calls on the client, not on the
`Publication` they name: they may run beside that publication's `offer` or `tryClaim`
in another thread, but not beside its `close` (`spec/c-abi.md`, "Threads"; ADR 0057).
The upcall stub a subscription makes is bound to the subscription itself, so the ABI's
`clientd` is unused and no table is read per fragment. A `ClusterSession` and a
`ClusterService` are used from one thread at a time too, and each makes a stub bound
to itself.

A `ClusterService` changes what holds for one client. Its `poll` is a call on its
client as well as on the service: the ABI's poll sends its commands with the client
and holds it for the run of the handler. `poll` is on every entry's path, so it is
not `synchronized` and takes no lock: it reads the client's handle through an
accessor that takes none. So from `client.clusterService()` until the service's
`close()`, that client is used by the thread that polls the service and by no other.
A program whose other threads add or command gives them a `Client` of their own, as
`spec/c-abi.md`, "Threads", says for a program in C. The lock would be on a message's
path, where another thread's add could hold it for up to 10 s (ADR 0124,
Alternatives). The service's open and its close take the client's lock, as a
committed log's do, and so do its `snapshot` and its `restore`, which the thread that
polls makes between polls: each sends commands with the client and waits for the
driver. A `ClusterSession`'s connect is a call on the client and takes
its lock; nothing else of a session is.

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
round below. The body is never copied. A committed log's upcall, `CommittedLog.onEntry`,
makes the same four and one downcall more per entry before the handler:
`cavalry_committed_log_entry_position` on the header, for the position it hands the
handler, a `long`, no object.

There is no one view per subscription reused across messages, as the card asked and
as the Rust API's slice is, because a `MemorySegment` cannot be pointed somewhere else
once made. Three other forms lost:

- One segment over the whole address space, handed to the handler with the body as an
  offset into it, would make no object of the client's own per message. It would also
  hand the handler every byte of the process.
- The body as a slice of a private segment over the address space, the form the claim's
  view takes ("The view over a claimed slice"), is one object, the same as the
  `reinterpret` it would replace. It saves nothing here, because the runtime has
  already made the body's segment of length zero before `onFragment` runs.
- The stub's descriptor could declare its three pointer parameters `JAVA_LONG`. The
  runtime would then pass three numbers and make no segment, and the body would be one
  slice: one object per message instead of four. But `cavalry_fragment_handler_t` takes
  three pointers, and the API leaves unspecified what a stub does whose descriptor does
  not match the C signature. So the stub keeps `ADDRESS`, and the runtime makes its
  three objects.

The objects cost what "The count" says.

The `Header`'s getters read `cavalry_header_values` once per fragment, on the first
getter, into the one native struct the subscription keeps, and return the field as a
primitive, no object. Outside the handler call every getter throws
`IllegalStateException`, since the header is valid for the call alone. The body's
segment is valid for the call alone too: it points into the log, which the publisher
writes over once the subscription's position has moved past it. A segment kept past
the call reads whatever the log holds later; copy what must outlive the handler. The
body's segment is writable in Java's view, but a subscriber maps the log read-only
(`Map::Reader`, `crates/cavalry-shm/src/log/file.rs`), so a write through it faults
on a read-only page; copy the body to change it.

A cluster session's upcall and a cluster service's make the body's segment in the
same way, and one downcall more for each reply or entry, `cavalry_cluster_reply_values`
or `cavalry_cluster_entry_values`, which fills the one native struct the session or
the service keeps. The handler is handed the one `ClusterReplyValues` or
`ClusterEntryValues` over that struct, whose getters read a field as a primitive, and
a service's handler the service itself: no object is made for them. A service's
upcall then makes one downcall more for a handler that returned `Abi.ACTION_BREAK` or
`Abi.ACTION_ABORT`, `cavalry_cluster_service_action`; `Abi.ACTION_CONTINUE` makes
none. An entry that is no session's message has a body of no bytes: the ABI's address
for it is 1, the segment is made over no byte of it, and a read through it throws.

### The view over a claimed slice, by construction

`publication.claim()` is a `MemorySegment` over the bytes the ABI reserved and over
nothing else. `Publication` keeps one private segment over the whole address space,
`EVERYTHING`, made once when the class loads and never handed out. `tryClaim` reads
the address the ABI filled into the one `cavalry_buffer_claim_t` the publication keeps
as a `long`, and takes the slice of `EVERYTHING` at that address and of the claimed
length: one `asSlice`, so one object per claim. The address is read as a `long`
because every layout of this package is generated with 8-byte pointers. `commit`,
`abort` and `close` set the segment to `null`. A segment kept past the commit points
into the log, as a body does.

The form that lost reads the address as a segment, `get(ADDRESS, …)`, which makes a
segment of length zero, and calls `reinterpret` on it for the claimed length, which
makes a second segment: two objects per claim. Both forms measured 206.5 bytes per
message ("The count, measured"), since the compiler removed that form's first segment.
The one slice won because its one object per claim is the construction's and does not
depend on what the compiler removes.

### The count, measured

The runner's allocation round sends 10,000 messages whose handler only counts and whose
publisher copies one constant, with the thread's allocated bytes read from
`com.sun.management.ThreadMXBean` before and after the round with nothing between,
which counts every object the thread allocated, freed or not. The construction makes
five objects per message, the claim's one slice and the call-back's four. With it:
206.5 bytes per message, with 0 collections during the round, on CI's Linux runner
(Temurin 25.0.4, x64) and on CI's macOS runner (Temurin 25.0.5, arm64), both in run
42208103147.

The claim's two-object form, six objects per message, measured the same 206.5 bytes on
the laptop and on CI's Linux runner (Temurin 25.0.4, run 42205547531), with 0
collections. The two counts are equal, so with that form the compiler had already
removed one object, the claim's segment of length zero, which it can prove dies inside
`tryClaim`. The one-slice form makes five objects without that removal, and its count
is the same five objects' worth, 41.3 bytes for each. Every CI run prints the count, in
the line `conformance/jvm: report: allocations per message` of the gates job.

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
what to do. `fragments()` is the poll's result: the count, or −1 when the poll also
stopped on a fault, which `fault()` reads. A committed log's `HandlerException` has
`committedLog()` true and speaks of entries, and when `fragments()` is −1 it names
`log.fault()`, to read before the next poll, and `log.faulted()`, which keeps the
fault of the log's own image ("The committed log"). The throwable is
kept as thrown, an `Error` included, and stays the first when another follows it in
the same poll. A cluster session's handler is held as a subscription's is: every reply
the poll hands over reaches it, and the `HandlerException`, whose `cluster()` is
`"session"`, speaks of replies and names `session.fault()`.

A cluster service's handler is the one whose failure stops its poll, because what it
returns is read: it is the entry's action. When the handler throws, the upcall keeps
the throwable and answers `Abi.ACTION_ABORT` for the entry, so the ABI ends the poll
there with the entry not taken. `poll` then throws `HandlerException`, whose
`cluster()` is `"service"`: `fragments()` is the count of entries the poll took before
that entry, `raised()` is 1, and the cause is what the handler threw. A handler that
returns a value that is no action, such as 3, is treated the same way. The ABI
refuses the value, the upcall reads the thread's error where the call failed and
aborts the entry, and the cause is a `CavalryException` with `Abi.ERR_CLUSTER_ACTION`
and the ABI's message, which names the value. The next poll hands the aborted entry
over again, and throws again until the handler takes it: the service does not skip an
entry, since every member's service must apply the same entries. A reply the failed
handling sent is not sent twice. What the handler changed of its own state before it
threw is the handler's to guard, which is why it replies first and changes its state
last ("The cluster service"). The runner checks it: a handler that replies and then
throws is handed its entry twice at one position, and the session reads one reply.

## What to read next

- `spec/c-abi.md`: the page the library wraps, function for function.
- `clients/README.md`: how a client is generated and what every client passes.
- `clients/python/README.md`: the first client, whose shape this one copies.
- `conformance/jvm/`: the runner and its report.
- `docs/decisions/0046-build-the-c-abi-and-the-clients-fourth.md`: the slice this
  client is the fifth feature of.
- `docs/decisions/0124-give-each-client-a-session-object-and-a-service-object-whose-handlers-failure-aborts-its-entry.md`:
  the shape of the cluster session and the cluster service in every client, why a
  failed handler's entry is aborted, and why a service's poll takes no lock.
- `docs/how-to/run-a-cluster.md`: how to start the members a session connects to.
