# Record a stream to the durable log

A how-to page: one task, record a stream's live publication to disk while a driver
runs, watch the recording's progress in `doctor`, stop it, and then cut, purge or
delete it. What the files hold is
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
  writable by the driver's user, under 1 GiB free, or at a path over 337 bytes, where
  a segment roll would allocate on the recorder thread (`log-directory-backing`).
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

4. Cut it, when the end of the recording is not wanted:

   ```sh
   cargo run --release -p cavalry -- record --truncate 0 --to 2026-10-29T09:36:00Z
   ```

   You will see `recording 0 truncated to <position>`: the driver resolved the instant
   through the recording's time index to the first position written after it, removed
   the segment files past that position, cut the one holding it so the bytes past it
   are zero, cut the time index, and moved the stop there. A position in place of the
   instant (`--to 3200`) is taken as given: a multiple of 32 at which a frame begins,
   from the recording's retained position to its stop. An instant needs `--log-dir`
   on a host with no default log directory. A replay of the recording now ends at the
   new stop.

5. Purge the segment files before a point, when the disk is wanted back:

   ```sh
   cargo run --release -p cavalry -- record --purge 0 --before 2026-10-29T09:36:00Z
   ```

   You will see `--before resolved to position <p>; purging before the segment base
   <b>` and `recording 0 purged before <b>`: the instant resolved to the position
   recorded at it, which is almost never a segment base, so the command rounded it
   down to the segment base at or before it and sent that, so that no byte recorded
   after the instant goes. The driver wrote the base as the recording's retained
   position and removed the segment files wholly before it, one per duty cycle; the
   time index keeps its entries. A replay from the start, or from a position before
   the retained position, is refused with error 23 naming it. A base at or below the
   retained position is nothing to purge, which the driver refuses with error 23 and
   the retained position, so you see that an earlier purge already took it. A
   position in place of the instant (`--before 134217728`) is sent as given and must
   be a segment base.

6. Delete it, when none of it is wanted:

   ```sh
   cargo run --release -p cavalry -- record --delete 0
   ```

   You will see `recording 0 deleted`: every file of the recording is gone and its
   catalog slot reads deleted, which no command reads again; the id is never reused.

From a program, the same six steps are `client.start_recording("cavalry:ipc", 1001)?`,
which returns a `Recording` with `recording_id()`, `position()` and `synced_position()`
read from the counters file, `client.stop_recording(id)?`, from any client, or
`recording.stop(&mut client)?`, the same command with the handle's id, and
`client.truncate_recording(id, position)?`, `client.purge_segments(id, segment_base)?`
and `client.purge_recording(id)?`, each with a position, since a program resolves an
instant through `cavalry::index::position_after` or `position_at` itself. Dropping the
`Recording` stops nothing. After a stop, whichever client asked for it,
`recording.stop_position()` is the position the recording stopped at, and `position()`
and `synced_position()` return it too. That position is durable: the stop synced it, or,
when the stop's own writes failed, the last sync before the stop covered it (see
`recording-write-failed` below).

A program that must know its messages are on the disk before it goes on, to
acknowledge them for one, does not wait for the recorder's sync once a second. It
waits until `recording.position()` has reached the position its last `offer` returned,
or until `recording.stop_position()` is `Some`. Then it calls
`recording.sync(&mut client)?` and reads `recording.synced_position()`. It
acknowledges its messages up to that position and no further, never up to the
position `offer` returned. While the recording is live, `synced_position()` is then at
or past the position `position()` had reached. The call returns when the recorder has
synced what it had written; it syncs nothing the recorder had not written yet, which
is why the program waits for `position()` first. Once `stop_position()` is `Some`, the
recording has stopped: `position()` and `synced_position()` return the stop position
and never move again, so the program stops waiting for them, and `sync` is `Ok` with
nothing done. After a sync that failed, on a failing or full disk, the stop position
is the position last synced, below what was written, and the messages past it may not
be on the disk; `recording-write-failed` below says why and what to do.
`recording.request_sync(&mut client)?` asks for the same sync and returns at once, for
a program that polls `synced_position()` in a loop of its own
([ADR 0069](../decisions/0069-sync-a-recording-on-command-as-client-protocol-version-8.md)).

## How to verify it worked

- `ls -l ~/.local/share/cavalry/log` lists `catalog`, `0-0.rec` and `0.idx`; the first
  bytes of `0-0.rec` are the stream's frames as the publisher wrote them, header and
  body, so a position in the recording is a position in the stream.
- `just doctor` after the stop lists no recording and reports no `recording-*` finding.
- After step 4, `cavalry replay --recording 0 --from start` ends at the new stop; after
  step 5, `ls -l ~/.local/share/cavalry/log` no longer lists the segment files before
  the base you purged, and a replay from a position before it is refused with error 23;
  after step 6, the directory holds no `0-*.rec` and no `0.idx`, and `cavalry record
  --delete 0` again is refused with error 21, whose message says the slot is deleted.
