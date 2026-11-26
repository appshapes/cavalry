# conformance

The suite `CLAUDE.md`'s gate 1 names: every client against `spec/`. `just conformance`
runs, in this order, the Rust vector suites of `cavalry-proto`, `cavalry-shm`,
`cavalry-log` and `cavalry-ffi` (each page's vectors under `spec/vectors/`, the C
ABI's held against the committed header), then one runner per client in this
directory, each a program in its language that runs the ten-minute path's shape
against a driver through `crates/cavalry-ffi/include/cavalry.h`: publish and subscribe
over `cavalry:ipc` and `cavalry:udp`, record and replay, every message once and in
order, with the header's fields and each frame's position checked against the pages.
A runner that fails prints the function and the code, with `cavalry_errmsg` when the
call set the thread's error (a result code such as `CAVALRY_PUBLICATION_CLOSED` sets
none, so the runner names the code itself), and exits 1. Every wait in a runner is
bounded at 10 s, and one that runs out says how far it got.

| Runner | Language | Runs with | Decided in |
| --- | --- | --- | --- |
| `c/` | C11, `quickstart.c` and then `cluster.c`, the cluster round, beside three cluster members that `run.sh` starts with `cavalry run`, then `snapshot.c`, the snapshot round, beside all three, then `closed.c`, a service's poll after its driver's stop (ADR 0155), through `run.sh` | the runner's `cc`, the static library | ADR 0051 (card 204); ADR 0122 and ADR 0123 (work item 229); ADR 0146 and ADR 0149 (work item 335) |
| `python/` | Python 3, `quickstart.py` and then `readme.py`, the example of `clients/python/README.md`, then `committed_log.py` beside one of three cluster members that `run.sh` starts with `cavalry run`, then `cluster.py`, the cluster round, beside all three, then `snapshot.py`, the snapshot round, beside all three, then `howto.py`, the program of `docs/how-to/take-a-services-snapshot.md`, beside three members of its own, then `howto_timers.py`, the program of `docs/how-to/schedule-a-services-timer.md`, beside three more, then `closed.py`, a service's poll after its driver's stop (ADR 0155), through `run.sh` over `clients/python` | the runner's `python3`, the shared library | ADR 0046 (card 205); ADR 0124 (work item 229); ADR 0149 (work item 335) |
| `node/` | Node, `quickstart.js` and then `readme.js`, the example of `clients/node/README.md`, then `committed_log.js` beside one of three cluster members that `run.sh` starts with `cavalry run`, then `cluster.js`, the cluster round, beside all three, then `snapshot.js`, the snapshot round, beside all three, then `closed.js`, a service's poll after its driver's stop (ADR 0155), through `run.sh` over `clients/node` | the runner's `node` and `npm`, whose own `node-gyp` builds the addon against the static library | ADR 0046 (card 206); ADR 0124 (work item 229); ADR 0149 (work item 335) |
| `go/` | Go, the client's own `go test` (its subscription table at the bound), then `quickstart` and then `readme`, the example of `clients/go/README.md`, then, beside one of three cluster members that `run.sh` starts with `cavalry run`, the client's one test that needs a member (a committed log's open with the table full) and `committed_log`, then `cluster`, the cluster round, beside all three, then `snapshot`, the snapshot round, beside all three, then `closed`, a service's poll after its driver's stop (ADR 0155), through `run.sh` over `clients/go` | the runner's `go`, whose cgo builds the client against the header and the static library | ADR 0046 (card 207); ADR 0124 (work item 229); ADR 0149 (work item 335) |
| `jvm/` | Java 25, `Quickstart.java` and then `Readme.java`, the example of `clients/jvm/README.md`, then `CommittedLogRound.java` beside one of three cluster members that `run.sh` starts with `cavalry run`, then `ClusterRound.java`, the cluster round, beside all three, then `SnapshotRound.java`, the snapshot round, beside all three, then `ClosedRound.java`, a service's poll after its driver's stop (ADR 0155), through `run.sh` over `clients/jvm` | the runner's `javac` and `java`, whose `java.lang.foreign` binds the shared library when the client loads | ADR 0046 (card 208); ADR 0124 (work item 229); ADR 0149 (work item 335) |
| `dotnet/` | C#, `Quickstart` and then `Readme`, the example of `clients/dotnet/README.md`, then `CommittedLog` beside one of three cluster members that `run.sh` starts with `cavalry run`, then `Cluster`, the cluster round, beside all three, then `Snapshot`, the snapshot round, beside all three, then `Closed`, a service's poll after its driver's stop (ADR 0155), through `run.sh` over `clients/dotnet` | the runner's `dotnet`, whose SDK builds the client and whose runtime binds the shared library when the client loads | ADR 0046 (card 209); ADR 0124 (work item 229); ADR 0149 (work item 335) |

A committed log round reads beside a member whose log is one recording that begins at
position 0, and checks the recording's id. Each `run.sh` asks `log-member.sh` which
member that is: member 1, unless it joined its leader's log late and holds its log in
two recordings, and then member 0 or member 2. The script reads each member's file of
its log's recordings in its driver directory (`spec/cluster.md`, "The log's recordings";
ADR 0103) and prints the member's id and the recording's. It waits 60 s for such a
member, since a member writes its file as it joins the log, and then fails with what
each member's file holds. A member renames each new file over the one before, so the
script opens a member's file once per read, copies its 40-byte header, and takes every
field from the copy: two opens could find two files, and mix their fields. It reads the
integers through `od` in hex, because
`od`'s signed decimal of eight bytes is wrong on macOS 26.6, and before it reads a file
it reads eight bytes it knows to be 4096 and chooses no member on a host that reads them
otherwise.

