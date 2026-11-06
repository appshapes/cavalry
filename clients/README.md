# clients

The language clients over the C ABI (`spec/c-abi.md`; ADR 0046, ADR 0051): thin
wrappers, each generated from the committed header
`crates/cavalry-ffi/include/cavalry.h`, each passing one conformance runner on both CI
runners, all released together. Everything a message does happens in `cavalry-ffi` and
the Rust behind it; a client here turns the ABI's handles into the forms its language's
users expect and nothing more.

## How a client is generated

`python3 clients/generate.py <language>` reads the header, the eight opaque handle
types, the nine structs with their fields, the fragment handler's type, the 58
functions with their return and parameter types and the 71 defines, and writes the
client's package: the binding, which declares every function and struct to the
language's foreign-function layer, and the client's own code, the wrapper types that
hold invariant 2's per-message rules between the user's code and the ABI. The
generator writes every file of the package; nothing in a package is edited by hand.
`just generate-clients` writes every package; commit what it writes. `just lint`
regenerates each package in memory and fails when a committed file differs, naming the
command that fixes it. Today the generator writes Python (`python/`); the Node, Go,
JVM and .NET clients (cards 206 to 209) add a language each.

## What every client must pass

- `just conformance` runs the client's runner under `conformance/<language>/`: the
  ten-minute path's shape through the client, publish and subscribe over `cavalry:ipc`
  and `cavalry:udp`, record and replay, every message once and in order, the
  fragment handler's header fields against `spec/frame.md`'s vectors, and a report,
  printed and never gated, from two rounds that check nothing else. The one-way p50,
  p99, p99.99 and max, over 100,000 messages so that ten samples lie above the p99.99,
  with the runtime's allocation counters off. The allocations per message on the
  client's side of the boundary, the client's code and the runtime's together,
  counted with what the runtime counts, switched on for that round alone and read
  with nothing else between the two reads; the README says which objects a message
  makes and frees that such a net count cannot see.
- `conformance/<language>/run.sh` runs the client README's example as written against
  the runner's driver, and fails unless it prints the line the README says the reader
  will see (`conformance/python/readme.py` is the shape).
- `just lint` runs `clients/lint.py` over the generated per-message functions, the
  wrapper of each of the ABI's seven (`crates/cavalry-ffi/README.md`: the claim, the
  reserved value, the commit and abort, the offer, the poll and the header's values)
  and the trampoline that calls the user's handler: no lock, no `await`, no raise, in
  the language's forms, in the function's own body or in any function of the package
  it calls. It fails on generated code in a language it has no rules for, on a package
  in which it finds none of the per-message functions, and on one that is missing.
- The client's README answers the four questions every crate README answers, with the
  ten-minute path's program in its language as the example, and records the
  runtime's side of the boundary: how the runtime calls the fragment handler back,
  what that costs in locks and allocations per message, whether the view over a
  claimed slice is one object per publication or a new one per claim, and where an
  exception the user's handler raises goes.

## Released together

The clients version with `cavalry-ffi` and are released with it, at the version the
ABI freezes at (ADR 0046's Revisit; the release's ADR). A client's package carries
the header's `CAVALRY_ABI_VERSION` and passes it to `cavalry_context_init_v`, so a
package built against another version is refused at its first call.

| Client | Package | Runner | Decided in |
| --- | --- | --- | --- |
| `python/` | `cavalry`, `ctypes` over the shared library | `conformance/python/` | ADR 0046 (card 205) |
