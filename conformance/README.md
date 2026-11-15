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
| `c/` | C11, `quickstart.c` and then `cluster.c`, the cluster round, beside three cluster members that `run.sh` starts with `cavalry run`, through `run.sh` | the runner's `cc`, the static library | ADR 0051 (card 204); ADR 0122 and ADR 0123 (work item 229) |
| `python/` | Python 3, `quickstart.py` and then `readme.py`, the example of `clients/python/README.md`, then `committed_log.py` beside one of three cluster members that `run.sh` starts with `cavalry run`, then `cluster.py`, the cluster round, beside all three, through `run.sh` over `clients/python` | the runner's `python3`, the shared library | ADR 0046 (card 205); ADR 0124 (work item 229) |
| `node/` | Node, `quickstart.js` and then `readme.js`, the example of `clients/node/README.md`, then `committed_log.js` beside one of three cluster members that `run.sh` starts with `cavalry run`, then `cluster.js`, the cluster round, beside all three, through `run.sh` over `clients/node` | the runner's `node` and `npm`, whose own `node-gyp` builds the addon against the static library | ADR 0046 (card 206); ADR 0124 (work item 229) |
| `go/` | Go, the client's own `go test` (its subscription table at the bound), then `quickstart` and then `readme`, the example of `clients/go/README.md`, then, beside one of three cluster members that `run.sh` starts with `cavalry run`, the client's one test that needs a member (a committed log's open with the table full) and `committed_log`, then `cluster`, the cluster round, beside all three, through `run.sh` over `clients/go` | the runner's `go`, whose cgo builds the client against the header and the static library | ADR 0046 (card 207); ADR 0124 (work item 229) |
| `jvm/` | Java 25, `Quickstart.java` and then `Readme.java`, the example of `clients/jvm/README.md`, then `CommittedLogRound.java` beside one of three cluster members that `run.sh` starts with `cavalry run`, then `ClusterRound.java`, the cluster round, beside all three, through `run.sh` over `clients/jvm` | the runner's `javac` and `java`, whose `java.lang.foreign` binds the shared library when the client loads | ADR 0046 (card 208); ADR 0124 (work item 229) |
| `dotnet/` | C#, `Quickstart` and then `Readme`, the example of `clients/dotnet/README.md`, then `CommittedLog` beside one of three cluster members that `run.sh` starts with `cavalry run`, then `Cluster`, the cluster round, beside all three, through `run.sh` over `clients/dotnet` | the runner's `dotnet`, whose SDK builds the client and whose runtime binds the shared library when the client loads | ADR 0046 (card 209); ADR 0124 (work item 229) |

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

The cluster round is a runner's check of a cluster's service and of a session with a
cluster (`spec/c-abi.md`, versions 4 and 5; `spec/cluster.md`, "Sessions", "The service"
and "Replies"). Every runner has it: the C runner, in `c/cluster.c`, the Python
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
names the tree, so the directory is a cache, and N moves only when the page's version
does. N's build is kept on a host that keeps `target/`, as a laptop does. CI's cache
drops built programs: `Swatinem/rust-cache` keeps only `build/`, `.fingerprint/` and
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
and commit position. At the end it holds four things. Every number the cluster
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
beside the old, in the same change.

The script's header says how to run it by hand.
[Upgrade a cluster one member at a time](../docs/how-to/upgrade-a-cluster.md) is the
same upgrade for an operator.