A runner picks a loopback port by binding a socket to port 0, reading the port the
system gave and closing the socket. Nothing holds that port afterwards, so a second
pick can be given the same one. A step that needs several ports that must differ
takes them together: every socket is bound before any is closed. `c/quickstart.c`'s
step 2b, a control address and two destinations, takes its three from
`free_udp_ports`, and `run.sh` runs that picker alone first, `quickstart-c
--pick-ports 20000`, which fails on a draw with two ports the same (work item 461).
The other runners' quickstarts take one port in a step. The tests under `crates/`
take theirs by number, each once in a process, from
`crates/cavalry-raft/tests/ports/mod.rs`.

The cluster round is a runner's check of a cluster's service and of a session with a
cluster (`spec/c-abi.md`, versions 4 and 5; `spec/cluster.md`, "Sessions", "The service"
and "Replies"). Every runner's round also holds a program's timer (version 7; ADR
0157): each service schedules one at a message it takes, for a second later, and
all three are handed it once, as an entry of its own kind with the timer's id, at
one position and no earlier than its deadline; and each schedules a second timer,
due at once, and cancels it in the same handling, and none is handed it. The
second before the first timer is the check's margin: the event of a timer that is
due at once is stamped tens of milliseconds after its message, once the message's
entry is committed and read by a service and the service has told its member, so
a deadline of 50 ms, which these rounds had first, is a thin margin for telling a
timer that was wrongly due at once from one that waited. The five clients' rounds
print what that was in their own run, the time of their first timers that were
due at once, beside the deadline. In ten runs of each on a laptop (Apple M5 Pro)
it was 15 to 27 ms. Before ADR 0168 a service's first timer waited for its
publication to its member too, and in the runs made for work item 419 it was 38
to 68 ms, the .NET round's the latest. The Python round also measures what a
later timer costs: at the end of its round it sends 200 messages, one at a time,
at each of which every program schedules one more timer, due at once, and it
prints their histogram. In one run with 10,000 of them, which
`CAVALRY_LATER_TIMERS=10000` asks for, the leader stamped the event after the
message's time, nearest rank, at p50 20 ms, p99 28 ms, p99.99 38 ms and max 38
ms, and the service beside the leader was handed each timer, after it scheduled
it, at p50 19,352 µs, p99 26,246 µs, p99.99 31,677 µs and max 38,186 µs. In the
ten runs of 200 the stamps were 11 to 42 ms. So a first timer costs what a later
one costs. Both measures hold the round's own cycle, one thread that polls three
services and a session and sleeps 200 µs between cycles; no run's number is a
bound, and work item 493 asks where the 20 ms go. Those five rounds
hold more of the client's own: a second schedule of the timer sets its deadline,
where its first was an hour away; the least and the greatest int64, scheduled
as ids, are handed back as they are, and in the Node round the least and the
greatest integer a number carries exactly; both calls are refused from outside
the handler, and, in Python and Node, an id or a deadline that is no int64 before
the ABI is called, in Node a number with a fraction or outside those integers
among them, from inside the handler too, where Go's `int64` and Java's and C#'s
`long` leave nothing to refuse; and no timer is handed to a service a second time in a
timer refresh interval and more. What a runner does not hold of a service's timers rests on the library's
tests, which every client reaches through the same ABI: the bound of 1,024 and
the refusal at it, a handling that is not taken leaving every timer as it was, a
timer event passed over and counted, a cancel of a timer whose event is in the
log, and the timers in a snapshot (`crates/cavalry-proto/tests/cluster_timers.rs`,
the table's rules against a model under seeded calls;
`crates/cavalry-client/tests/cluster_service.rs` and
`crates/cavalry-ffi/tests/cluster_service.rs`, a service beside real drivers;
`crates/cavalry-raft/tests/timer_search.rs` and `snapshot.rs`, three members;
`just replay-check`). Every runner has the round: the C runner, in `c/cluster.c`, the Python
runner, in `python/cluster.py`, the Node runner, in `node/cluster.js`, the Go runner,
in `go/cluster`, the JVM runner, in `jvm/ClusterRound.java`, and the .NET runner, in
`dotnet/Cluster`. It runs a service beside each of
the three members, each over a client on that member's driver, and polls each until it
has read an entry, since a service answers no entry that its member held when the
service first opened the member's log. Then it opens a session with the cluster on the
runner's first driver, sends one message, and polls the services and the session until
all three services have taken the message and the session has read a reply, and for
300 ms more. It checks that the three services were handed one entry, with one
position, term, time and session; that one service answered it, the one beside the
leader, and the two others were answered `CAVALRY_CLUSTER_REPLY_NOT_ANSWERING`; that
the session read one reply, that service's, with the entry's term and the cluster's
time; and what `spec/c-abi.md` says a service's header carries and what its poll holds.
Its waits are bounded at 60 s, the time a cluster may take to commit on a busy host.

