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
generator writes every file of the package; nothing in a package is edited by hand,
and `just lint` regenerates each package to a temporary directory and fails when the
committed files differ. Today the generator writes Python (`python/`); the Node, Go,
JVM and .NET clients (cards 206 to 209) add a language each.

## What every client must pass

- `just conformance` runs the client's runner under `conformance/<language>/`: the
  ten-minute path's shape through the client, publish and subscribe over `cavalry:ipc`
  and `cavalry:udp`, record and replay, every message once and in order, the
  fragment handler's header fields against `spec/frame.md`'s vectors, and a report,
  printed and never gated: the one-way p50, p99.99 and max, and the allocations per
  message on the client's side of the boundary, the client's code and the runtime's
  together, counted with what the runtime counts.
- `just lint` runs `clients/lint.py` over the generated per-message functions, the
  offer, the claim, the commit and abort, and the poll: no lock, no `await`, no raise,
  in the language's forms, and fails on generated code in a language it has no rules
  for, or on a package in which it finds none of the per-message functions.
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
