# cavalry-shm

Memory-mapped log buffers, ring buffers and the counters file. Data plane, and one of
the two crates where `unsafe` is permitted.

## What this does

- Maps the log buffers that clients and the driver share: a stream's log is a ring of
  fixed-size terms, and a position is a byte offset into it that increases across terms.
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

The driver creates the counters file and allocates a counter; a reader opens the file
and reads the counter back; the file fills up and says so. This is the crate's doctest.

```rust
use cavalry_shm::counters::generated::{COUNTERS, COUNTERS_ALLOCATED, COUNTERS_ALLOCATION_FAILED};
use cavalry_shm::counters::{CounterId, CountersError, CountersFile, CountersReader, Scope};

fn main() -> Result<(), CountersError> {
    // The driver's directory. The file inside it is named `counters`.
    let dir = std::env::temp_dir().join(format!("cavalry-shm-readme-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;

    // The driver creates the file once, with room for 64 counters. Every static counter
    // in counters.toml is allocated with it, in the record whose index is its id.
    let mut driver = CountersFile::create(&dir, 64)?;
    let statics = COUNTERS.iter().filter(|def| def.scope == Scope::Static).count() as i64;

    // An instance counter: one per publication, image or whatever it counts. The type id
    // is the counters.toml id of the entry a later feature adds; the key and the label
    // say which instance. Moving it is one relaxed add, the data plane's only operation.
    let gaps = driver.allocate(CounterId(7), &1001i32.to_le_bytes(), "image.gaps 1001")?;
    gaps.increment();

    // Everyone else opens the file read-only. A reader checks the header before it trusts
    // anything, and a record's state before its value.
    let reader = CountersReader::open(&dir)?;
    assert_eq!(reader.read(COUNTERS_ALLOCATED.record())?, statics + 1); // the statics and `gaps`
    assert_eq!(reader.read(gaps.id())?, 1);
    assert_eq!(reader.record(gaps.id())?.map(|r| r.label().to_owned()), Some("image.gaps 1001".into()));

    // The capacity is fixed at creation. When every record is in use, an allocation fails
    // with `full`, counters.allocation_failed moves, and whatever needed it is refused.
    for n in (statics + 1)..64 {
        driver.allocate(CounterId(7), &[], &format!("image.gaps {n}"))?;
    }
    let full = driver.allocate(CounterId(7), &[], "image.gaps 64").unwrap_err();
    assert_eq!(full.reason(), "full");
    assert_eq!(reader.read(COUNTERS_ALLOCATION_FAILED.record())?, 1);
    println!("{full}");

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}
```

Running it prints one line, the refusal with its reason, what to do and the docs
anchor:

```text
counter refused (full): every one of 64 records is in use; restart the driver with a larger counters file capacity, new publications and subscriptions are refused until then; see docs/reference/counters-file.md#when-the-file-is-full
```

## What to read next

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