A client's cluster round is the C runner's through the client's session object and its
service object, and then the client's own edges (ADR 0124), of which the Python round
is the shape. What `offer` refuses. The calls `spec/c-abi.md` refuses from inside a
service's handler, on the service and on the client its poll holds, and from inside a
session's, each made after the thread's error was set to another code. A service's
handler that replies and then raises, and one that returns what is no action, in
Python `True` and then 3: each
entry is aborted, handed over again at the same position, and answered once. A reply
too long for a frame and a reply to a session that is not open. A handler that returns
break with two entries waiting, whose poll takes one. A session's handler that raises,
whose reply is not handed over again. The session's close, read by every service as
an entry. A session and a service closed twice, and a service closed after its
client. The Python round then prints a report, never gated: how many polls the
services made, how many took over a millisecond, and the longest, which is how long a
poll held the GIL.

The Node round is the Python round through `clients/node`, with three edges of its
own. A service's handler that returns a promise, as an `async` handler does, has its
entry aborted as one that returns 3 has, and so has one that returns `true`, where
the Python round returns `True`. The one object of values a handler is handed
reads `undefined` once the call has returned, and a kept body reads as empty. And the
`Buffer`s the round's handlers were handed are all freed by one turn of the event
loop, which the round checks after its report. The report is the Python round's:
there the longest poll is how long a poll held the thread, and with it the event loop.

The Go round is the Python round through `clients/go`, where a handler that fails
panics, with three edges of its own. An entry that is no session's message reaches its
handler with a `nil` body, since the client makes no slice at the address the ABI
gives for no bytes. Each open session and each open service holds one slot of the
client's table, which each close frees, so that the table holds what it held before
the round. And the client's close from inside a service's handler, which the ABI
refuses and which frees nothing, leaves the client open: it still reaches the ABI, and
the service polls it again. Its report's longest poll is how long a poll held its
goroutine.

The JVM round is the Python round through `clients/jvm`, with two edges of its own.
Every message it offers and every reply it sends is a segment over a `byte[]`, which
a cluster session's offer and a cluster service's reply take as a publication's offer
does. And the client's close from inside a service's handler, which the ABI refuses
and which frees nothing, leaves the client open, as in the Go round: the service polls
it again, and the round closes it at its end. Its report's longest poll is how long a
poll held its thread.

