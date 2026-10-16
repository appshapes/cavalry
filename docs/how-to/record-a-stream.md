# Record a stream to the durable log

A how-to page: one task, record a stream's live publication to disk while a driver
runs, watch the recording's progress in `doctor`, and stop it. What the files hold is
[`spec/durable-log.md`](../reference/durable-log.md); what the commands mean is
[`spec/client-protocol.md`](../reference/client-protocol.md), "Commands". In Aeron, this
is Archive's `startRecording` ([glossary](../glossary.md)).

## Prerequisites

- A driver running on this host ([Run a driver and read doctor](run-the-driver.md)).
  Its log directory, where recordings go, is `~/.local/share/cavalry/log` by default,
  `$XDG_DATA_HOME/cavalry/log` when that variable is set, and `/var/lib/cavalry/log`
  when the driver runs as root; `--log-dir <path>` on `run`, or `CAVALRY_LOG_DIR`,
  chooses another, on a disk with room. One driver holds a log directory at a time; a
  second refuses to start with `log directory locked`. `cavalry doctor --log-dir <path>`
  warns when the directory is in memory, on a network or overlay filesystem, not
  writable by the driver's user, or under 1 GiB free (`log-directory-backing`).
- A live publication on the stream. The tutorial's program publishes on stream 1001
  ([Publish and subscribe in ten minutes](../tutorials/publish-and-subscribe.md)); a
  program of your own adds one with `Cavalry::add_publication`. A publication closes
  with its client, and a recording follows its publication to the end of stream.

## Steps

1. Start the recording, naming the stream:

   ```sh
   cargo run --release -p cavalry -- record 1001
   ```

   You will see `recording 0 started: stream 1001 session <session> on cavalry:ipc,
   position <p>, synced <q>; cavalry record --stop 0 stops it, and doctor lists it`, and
   the command exits: the recording is the driver's, not the command's. The recording id
   is the slot in the catalog, never reused. When several publications are live on the
   stream, one concurrent and some exclusive, each `record` records the next one, the
   lowest registration id first; when every one is recorded the command is refused with
   error code 22, naming the recording.

2. Watch it:

   ```sh
   just doctor
   ```

   The report has a `note recording` line, `recording 0 of stream 1001 session
   <session>: position <p>, synced <q>`: the position is what the operating system
   holds, within a millisecond of the publisher; the synced position is what a sync has
   made durable, within a second. The log directory holds `catalog`, `0-0.rec`, the
   first segment file of 128 MiB (sparse until written), and `0.idx`, the time index.
   The publisher's limit respects the recording's position, so a disk slower than the
   stream back-pressures the publisher and never loses a byte; `doctor` says
   `recording-stalled` when the recorder is a full window behind.

3. Stop it:

   ```sh
   cargo run --release -p cavalry -- record --stop 0
   ```

   You will see `recording 0 stopped`. The catalog holds the stop position, every
   client hears `recording stopped`, and `doctor` no longer lists the recording. A
   recording also stops by itself when its publication closes, once the recorder has
   written up to the end of stream.

From a program, the same three steps are `client.start_recording("cavalry:ipc", 1001)?`,
which returns a `Recording` with `recording_id()`, `position()` and `synced_position()`
read from the counters file, and `client.stop_recording(id)?`, from any client. Dropping
the `Recording` stops nothing.

## How to verify it worked

- `ls -l ~/.local/share/cavalry/log` lists `catalog`, `0-0.rec` and `0.idx`; the first
  bytes of `0-0.rec` are the stream's frames as the publisher wrote them, header and
  body, so a position in the recording is a position in the stream.
- `just doctor` after the stop lists no recording and reports no `recording-*` finding.
- `cavalry record --stop 0` a second time prints `recording 0 stopped` again: a stop of
  a stopped recording is success with nothing written.

## When it does not work

- `error 20 (no publication to record)`: nothing live publishes on that stream and
  channel; start the publisher first. A publication is gone once its client is.
- `error 22 (already recording)`: every live publication on the stream is recorded; the
  message names the recording. Stop one with `--stop`, or let it be.
- `error 21 (unknown recording)`: the id is not in the catalog; `doctor` lists the live
  recordings by id.
- `error 10 (resource temporarily unavailable)`: the recordings registry (1024), the
  catalog (1048576 slots over the directory's life), the counters file or the recorder's
  queue is full; stop recordings that are not needed, or start the driver on a new log
  directory.
- `error 12 (storage space)`: the time index or the first segment file could not be
  created; `doctor --log-dir` says what the directory is short of. The catalog slot is
  taken and stopped at its start.
- `recording-write-failed` in `doctor`: a write failed and the recording stopped at the
  position written; what was written is whole and replayable. Free space, or move the
  log directory, and start the recording again.
- `driver failed (log-dir): log directory locked`: another driver holds the directory;
  stop it, or give this one its own with `--log-dir`.
