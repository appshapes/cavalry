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
| `python/` | Python 3, `quickstart.py` through `run.sh` over `clients/python` | the runner's `python3`, the shared library | ADR 0046 (card 205) |

The Python runner is the shape the Node, Go, JVM and .NET runners copy (ADR 0046, cards
206 to 209): the C runner's checks through the client, then a report, printed and never
gated, of the one-way p50, p99.99 and max and the allocations per message on the client's
side of the boundary, counted with what the runtime counts.
