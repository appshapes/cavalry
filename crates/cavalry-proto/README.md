# cavalry-proto

Wire types, zero-copy frames and version negotiation. Data plane.

## What this does

- Defines every frame on the wire as a `#[repr(C)]` struct with a versioned header,
  using `zerocopy` or `bytemuck` derives, so a frame is read and written in place with
  no copy and no allocation.
- Parses and builds the frames that carry data fragments (the wire-sized pieces of a
  message larger than the MTU), NAKs (a receiver's report of a gap) and status messages
  (a receiver's position and window, which drive flow control), and negotiates the
  protocol version between peers so that mixed versions run during an upgrade.
- Is the one place bytes are shuffled by hand. Every other crate uses its types.

In Aeron, this is the transport protocol specification and its frame layouts
([glossary](../../docs/glossary.md)).

## What it must never do

- Implement behaviour that `spec/` does not specify. Unspecified behaviour is a bug in
  the spec, not a freedom in this crate. A wire change is a change to `spec/`, a
  conformance test and a version bump, in that order, before any code.
- Change a layout without changing its version. A layout change is a wire-protocol
  change.
- Trust an incoming frame. Frame parsers are fuzzed; a malformed frame is rejected with
  a counter, never a panic.
- Allocate, lock, make a syscall, await or panic per message (invariant 2).
- Invent a name. The vocabulary is the glossary's: publication, subscription, image,
  term, position, NAK, status message, stream id, session id.

## Example

Not yet. This crate has no public item to show, and a crate README's example is also
its doctest, so an example written now would invent an API.
[ADR 0003](../../docs/decisions/0003-lay-out-the-workspace.md) records why the example
rule is deferred for scaffold crates. The example arrives in the same commit as the
first public item.

## What to read next

Not yet: the pages this crate will point at, its section of `spec/` and its reference
page, do not exist. Until they do, read [`CLAUDE.md`](../../CLAUDE.md) for the
invariants and the gates, and
[ADR 0003](../../docs/decisions/0003-lay-out-the-workspace.md) for how the workspace is
laid out.