The .NET round is the Python round through `clients/dotnet`, with one edge of its own.
The client's close from inside a service's handler, which the ABI refuses and which
frees nothing, leaves the client open, as in the Go and JVM rounds: the service polls
it again, and the round closes it at its end. A handler's body is a span, which cannot
leave its call, and an entry that is no
session's message has an empty one. It sleeps 1 ms between its duty cycles, where the
other rounds sleep 200 µs: `Thread.Sleep` counts in milliseconds and yields alone for
less, and a round that only yields spins beside the members. So its report counts
about a fifth of the polls another round's does. Its report's longest poll is how long
a poll held its thread.

The Python runner is the shape the Node, Go, JVM and .NET runners copy (ADR 0046,
cards 206 to 209): a package of another ABI version refused when it loads the library; the C
runner's checks through the client; the client's own edges, where a handler's exception
goes, what `offer` refuses, how long a view lives, and the calls `spec/c-abi.md` refuses
during a claim or a poll, which leave the client's state as they found it; then a report,
printed and never gated, from two rounds that check nothing else: the one-way p50, p99,
p99.99 and max over 100,000 messages, with the runtime's allocation counters off, and the
allocations per message on the client's side of the boundary over 10,000 more, counted
with what the runtime counts, switched on for that round alone (clients/README.md, "What
every client must pass").

## The snapshot round

The snapshot round is a runner's check of a service's snapshot and of its start from
one (`spec/c-abi.md`, version 6, "A service's snapshot and its start";
`spec/cluster.md`, "Snapshots"; ADR 0146, ADR 0147, ADR 0149). It runs after the
cluster round, beside the same three members and on the log that round left, so its
services read a term's first entry and a session's open, its messages and its close
before the request. Each client's runner has it, in a program that its `run.sh` runs
after its cluster round: `python/snapshot.py`, which is the shape, `node/snapshot.js`,
`go/snapshot`, `jvm/SnapshotRound.java` and `dotnet/Snapshot`. The C runner has it
over the header alone, in `c/snapshot.c`: its state is a count and an FNV-1a hash,
2,480 bytes, and from inside the handler it checks too that the refused read writes
no position and the refused start writes a null state.

A service beside each member asks where it starts and is told from the log's start,
and a second ask is refused. Each reads an entry, and a snapshot where none is due is
refused. Then the round runs `cavalry snapshot` beside each member: beside the leader
it exits 0 and prints the snapshot's log position, and beside each follower it exits
1 and names the leader. Each service's poll ends at the leader's request, with the
snapshot due at the position the command printed and one state beside the three
members; from inside the handler there, the read says nothing with the thread's error
at `CAVALRY_ERR_POLLING`, and the snapshot and the start are refused with it. Each
service gives its state, once, and is closed, since one service beside a member takes
snapshots. The round then reads each member's listing through the client: a new
service beside each member starts from the snapshot, with the state as it was given,
at the snapshot's position. A service starts so only when its member lists the
snapshot. The round asks for a second snapshot: each new service's first entry is
that request, at the first snapshot's position, so it was handed nothing below it,
and it gives its state, which a start beside each member is then given. Last, a
service that never asks where it starts reads from position 0 and meets both
requests, each with the state the other services held there, and is told at each
that its member lists a snapshot there already. The program's state is a count of
the entries read and a hash over them, 2,472 bytes, so that a snapshot holds it in
three messages. The round's waits are bounded at 60 s, and a snapshot that the
driver gave up at a step is given again. No session is open at either request: a
snapshot that holds sessions, and a start that takes them, are held through the ABI
by `crates/cavalry-ffi/tests/cluster_snapshot.rs`, and by no runner.

The Python runner then runs the program of `docs/how-to/take-a-services-snapshot.md`
as that page's steps do, in `python/howto.py`, beside three members of its own, and
fails unless the program prints the three lines the page says a reader will see.

It runs the program of `docs/how-to/schedule-a-services-timer.md` the same way, in
`python/howto_timers.py`, beside each of three more members of its own, which have
run for more than three seconds, as a reader's cluster has run for longer than the
program's first deadlines. It fails unless each program opens as the page says, a
tick and a tock at one position, 96 for this cluster, the tick 1,000 ms later than
the tock, and then a tick some timer events above, the page's own example lines
being held to the same shape; unless the three programs print the same lines; and
unless the program beside the member that leads, stopped and run again, prints its
earlier lines again from the log's start and then three more that another member's
program prints too, the third of which is of an event the leader appended after
the restart.

## The upgrade gate

`upgrade/run.sh` is the last thing `just conformance` runs, and the gate of
`CLAUDE.md`'s invariant 3, "Mixed versions run during an upgrade" (work item 231;
ADR 0136). It is no client's runner: it holds `spec/cluster.md`, "Versions and the
upgrade", on a cluster of two builds of Cavalry.

