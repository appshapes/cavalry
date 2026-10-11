# 0009. Read and write wire structs with zerocopy

- **Status:** Accepted
- **Date:** 2026-10-11
- **Invariants affected:** 1, 2
- **Supersedes:** none

## Context

`cavalry-proto` turns the layout tables in `spec/` into Rust types. The data frame
header (ADR 0008) is the first. The crate is on the data plane and denies `unsafe`
(ADR 0003), a header is read in place from a mapped term or a packet buffer with no
copy and no allocation (invariant 2), and the bytes must match the table exactly
(invariant 1). `.claude/rules/data-plane.md` already says wire structs use `zerocopy`
or `bytemuck` derives, and a dependency in a data-plane crate needs an ADR, an
allowlist entry and a justification (ADR 0001, ADR 0003). This is the crate's first
dependency.

## Decision

`cavalry-proto` depends on `zerocopy` 0.8, with `default-features = false` and the
`derive` feature, listed in its `allowed-dependencies`. A wire struct is `#[repr(C)]`
and derives `FromBytes`, `IntoBytes`, `KnownLayout`, `Immutable` and `Unaligned`, with
`zerocopy`'s little-endian integer types as its fields, so that the struct is the
layout table, little-endian is part of each field's type, a reference to a header is
taken from any byte offset without a copy, and the derive refuses at compile time a
struct with padding or a field that could hold an invalid value. The crate stays
`unsafe_code = "deny"`: the `unsafe` that makes this sound is inside `zerocopy`,
whose unsafe code is reviewed under its own documented policy. The `cargo` ecosystem
joins `.github/dependabot.yml` with this dependency, as ADR 0003 said it would.

## Alternatives considered

- **Field-by-field `from_le_bytes` and `to_le_bytes`, no dependency.** Lost
  narrowly: it is safe Rust and a 32-byte copy costs nothing measurable, but the
  layout would then exist twice, once in the spec table and once in the parser's
  offsets, with only the vectors to keep them equal; the subscriber reads a header
  in place in a mapped term, and every later layout, the log buffer metadata above
  all, needs the in-place view anyway.
- **`bytemuck`.** Lost: `Pod` requires the host's alignment, so a view at an
  arbitrary byte offset is a copy (`pod_read_unaligned`), and it has no byte-order
  types, so little-endian would be a convention in the code rather than a type.
- **A generated codec, such as SBE.** Lost for the frame header: code generation and
  a schema language for one fixed layout. Revisited if the control plane's messages
  grow a schema.
- **`zerocopy` with default features.** Lost: the defaults are not needed, and a
  data-plane crate takes the smallest surface that works.

## Evidence

- `zerocopy` 0.8.62 was published on 2026-10-08, the newest of the 0.8 line; its
  `rust-version` is 1.56, below the workspace's 1.85; its licence is
  `BSD-2-Clause OR Apache-2.0 OR MIT`. The 0.9 line is at an alpha and is not
  taken.
- Nothing measured: `just bench` does not exist (ADR 0007), and the header is 32
  bytes either way.

## Consequences

Easier: a layout is one struct, checked by the compiler against the derives and by
the vectors against the spec; later frames and the log buffer's metadata reuse the
pattern; `cavalry-ffi` can hand the same structs across the C ABI.

Harder: a proc-macro dependency on the build; a 0.x crate whose minor releases
break, moved by Dependabot one pull request at a time and reviewed by the agent;
`zerocopy`'s integer types appear in `cavalry-proto`'s public API, so a bump of its
major version is a breaking change of ours.

Revisit: at `zerocopy` 1.0; if a derive cannot express a layout `spec/` needs.

## References

- ADR 0001, ADR 0003, ADR 0008; `.claude/rules/data-plane.md`
- `crates/cavalry-proto/Cargo.toml`, `.github/dependabot.yml`
- zerocopy: <https://crates.io/crates/zerocopy>, <https://github.com/google/zerocopy>
- AppShapes Trello card 80, "Adopt Aeron's data frame header"
