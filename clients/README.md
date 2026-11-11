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
command that fixes it. The generator writes Python (`python/`, by `write_python` in
`generate.py`), Node (`node/`, by `generate_node.py` beside it, which `generate.py`
imports), Go (`go/`, by `generate_go.py`), the JVM (`jvm/`, by `generate_jvm.py`) and
.NET (`dotnet/`, by `generate_dotnet.py`).

## What every client must pass

- `just conformance` runs the client's runner under `conformance/<language>/`: the
  ten-minute path's shape through the client, publish and subscribe over `cavalry:ipc`
  and `cavalry:udp`, record and replay, every message once and in order, the
  fragment handler's header fields against `spec/frame.md`'s vectors, a package of
  another ABI version refused when it loads the library, the calls `spec/c-abi.md`
  refuses during a claim or a poll leaving the client's state as they found it, and a
  report,
  printed and never gated, from two rounds that check nothing else. The one-way p50,
  p99, p99.99 and max, over 100,000 messages so that ten samples lie above the p99.99,
  with the runtime's allocation counters off. The allocations per message on the
  client's side of the boundary, the client's code and the runtime's together,
  counted with what the runtime counts, switched on for that round alone and read
  with nothing else between the two reads; the README says which objects a message
  makes and frees that such a net count cannot see. Where the runtime frees what a
  message makes natively later than a collection, as Node does when its event loop
  turns, the runner counts what is held, shows that the loop the client's README
  gives keeps it bounded, and fails when it does not (invariant 3).
- `conformance/<language>/run.sh` runs the client README's example as written against
  the runner's driver, and fails unless it prints the line the README says the reader
  will see (`conformance/python/readme.py` is the shape).
- `just lint` runs `clients/lint.py` over the generated per-message functions, the
  wrapper of each of the ABI's nine (`crates/cavalry-ffi/README.md`: the claim, the
  reserved value, the commit and abort, the offer, the poll and the header's values,
  and the committed log's poll and an entry's position; ADR 0070), and each
  trampoline that calls the user's handler: no lock, no `await`, no raise, in
  the language's forms, in the function's own body or in any function of the package
  it calls. Generated code of the client in another language on the same path is read
  in that language's forms, as the Node addon's C is. It fails on generated code in a
  language it has no rules for, on a package in which it finds none of the
  per-message functions, and on one that is missing.
- The client's README answers the four questions every crate README answers, with the
  ten-minute path's program in its language as the example, and records the
  runtime's side of the boundary: how the runtime calls the fragment handler back,
  what that costs in locks, allocations and system calls per message and when the
  runtime frees what it allocated, whether the view over a
  claimed slice is one object per publication or a new one per claim, and where an
  exception the user's handler raises goes.

## Released together

The clients version with `cavalry-ffi` and are released with it, at the version the
ABI freezes at (ADR 0046's Revisit; the release's ADR). A client's package carries
the header's `CAVALRY_ABI_VERSION`. When it loads the library it calls
`cavalry_abi_version()` before any other function and refuses a library of another
version with an error that names both numbers and says what to do: load the library
built from the same checkout, or regenerate the package from the library's header
(`spec/c-abi.md`, "Versions and what a reader rejects"). It also passes its version to
`cavalry_context_init_v`, which refuses a mismatch again at each client it makes.

| Client | Package | Runner | Decided in |
| --- | --- | --- | --- |
| `python/` | `cavalry`, `ctypes` over the shared library | `conformance/python/` | ADR 0046 (card 205) |
| `node/` | `@cavalry/client`, an N-API addon in C over the header, linked with the static library | `conformance/node/` | ADR 0046 (card 206) |
| `go/` | `github.com/appshapes/cavalry/clients/go`, cgo over the header, linked with the static library | `conformance/go/` | ADR 0046 (card 207) |
| `jvm/` | `com.appshapes.cavalry`, `java.lang.foreign` over the shared library, Java 25 and nothing else | `conformance/jvm/` | ADR 0046 (card 208) |
| `dotnet/` | `Cavalry`, P/Invoke with the source-generated marshalling over the shared library, .NET 10 and nothing else | `conformance/dotnet/` | ADR 0046 (card 209) |
