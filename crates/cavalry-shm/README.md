# cavalry-shm

Memory-mapped log buffers, ring buffers and the counters file. Data plane, and one of
the two crates where `unsafe` is permitted.

## What this does

- Maps the log buffers that clients and the driver share: a stream's log is three
  fixed-size terms and a metadata page, and a position is a byte offset into the stream
  that increases across terms. `LogBuffers` creates and opens one, a `TermAppender`
  claims a frame in place and commits it with one release store, a `TermReader` reads
  committed frames where its position says, and the driver's duties, the limit, cleaning,
  unblocking and end of stream, are methods on the log
  ([`spec/log-buffer.md`](../../spec/log-buffer.md)).
- Provides the lock-free SPSC and MPSC queues every data-plane thread uses to talk to
  another, with 64-byte alignment on shared state so that two threads never share a
  cache line by accident, and a 128-byte slot of its own for every field one party
  writes and another reads in its hot loop, such as a counter's value
  ([ADR 0012](../../docs/decisions/0012-lay-out-the-counters-file.md)).
- Owns the counters file, the shared-memory file where the driver publishes every
  counter. It is the primary instrument: cheaper than a log line, and it survives a
  crash. `CountersFile` creates it and allocates counters, a `Counter` moves a value
  with one relaxed add, and `CountersReader` reads it after the checks of
  [`spec/counters-file.md`](../../spec/counters-file.md).
- Keeps every shared layout `#[repr(C)]` with a version in its header. Atomics use
  `Acquire` for loads that gate reads of other memory and `Release` for stores that
  publish it.

In Aeron, this is the log buffer layout and the CnC file, which Agrona provides
([glossary](../../docs/glossary.md)).

## What it must never do

- Carry an `unsafe` block without a `// SAFETY:` comment naming the invariant and where
  it is established. New `unsafe` needs a `loom` test in the same commit, and the crate
  passes `loom` and Miri as a release gate.
- Change a shared layout without bumping its version. Another process may be mapping the
  old one.
- Grow. Every buffer, queue and map has a capacity fixed at startup and a documented
  behaviour when full (invariant 3). Back-pressure is returned to the caller, never
  absorbed.
- Allocate, lock, make a syscall, await or panic per message (invariant 2).
- Trust a field another process wrote. A reader checks every field against the bound
  the spec names before using it, and a check that fails is an error with a name,
  never "no data".

## Example

The driver creates a log, a publisher claims a frame in place and commits it, a
subscriber reads it back, and a claim at the limit is refused by name. This is the
crate's doctest.

```rust
use cavalry_shm::log::{HeaderFields, LogBuffers, LogType};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The driver's directory. A log lives at publications/<correlation id>.logbuffer.
    let dir = std::env::temp_dir().join(format!("cavalry-shm-readme-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;

    // The driver creates the log of publication 7, session 1 on stream 1001: 64 KiB terms,
    // frames of at most 1408 bytes, a window of half a term; then a subscriber connects.
    let fields = HeaderFields::new(LogType::Exclusive, 7, 0, 65_536, 1408, 32_768, 1, 1001)?;
    let log = LogBuffers::create(&dir, &fields)?;
    log.set_connected(true)?;

    // The publisher claims a frame in place, writes the body and commits with one release
    // store, while its position is below the limit the driver maintains in a counter.
    let publisher = LogBuffers::open(&dir, 7, true)?;
    let mut appender = publisher.appender()?;
    let limit = i64::from(fields.publication_window_length());
    let mut claim = appender.try_claim(8, limit)?;
    claim.body().copy_from_slice(b"cavalry!");
    assert_eq!(claim.commit(), 64); // a 40-byte frame, aligned to 32

    // The subscriber maps the log read-only and reads the frame where its position says,
    // checked as spec/frame.md says before the body is handed over.
    let subscriber = LogBuffers::open(&dir, 7, false)?;
    let mut reader = subscriber.reader(0)?;
    let poll = reader.poll(16, |body, header| {
        assert_eq!(body, b"cavalry!");
        assert_eq!(header.stream_id(), 1001);
    })?;
    assert_eq!((poll.fragments, poll.position), (1, 64));

    // At the limit the claim is refused by name and nothing is written; the driver raises
    // the limit as the slowest subscriber advances.
    let refused = appender.try_claim(8, 64).unwrap_err();
    println!("{refused}");

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}
```

Running it prints one line, the refusal with its reason, what to do and the docs
anchor:

```text
claim refused (back-pressured): the slowest subscriber is a window behind; try again later, and read subscription.position to find it; see docs/reference/log-buffer.md#claim-commit-and-abort
```

The counters file has its own example, the doctest of the `counters` module.

## What to read next

- [`spec/log-buffer.md`](../../spec/log-buffer.md): the layout, who writes and who reads
  every field, the claim, commit, abort and rotation protocols, the reader, the driver's
  duties and the checks a reader runs, in order. The book shows the same page as
  [`docs/reference/log-buffer.md`](../../docs/reference/log-buffer.md).
  [ADR 0013](../../docs/decisions/0013-lay-out-the-log-buffer.md) is why the layout is
  Aeron's and why positions are counters.
  [`spec/vectors/log-buffer.json`](../../spec/vectors/log-buffer.json) runs in
  `tests/log_buffer_conformance.rs` on every `just test`; `tests/log_buffer.rs` is the
  file on disk, the full term first.
- [`spec/counters-file.md`](../../spec/counters-file.md): the layout, who writes and
  who reads every field, the creation and allocation protocols, and the checks a reader
  runs, in order. The book shows the same page as
  [`docs/reference/counters-file.md`](../../docs/reference/counters-file.md).
- [`docs/reference/counters.md`](../../docs/reference/counters.md): every counter, what
  a change means and what to do, generated from [`counters.toml`](../../counters.toml)
  by `scripts/gen-counters.py`. `just lint-counters` fails when it is stale.
- [ADR 0012](../../docs/decisions/0012-lay-out-the-counters-file.md), why the layout is
  Aeron's records behind a Cavalry header and why `memmap2` maps it;
  [ADR 0010](../../docs/decisions/0010-validate-peer-written-shared-memory.md), why
  every peer-written field is checked against a bound; and
  [ADR 0011](../../docs/decisions/0011-pad-peer-shared-fields-to-128-bytes.md), why a
  value slot is 128 bytes.
- [`spec/vectors/counters-file.json`](../../spec/vectors/counters-file.json), which
  `tests/conformance.rs` runs on every `just test`; `tests/counters_file.rs`, the file
  on disk, the full case first; `just loom` for the publish protocols and `just miri`
  for the layout layer.