N+1 is the tree under test, built into `target/release`. N is the last tree of the
cluster protocol's version before this one, read from the history: the first parent of
the newest first-parent commit that moved the first line of `spec/cluster.md` to what
it is now. `CAVALRY_UPGRADE_FROM=<commit>` names another N. N is exported with `git
archive` and built into `target/upgrade/<N's first 8 hex digits>/`, once: the hash
names the tree, so the directory, with the exported tree in `src/` and the three
programs, is a cache, and N moves only when the page's version does. N's build is
kept on a host that keeps `target/`, as a laptop does. CI's cache drops built
programs: `Swatinem/rust-cache` keeps only `build/`, `.fingerprint/` and
`deps/` of each profile directory under `target/` when it saves, so CI's `gates` job
builds N on every run and prints `built N in <n> s` each time. The script needs the
whole history, and says so in a shallow clone.

It starts three members at N on the loopback with `cavalry run`, a service beside
each, and a client that adds numbers and reads each total back. Then it upgrades one
member at a time, each with its service, on the directories they had: a follower; then
the leader, so that the two that remain, one of each build, elect a leader in a ballot
that crosses the two versions; then the last member at N. The client adds numbers
after every step, from N's build first and from N+1's after the leader's stop.

After every start, stop and client run it asks `cavalry doctor` of each running
member, from that member's own build: one leader, every other member its follower in
its term, every election closed, and one position as every member's append position
and commit position. At the same moments it asks the newer build's `cavalry doctor`
of each running member, whatever build the member is of, and holds what the end of
the member's `note cluster-member` line says of the member's
`cluster.protocol_version` to the build the script started the member from (ADR
0137). A member at N+1 is named `it runs version 3 of the cluster protocol, 0.3.0`,
the number read from the first line of this tree's `spec/cluster.md`. Whether a
member at N has the counter is read from N's `counters.toml` alone, in N's exported
tree, and not from the version of N's page: every tree of version 0 lacks it, and so
does the first tree of version 1. When N's `counters.toml` holds no such counter,
`doctor` must say `it has no cluster.protocol_version, so it runs a build older
than that counter: version 0 of the cluster protocol, or the first build of version
1`. When it holds it, a member at N is named as one at N+1 is, by the first line of
the page in N's exported tree. Each step's line says what `doctor` named, as
`doctor of N+1 names member 1 at version 1 and members 0 and 2 with no
cluster.protocol_version`. At the end it holds four things. Every number the cluster
acknowledged is in the total that every member's service reached. The three services
were handed the same entries, with the same positions, terms and sessions. A service
that started at N+1 read the log that N wrote. And each member's log, replayed from
its log directory in the order its file of recordings gives, is the same bytes as the
other two, to the commit position the members reported. Every wait is bounded at
60 s. A failure prints the step, what `doctor` says of each member that runs and each
program's output, leaves the script's directory, and exits 1.

The script reads lines that both builds print: `cavalry run`'s `started` and
`closed` lines, the `note cluster-member` line of `cavalry doctor`, and the lines of
the two adder programs. N prints the lines of its own tree until the page's version
moves again, so a change that rewords one of them teaches the script the new wording
beside the old, in the same change. The clause that names a member's version is the
newer build's alone: the script asks no other build's `doctor` for it.

The script's header says how to run it by hand.
[Upgrade a cluster one member at a time](../docs/how-to/upgrade-a-cluster.md) is the
same upgrade for an operator.

## The log directory's upgrade gate

