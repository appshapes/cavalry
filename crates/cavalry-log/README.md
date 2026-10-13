# cavalry-log

The durable log: recording and replay. Data plane.

## What this does

- Records a stream to disk as an ordered, replayable log, written by the driver.
- Replays a log back into a subscription from a given position, at original or maximum
  speed. `cavalry replay` answers "what did this service see between two instants" in
  one command.
- Carries the headline promise: replaying the same log produces the same state, byte for
  byte. `just replay-check` replays a recorded log twice and diffs the state.

In Aeron, this is Archive, with its recordings and replay
([glossary](../../docs/glossary.md)).

## What it must never do

- Produce a different state from the same log (invariant 1). A bug here blocks a
  release.
- Change the on-disk format without an accepted ADR and a version in the file header. A
  recorded log outlives the binary that wrote it.
- Lose or reorder a recorded message, or report a position it has not made durable.
- Grow without a bound. Every buffer on the recording path has a fixed capacity and a
  documented behaviour when full (invariant 3); retention is explicit, never silent
  growth.
- Allocate, lock, make a syscall, await or panic per message (invariant 2).

## Example

A catalog in a directory of its own: the driver's writer adds a recording, stops it, and
any process reads it back (`spec/durable-log.md`).

```rust
use cavalry_log::catalog::{CatalogReader, CatalogWriter};
use cavalry_log::layout::{Recording, State};

let dir = std::env::temp_dir().join(format!("cavalry-log-{}", std::process::id()));
std::fs::create_dir_all(&dir)?;
let mut writer = CatalogWriter::open(&dir, 1_760_000_000_000_000_000)?;
let id = writer.add(&Recording {
    recording_id: 0,
    start_timestamp: 1_760_000_000_000_000_000,
    start_position: 0,
    initial_term_id: 7,
    term_length: 64 * 1024 * 1024,
    mtu_length: 1408,
    session_id: 1,
    stream_id: 1001,
    channel: "cavalry:ipc",
    source_identity: "cavalry:ipc",
})?;
writer.stop(id, 4096, 1_760_000_001_000_000_000)?;
let reader = CatalogReader::open(&dir)?;
let recording = reader.get(id)?.expect("the slot holds the recording");
assert_eq!(recording.state(), State::Valid);
assert_eq!(recording.stop_position(), 4096);
println!("recording {id} on stream {} stopped at {}", recording.stream_id(), recording.stop_position());
std::fs::remove_dir_all(&dir)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## What to read next

- [`spec/durable-log.md`](../../spec/durable-log.md): the catalog, the segment files and
  the time index, byte by byte, and what a reader rejects.
- [ADR 0024](../../docs/decisions/0024-lay-out-the-durable-log.md): why the layout is
  what it is, and [ADR 0023](../../docs/decisions/0023-build-the-durable-log-second.md)
  for the slice this crate is built in.
- [`spec/log-buffer.md`](../../spec/log-buffer.md), "Positions": what a position is.
