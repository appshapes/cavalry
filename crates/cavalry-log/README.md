# cavalry-log

The durable log: recording and replay. Data plane.

## What this does

- Records a stream to disk as an ordered, replayable log, written by the driver.
- Holds the log directory for the driver: `dir::LogDir` opens it under an exclusive
  advisory lock on the catalog, so two drivers never write one directory, and runs the
  recovery pass of the spec before any recording is added, reporting every action for
  the driver to count (ADR 0025).
- Writes a recording for the driver: `LogDir::start` takes the catalog slot and creates
  the time index and the first segment file; `recorder::RecordingWriter` writes each
  block of committed frames with one positional write, appends an index entry at most
  once per millisecond, rolls segments at their length, and syncs once per second
  behind two positions, what is written and what is synced; `LogDir::stop` writes the
  stop (ADR 0025). A roll that fails on a full disk is an `io` error and is retried at
  the next write, never a stuck writer. On any error from `write`, the block was taken
  exactly when `position()` moved. The delete of a stopped recording, state 3, arrives
  with card 95's purge, with its counter and its caller in the driver; until then the
  catalog writer has no `delete`, so every transition it writes is one the driver counts.
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

The driver's path, in one process: open the log directory, record one block of a
stream, sync it, stop, and read the recording back by position
(`spec/durable-log.md`). The clock is the caller's, nanoseconds since the Unix epoch.

```rust
use cavalry_log::catalog::CatalogReader;
use cavalry_log::dir::{LogDir, Recovery};
use cavalry_log::layout::{Recording, State, locate};
use cavalry_proto::frame::{DataFrameHeader, Flags, FrameType};

let dir = std::env::temp_dir().join(format!("cavalry-log-{}", std::process::id()));
let now = 1_760_000_000_000_000_000;
let (mut log, recovery) = LogDir::open(&dir, now)?;
assert_eq!(recovery, Recovery::default(), "a new directory has nothing to repair");
let mut writer = log.start(&Recording {
    recording_id: 0, start_timestamp: now, start_position: 0, initial_term_id: 7,
    term_length: 64 * 1024 * 1024, mtu_length: 1408, session_id: 1, stream_id: 1001,
    channel: "cavalry:ipc", source_identity: "cavalry:ipc",
}, now)?;
// One block as the recorder's scan hands it: a committed frame, header and body.
let mut block = [0u8; 64];
let header = DataFrameHeader::new(FrameType::Data, Flags::UNFRAGMENTED, 64)?
    .with_term_id(7).with_session_id(1).with_stream_id(1001);
block[..32].copy_from_slice(&header.to_bytes());
writer.write(&block, block.len(), now)?;
assert!(writer.sync(now + 1_000_000_000)?, "a second passed: the block is durable");
assert_eq!(writer.synced_position(), 64);
let stopped = log.stop(writer, now + 2_000_000_000)?;
assert_eq!((stopped.state(), stopped.stop_position()), (State::Valid, 64));
let located = locate(&stopped, 0)?;
println!("recording {} holds position 0 in {} at offset {}",
    stopped.recording_id(), located.segment_file, located.offset_in_segment);
let reader = CatalogReader::open(&dir)?;
assert_eq!(reader.get(0)?.expect("the slot holds the recording").stop_position(), 64);
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
