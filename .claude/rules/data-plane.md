---
paths:
  - "crates/cavalry-proto/**"
  - "crates/cavalry-shm/**"
  - "crates/cavalry-driver/**"
  - "crates/cavalry-client/**"
  - "crates/cavalry-log/**"
  - "crates/cavalry-raft/**"
  - "crates/cavalry-ffi/**"
---

# Data-plane checklist

You are editing code a message passes through. Before committing, confirm each line
below and say so in the commit message.

## Per-message hot path

- No heap allocation. `Vec::push`, `format!`, `Box::new`, `String`, `collect()` and
  `to_vec()` are all allocations. Pre-size at startup; reuse buffers.
- No lock. No `Mutex`, `RwLock`, `parking_lot`, or channel with a mutex inside.
  Use the SPSC/MPSC queues in `cavalry-shm`.
- No syscall. Sockets are drained and filled in batches by the sender and receiver
  threads; a message never triggers its own `sendmsg`. Clock reads go through the
  cached clock, not `Instant::now()` per message.
- No `await`, no `async fn`, no executor. The workspace lint denies it here; do not
  `#[allow]` it.
- No panic path. No `unwrap()`, `expect()`, indexing without a bounds proof, or
  arithmetic that can overflow in release. Return an error code and bump a counter.
- No `tracing`, `log`, `println!` or `eprintln!`. Bump a counter. If you need a
  message for a human, that is a `doctor` check, which reads counters.

## Memory and layout

- Anything shared between threads or processes is `#[repr(C)]` and padded or aligned
  to 64 bytes. Put a comment on each field group that shares a cache line.
- Atomics use `Acquire` for loads that gate reads of other memory, `Release` for
  stores that publish it, `Relaxed` only for counters nobody branches on. `SeqCst`
  needs a comment explaining why the weaker orderings are insufficient.
- Every shared layout has a version in its header. Changing a field changes the
  version, the spec, and a conformance test.
- Use `zerocopy` / `bytemuck` derives for wire structs. No manual byte shuffling
  outside `cavalry-proto`.

## Bounds

- Every buffer, queue, map and retry loop has a capacity set from config at startup
  and a stated behaviour when full (back-pressure code, drop with counter, or NAK).
  Write the full-case test before the happy-path test.
- Term buffer sizes, MTU, and shm size are validated against each other at startup
  with an error that says which value to change.

## Unsafe (cavalry-shm and cavalry-ffi only)

- `// SAFETY:` on every block, naming the invariant and where it is established.
- `cavalry-shm`: a `loom` test for every new interleaving the change introduces, and
  `just miri` on the crate before committing. Miri cannot see across processes, so
  describe the cross-process invariant in the commit message as well.
- `cavalry-ffi`: buffer lifetimes across the C ABI are by convention, not the borrow
  checker, so debug builds poison released buffers (CLAUDE.md, Unsafe).

## Evidence in the commit message

1. `just bench-compare` output: p50, p99, p99.99, max, before and after.
2. `just test -p <crate>` and `just conformance` green.
3. For `cavalry-shm`: `loom` and Miri output.
4. One sentence on which invariant in `CLAUDE.md` this change serves. If the answer
   is "none", reconsider the change.