- `cavalry record --stop 0` a second time prints `recording 0 stopped` again: a stop of
  a stopped recording is success with nothing written.

## When it does not work

- `error 20 (no publication to record)`: nothing live publishes on that stream and
  channel; start the publisher first. A publication is gone once its client is.
- `error 22 (already recording)`: every live publication on the stream is recorded; the
  message names the recording. Stop one with `--stop`, or let it be.
- `no recording yet (recording-armed)` from `start_recording` in a program: the stream
  is a `cavalry:udp` stream with a subscription and no publication or image, so the
  driver holds the recording armed and starts it when the stream's next image arrives.
  Call `client.arm_recording(channel, stream_id)?` before the publisher starts and
  poll the `ArmedRecording` it returns, which hands over the `Recording` at the
  image's first frame; or call `start_recording` once the image is there.
- `error 21 (unknown recording)`: the id is not in the catalog; `doctor` lists the live
  recordings by id.
- `error 10 (resource temporarily unavailable)`: the recordings registry (1024), the
  catalog (1048576 slots over the directory's life), the counters file or the recorder's
  queue is full; stop recordings that are not needed, or start the driver on a new log
  directory.
- `error 12 (storage space)`: the time index or the first segment file could not be
  created; `doctor --log-dir` says what the directory is short of. The catalog slot is
  taken and stopped at its start.
- `error 12 (storage space)` from `sync_recording` or `recording.sync`: the sync
  failed, and the driver has given the recording up at the position last synced, as
  `recording-write-failed` below describes; `recording.stop_position()` gives that
  position. Check the disk the log directory is on, then start a new recording.
- `error 6 (unknown command type)` from `sync_recording` or `recording.sync`: the
  driver is older than this client and does not know the command; upgrade the driver.
- `recording-write-failed` in `doctor`: a write or a sync of a recording failed, and
  the recording stopped. After a write that the disk refused, it stopped at the
  position written, and the recording is whole and replayable up to there. After a
  sync that failed, or when the writes of the stop itself failed, it stopped at the
  position last synced: `recording stopped` and `recording.stop_position()` give that
  position, which can be below what `position()` returned before, since what was
  written after the last sync that succeeded may not be on the disk, and the driver
  tells no client of it and replays none of it. The catalog then lists the recording
  as live until the driver's next start, which rebuilds its stop from the files, at
  or above that position. Free space, or move the log directory, start the driver
  again, and start the recording again
  ([ADR 0064](../decisions/0064-announce-a-failed-stop-at-the-position-last-synced.md),
  [ADR 0065](../decisions/0065-give-up-a-recording-whose-sync-failed-without-a-stop.md)).
- `driver failed (log-dir): log directory locked`: another driver holds the directory;
  stop it, or give this one its own with `--log-dir`.
- `error 24 (recording busy)`: the recording is live, read by a replay, or under
  another truncate, purge or delete; the message says which and what to do: `--stop`
  it, `cavalry replay --stop` the replay, or wait for the other command's answer.
- `error 23 (position out of range)` from `--truncate` or `--purge`: the position is
  not at a frame of the recording between its retained position and its stop, or not
  a segment base above the retained position; the message names both. An instant that
  resolves before the retained position means an earlier purge already took it.
- `error 12 (storage space)` from `--truncate`, `--purge` or `--delete`: a file could
  not be read, cut or removed; the message names it, and the driver has repaired the
  recording's catalog slot from what its files hold. Fix the directory's permissions
  or disk, and send the command again.
- `log-directory-space` in `doctor --log-dir`: the log directory's filesystem has under
  a tenth free. The finding names the stopped recording whose segment files a purge
  frees the most of, and the `cavalry record --purge` and `--delete` commands that
  remove them, with the files and MiB each frees.
- `recording-over-half` in `doctor --log-dir`: one recording takes over half the
  filesystem, a live one by its `recording.bytes` gauge, a stopped one by its
  descriptor. Stop a live one with `--stop`, then purge its segments before the
  filesystem fills, as step 5 does.
- `log-directory-backing` in `doctor` says the log directory's path is over 337 bytes:
  the recorder opens each segment file by its full path, and the standard library
  allocates for a path of 384 bytes or more instead of using its stack buffer, so a
  roll would allocate on the recorder thread, a data-plane thread, which the bench's
  zero-allocation gate catches only on a host whose directory is that long. The bound
  is `cavalry_log::recorder::LOG_DIR_PATH_LIMIT`, the directory and the longest segment
  name under 384. Set `--log-dir` (`CAVALRY_LOG_DIR`) to a shorter absolute path before
  starting the driver.
