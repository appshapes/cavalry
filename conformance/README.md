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
| `c/` | C11, `quickstart.c` through `run.sh` | the runner's `cc`, the static library | ADR 0051 (card 204) |
| `python/` | Python 3, `quickstart.py` and then `readme.py`, the example of `clients/python/README.md`, through `run.sh` over `clients/python` | the runner's `python3`, the shared library | ADR 0046 (card 205) |
| `node/` | Node, `quickstart.js` and then `readme.js`, the example of `clients/node/README.md`, through `run.sh` over `clients/node` | the runner's `node` and `npm`, whose own `node-gyp` builds the addon against the static library | ADR 0046 (card 206) |
| `dotnet/` | C#, `Quickstart` and then `Readme`, the example of `clients/dotnet/README.md`, through `run.sh` over `clients/dotnet` | the runner's `dotnet`, whose SDK builds the client and whose runtime binds the shared library when the client loads | ADR 0046 (card 209) |

The Python runner is the shape the Node and .NET runners copy and the Go and JVM
runners will (ADR 0046, cards 206 to 209): a package of another ABI version refused when it loads the library; the C
runner's checks through the client; the client's own edges, where a handler's exception
goes, what `offer` refuses, how long a view lives, and the calls `spec/c-abi.md` refuses
during a claim or a poll, which leave the client's state as they found it; then a report,
printed and never gated, from two rounds that check nothing else: the one-way p50, p99,
p99.99 and max over 100,000 messages, with the runtime's allocation counters off, and the
allocations per message on the client's side of the boundary over 10,000 more, counted
with what the runtime counts, switched on for that round alone (clients/README.md, "What
every client must pass").
