# Specifications

`spec/` holds the wire protocol, the shared-memory layouts, the counters and the
on-disk log format: the bytes and the behaviour that every implementation of Cavalry,
in any language, must agree on. Read it before `crates/`. Behaviour this directory
does not state is a bug in the spec, not a freedom in the code (`CLAUDE.md`,
Protocol).

## Conventions

- One page per layout, protocol or ABI, and each page names its version in its title.
  Multi-byte integers are little-endian. Offsets and lengths are in bytes. Signed
  fields are two's complement. An ABI page gives names, types, signatures, values and
  the rules a caller holds by convention, and its artefact is a committed header.
- Every layout carries a version in its header, and a reader rejects a version it does
  not know. Changing a field is a new version, a new ADR and new vectors, in that
  order, before any code. `just lint-docs` refuses a change under `spec/` without an
  Accepted ADR in the same commit.
- Names are Aeron's where the concept matches. `docs/glossary.md` maps each term.
- A page says what a reader rejects, in the order the checks run, so that every
  implementation reports the same reason for the same bytes.
- Test vectors live in `spec/vectors/<page>.json`, one file per page, written by the
  Python script beside it. The script is the page's reference encoder, independent of
  the Rust code, so the two must agree. Each vector is a name, the bytes as hex, and
  either the fields a conforming reader decodes or the error a conforming reader
  reports. A vector may carry `inputs` instead of bytes: an arithmetic vector, whose
  fields a conforming implementation computes from the inputs, for the position and
  claim arithmetic a layout defines. An ABI page's vectors are a third kind: the
  names, signatures, fields and values a conforming header declares, which a reader
  checks the committed header against. `just conformance` runs every implementation
  against every vector; `just test` runs the Rust one on every commit.
- The book's reference section includes these pages unchanged, from
  `docs/reference/`. Edit here, never there.

## Pages

| Page | Version | Decided in |
| --- | --- | --- |
| `frame.md`, the data frame header | 0 | ADR 0008 |
| `counters-file.md`, the counters file | 0 | ADR 0012, ADR 0062 |
| `log-buffer.md`, the log buffer | 1 | ADR 0013, ADR 0040, ADR 0048, ADR 0066 |
| `command-ring.md`, the command ring | 1 | ADR 0014, ADR 0018 |
| `broadcast-buffer.md`, the broadcast buffer | 0 | ADR 0014 |
| `client-protocol.md`, the client protocol | 8 | ADR 0019, ADR 0025, ADR 0030, ADR 0031, ADR 0040, ADR 0049, ADR 0053, ADR 0059, ADR 0062, ADR 0064, ADR 0065, ADR 0068, ADR 0069, ADR 0072 |
| `durable-log.md`, the durable log | 2 | ADR 0024, ADR 0031, ADR 0060, ADR 0064, ADR 0065 |
| `udp.md`, the UDP transport | 2 | ADR 0040, ADR 0045, ADR 0053, ADR 0059, ADR 0068 |
| `c-abi.md`, the C ABI | 3 | ADR 0051, ADR 0057, ADR 0070, ADR 0072, ADR 0080 |
| `cluster.md`, the cluster protocol | 0 | ADR 0058, ADR 0063, ADR 0066, ADR 0067, ADR 0069 |