`upgrade/log-directory.sh` runs after `upgrade/run.sh`, and holds one rule for every
change of the durable log's format: a driver opens a log directory of the format before
it in place, with no step by an operator (work item 362;
[ADR 0141](../docs/decisions/0141-open-in-place-a-log-directory-written-by-the-tree-that-servers-run-and-build-that-tree-on-every-run.md)).
It is no client's runner either: it holds `CLAUDE.md`'s invariant 1 across two builds
of Cavalry, on the directory that Brigade's server keeps
([What does Brigade's server rely on?](../docs/explanation/what-brigades-server-relies-on.md)).

N+1 is the tree under test. The older builds are the trees that Brigade's servers in
production have run, constants of the script: 620a2e29, the pin of Brigade's server
0.9.0, which they ran until 2026-11-24, and d265af82, the pin of server 0.10.0, which
they run since. `CAVALRY_DIRECTORY_FROM=<commit>` names one other in their place, which
writes one directory alone, in the pinned shape or, with
`CAVALRY_DIRECTORY_SHAPE=moved-to`, in server 0.10.0's. On CI's macOS runner the script
builds 620a2e29 alone and writes server 0.9.0's directory alone, as it did before work
item 441: with the three directories written all seven ways, that runner lost
communication with GitHub partway through `just conformance` in 2 of 8 jobs. With
one tree it lost it too, in 2 of `master`'s 39 jobs: one tree does not prevent the
loss, and the gap between the two forms is not shown.
Linux, which the servers in production run, writes all seven.
[ADR 0160](../docs/decisions/0160-keep-the-log-directorys-gate-on-one-tree-on-cis-macos-runner-and-track-the-lost-runner-as-a-fault-of-its-own.md) says
why the exception stays, and work item 456 tracks the lost runner itself. ADR 0141
says when a tree is added.
[ADR 0156](../docs/decisions/0156-hold-in-the-upgrade-gate-every-directory-a-server-holds-each-trees-alone-and-the-trees-in-turn.md)
says when it goes, in place of ADR 0141's rule for that.
Each older tree is exported with `git archive` into `target/upgrade/<its first 8 hex
digits>/` and built there on every run, as `upgrade/run.sh` builds its own. Each build
is `cavalry` and one example, `feed-directory`. 620a2e29 has no such example, and
d265af82 has an older copy of it and of `crates/cavalry/tests/feed/`. The script puts
this tree's `crates/cavalry/examples/feed-directory.rs` and `crates/cavalry/tests/feed/`
in place in each exported tree first, over the older copy in d265af82's. Those files
are the server's use of Cavalry, which `just test` runs, and they use no call that
either tree lacks. So the script fails at an older build when a change makes them use one, and
says what to do then.

The older builds write three log directories as a server writes its own, each one a
server holds or may hold:

- 620a2e29 alone, in the pinned shape, server 0.9.0's: the copy each server keeps as
  its way back.
- 620a2e29, stopped, then d265af82 in place, which reads back what 620a2e29 left and
  writes in the shape server 0.10.0 moved to, `ssc=true` with `term-length=1m`: what
  each server holds now.
- d265af82 alone, in that shape: what a server first installed at 0.10.0 holds.

The first build to write a directory appends 20 records in two runs and releases twice,
which purges the first two recordings. A build that opens a directory another wrote
reads back every record left, appends nine, and releases once more. The script does
each directory twice. In the first, the last run is stopped as a server's clean stop
does. In the second, it is killed with `SIGKILL`, which leaves its last recording live
in the catalog. The directory written in turn is done a third time, with 620a2e29's
last run killed and d265af82's stopped, so that d265af82 recovers the recording that
620a2e29 left live. For each directory, as the older builds left it, the script then holds
with N+1's programs:

- `cavalry run` starts on it, and `cavalry replay` plays each recording that is left
  from its start to the position its writer answered.
- The feed, in the pinned shape, begins at the last position answered. It read the
  catalog before its driver started, as Brigade's server 0.10.0 does, and found the
  longest term among the recordings left: 16 MiB where one of 620a2e29's is left, 1 MiB
  where d265af82 wrote alone. It reads back every record left, each once, in order, at
  its position. A read from position 0, which was purged, is refused. It appends five
  records.
- The feed, in the shape server 0.10.0 moved to, begins where that ended, reads back
  what is left and those five, and appends five more.
- One more run reads all of it back: recordings that two or three builds wrote, in
  terms of two lengths.

A record read back is held to the line the feed's process wrote when it appended the
record: its length, a digest of its bytes and the position after it
(`crates/cavalry/tests/feed/process.rs`). So N+1's read is held to what each older
build was answered, and not to what that build reads back. Every wait is bounded at 60 s. A failure prints
the step and each program's output, leaves the script's directory, and exits 1. The
script's header says how to run it by hand.

