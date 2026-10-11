# Specifications

`spec/` holds the wire protocol, the shared-memory layouts, the counters and the
on-disk log format: the bytes and the behaviour that every implementation of Cavalry,
in any language, must agree on. Read it before `crates/`. Behaviour this directory
does not state is a bug in the spec, not a freedom in the code (`CLAUDE.md`,
Protocol).

## Conventions

- One page per layout or protocol, and each page names its version in its title.
  Multi-byte integers are little-endian. Offsets and lengths are in bytes. Signed
  fields are two's complement.
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
  reports. `just conformance` runs every implementation against every vector;
  `just test` runs the Rust one on every commit.
- The book's reference section includes these pages unchanged, from
  `docs/reference/`. Edit here, never there.

## Pages

| Page | Version | Decided in |
| --- | --- | --- |
| `frame.md`, the data frame header | 0 | ADR 0008 |
| `counters-file.md`, the counters file | 0 | ADR 0012 |
