# cavalry-driver

The daemon: conductor, sender, receiver and idle strategies. Data plane.

## What this does

- Runs as one driver per host, on one directory: `/dev/shm/cavalry-<user>` on Linux,
  `<tmp>/cavalry-<user>` elsewhere. It creates the counters file, the command ring and
  the broadcast buffer there, and a log buffer per publication; clients attach through
  `cavalry-shm`, and it never sits in the message path as a broker. The directory
  protocol, the commands it reads and the events it answers with are
  `spec/client-protocol.md` (ADR 0019).
- Chooses each publication's session id: the first is a random i32, drawn from the
  operating system's randomness when the driver starts, and each next one is one
  more, past any id a live publication of the stream holds (`spec/client-protocol.md`,
  "Commands"). Random, and not the clock: two drivers started in one instant, a
  cluster's members under one supervisor, must not send one stream under one session
  id, since a receiver that hears both on an endpoint holds them as one session and
  answers the first alone.
- Runs the conductor on a dedicated thread with an idle strategy, what the thread does
  when it has no work: busy-spin, yield or backoff. Backoff is the default; `doctor`
  recommends busy-spin on isolated cores. On every duty cycle the conductor stores its
  heartbeat, reads and answers commands, unblocks the ring, times out clients, and keeps
  every publication's positions, cleaning, unblocking and closing as
  `spec/log-buffer.md`, "The driver" says, loading a publication's producer position
  at most once every two milliseconds so that the publisher's cache line is not read on every
  cycle (ADR 0022). A closed publication's log, and an image's over UDP, stay for the
  counter reuse timeout, 1 s, after the last `available image` that named them,
  unless that subscription is removed, so that a client's conductor finds what the
  event names (ADR 0078). The connected flag the publisher reads is raised in the limit pass
  after the limit's raise, and lowered where the last subscriber, receiver or recording
  goes, before the limit pass lowers the limit. So a claim reads `back pressured` only
  while a subscriber, a recording or a receiver reads the stream, and `not connected`
  from the moment the last one is gone. `back pressured` means a subscriber a window
  behind, except in a cycle whose cleaning holds the raise back, where the subscriber
  is at the limit, and once for a claim whose two loads straddle a raise
  (ADR 0048; card 187). The strategy it runs is stored once in the
  `conductor.idle_strategy` counter, so that `doctor` can name it from the counters file.
  `Driver::start_with_threads` takes a hook each thread runs first, named the thread it
  is on, where a caller that owns the host's CPUs pins the conductor to a CPU of its own
  and leaves the recorder, the replayer, the sender and the receiver on the housekeeping
  CPUs, as the bench does under `--pin`; otherwise every thread keeps the affinity of
  the thread that started the driver.
- Records a stream to the durable log on a second thread, the recorder, with the same
  idle strategy: `start recording` and `stop recording` (`spec/client-protocol.md`,
  "Commands") name a stream, the conductor hands the recorder each start and stop
  through a bounded queue and never touches the disk, the recorder takes blocks of
  committed frames from the publication's log buffer and writes them into the log
  directory's segment files with one positional write each, reading at the position
  of a publisher that does not pause once in 50 µs and not every duty cycle, since
  each read there takes the cache line the publisher writes next, and writing what is
  committed before a stop or a sync it is asked for (ADR 0084), syncing once per
  second,
  or at once when a client's `sync recording` asks through the same queue (ADR 0069),
  and its position, `recording.position`, bounds the publisher's limit, so that a slow
  disk back-pressures the publisher and never loses a byte (ADR 0025). The log
  directory is opened under a lock at start and recovered (`spec/durable-log.md`, "The
  driver"); a second driver on it refuses to start. A recording one of whose
  syncs failed is given up with no stop run, since a sync asked again about bytes a
  sync has failed on can return success and write nothing; a stop whose own writes or
  syncs fail stops the recording all the same. Either is announced, in `recording
  stopped` and to every replay that followed the recording, at the position the last
  sync that returned success covered, never the position written, and its catalog
  slot stays live for the next start's recovery. A write the disk refused with no
  sync failing is followed by a stop that runs, at the position written.
  `recording.write_failed` counts a recording once, whichever failed (ADR 0064, ADR
  0065).
- Plays a recording back on a third thread, the replayer, which idles with the same
  strategy while it holds a replay and parks until the conductor hands it a request
  while it holds none, so a driver that replays nothing wakes no CPU for it:
  `start replay` names a recording, a position, a length and a replay stream id, the
  conductor creates the replay's publication, a log of the driver's own at the
  recording's start position so that a position in the replay is the position in the
  recording, and the replayer copies the recording's frames from its segment files into
  that log, each checked and each as recorded but for the replay's session and stream
  ids, whole frames only, so a length that ends inside a frame plays that frame and
  ends at its end, no faster than the slowest subscriber's position plus the window
  and never past what the recorder has synced; a subscription on the replay stream
  reads it as any stream, and `replay stopped` says where it ended and why (ADR 0030).
  A replay that follows a live recording allocates nothing while it runs: once a
  second the replayer asks the catalog where the recording stopped, and it reads the
  stop alone (`cavalry_log::catalog::CatalogReader::stop_position`), not the
  descriptor with its two strings. `cavalry-client`'s `replay_follow_allocations`
  test is the gate, over three seconds of such a replay.
- Truncates, purges and deletes a stopped recording on command: `truncate recording`,
  `purge segments` and `purge recording` (`spec/client-protocol.md`, "Commands"; ADR
  0031). The conductor admits each into a registry of commands in progress, bounded at
  `spec::MAX_COMMANDS_IN_PROGRESS` (64, code 10 when full), and refuses a recording that
  is live, read by a replay or under another command with code 24, as it refuses a
  `start replay` of a recording under a command, counted in `replay.start_failed`; the
  replayer reads the slot, code 21 for one it cannot read as a valid recording, state 2
  among them, checks the position against the slot and the files and does the file
  work one file per duty cycle; the recorder, which owns the catalog, makes each
  command's catalog writes, one slot and a sync each, in the page's order; and after a
  file error the replayer runs the file side of recovery's rules for the slot and the
  recorder writes the one slot they end in, so the slot and the files agree before the
  refusal is answered. The removal of a segment file allocates nothing while the log
  directory's path is at most 337 bytes (`cavalry_log::recorder::LOG_DIR_PATH_LIMIT`,
  the same limit as the recorder's roll, which `cavalry doctor --log-dir` checks): the
  replayer formats the file's name on its stack into a path it keeps, and the standard
  library hands a path under 384 bytes to the operating system from its stack. The
  rest of a command's file work allocates a few times per command and never per file:
  the listing of its files when it is admitted, a truncate's cut of the segment that
  holds its position and of the time index, and a delete's removal of the time index.
  A truncate, a delete and a refusal each move one counter once, when they end:
  `recording.truncated`, `recording.deleted` and `recording.retention_failed`, the last
  for a file the operating system refused and nothing else. A purge moves
  `recording.segments_purged` once per file instead, as it removes the file, so the
  counter reads a running purge's progress.
- Creates a `cavalry:udp` publication's log at position 0, or at the position its
  channel's `init-term-id`, `term-id` and `term-offset` name with `init-term-id` as its
  initial term id, so that its first setup frame carries that position and a join
  names the same keys or is refused (`spec/udp.md`, version 2; ADR 0059).
- Sends a `cavalry:udp` publication's log on a fourth thread, the sender, by the rules
  of `spec/udp.md`, "The sender" (ADR 0040, ADR 0041): `add publication` on
  `cavalry:udp?endpoint=<host>:<port>` resolves the host once, opens the channel's send
  endpoint, one socket shared by the publications on it, and hands the sender the
  socket and a reader on the publication's log through a bounded queue; the sender
  sends setup frames until the first status message answers, data in bursts of whole
  committed frames up to the sender limit, the receiver's consumption position plus its
  window, in one socket call per burst on Linux, heartbeats when it has nothing to send
  and with EOS once the publication is closed, and the frames a NAK names from the
  log's own term; the conductor keeps the publisher's limit at the sender position plus
  the publication window while a status message arrived within the connection timeout,
  cleans the log a term behind the sender position so that the resend window stays,
  counts a recording of the publication as one more reader, so that the limit is the
  window past the lesser of the sender position and the recorder's and nothing is
  cleaned that the recorder has not written (ADR 0097), and
  lingers a closed publication until the receiver acknowledges the end of stream or the
  linger passes. A publication whose channel names `ssc=true` is connected by a live
  recording of it as well, from the recorder's first write until its stop is asked:
  while no receiver answers, the conductor tells the sender which recording to follow,
  the sender sends no data and moves its position with the recorder's, counting in
  `sender.bytes_unsent` what it passed above the highest position it had sent to, and
  refusing a recording position that is not a frame boundary or lies more than two
  terms ahead, counted once in `log_buffer.rejected_position` with no client evicted; a
  receiver whose first status message names a position at most a term behind is
  joined there, the sender going back to send from it and taking off the gauge only
  what it had passed above that highest position, so that the gauge is never below
  zero, and one further behind is rejected and counted in the publication's
  `sender.joins_refused`; and when the recording stops, the
  conductor frees its position counter only after the sender has answered that it
  reads it no more (ADR 0098, ADR 0112). A publication's log has the term length its
  channel names with `term-length=<length>`, a power of two from 65536 to 67108864, and
  the 16 MiB term when it names none: the conductor creates the log, counts the
  position keys and cleans in that term, the sender and the recorder read it from the
  log's header, and an `add publication` that would join under another term length is
  refused (ADR 0099). On a control channel, `cavalry:udp?control=<host>:<port>|control-mode=manual`,
  the endpoint's socket is bound to the control address and connected to nothing, and
  the sender sends every datagram to each destination a client added with `add
  destination`, at most 64 per endpoint, each resolved when it is added to an address
  the control address can send to, the datagrams a destination did not take whole
  going to all of them again and a destination the socket refuses holding the others
  back by nothing; under `fc=min` it keeps per publication a table of
  at most 64 receivers by receiver id, the sender limit the least position plus window
  among them, a receiver dropped after 5 s without a status message (ADR 0053). A
  control channel's closed publication lingers for the linger timeout, 5 s, whatever a
  destination's status message says, since one destination's end of stream says
  nothing of another's, which may still be NAKing the tail (ADR 0054); a receiver
  whose stream ended stays in a closed publication's `min` table until the linger
  ends, so that `sender.flow_control_receivers` still counts it (ADR 0056). Every
  step has its counter, `sender.*`, `publication.connected` and
  `channel.*`, and the socket calls `std` lacks come through `nix`. The sender idles
  with the driver's strategy while it holds an endpoint or a publication and parks
  while it holds none, unparked by the conductor's next request, so a driver that
  sends nothing wakes no CPU for it, as it wakes none for the replayer.
- Receives a `cavalry:udp` stream into an image on a fifth thread, the receiver, by the
  rules of `spec/udp.md`, "The receiver" (ADR 0040, ADR 0042): `add subscription` on
  `cavalry:udp?endpoint=<host>:<port>` resolves the host once, binds the channel's
  receive endpoint, one socket shared by the subscriptions on it, and hands the receiver
  the socket and the stream wanted, and an add on a channel whose previous endpoint has
  lost its last subscription and not yet closed, its images still being retired or
  its socket still with the receiver, waits for the receiver's close and then runs as
  it arrived, so a client that removes and adds on one channel needs no retry and is
  given no image that is being retired, and one still waiting after the
  closing endpoint wait, 5 s, a receiver starved that long or a retired image's
  recording not yet stopped, its recorder behind, is code 4 with nothing registered,
  the message naming both causes and what to do for each, counted in
  `subscription.add_deferred_timed_out` (ADR 0049, ADR 0068); a setup
  frame for that stream goes back to the
  conductor, which creates the image, a log of type 2 under `images/` at the position
  the setup names with a registration id of its own, allocates its two counters and a
  `subscription.position` per subscription, announces it with `available image` and the
  sender's address as the source identity, and hands the receiver the log, its writer
  and the readers; the receiver inserts each frame at its term offset within the bounds
  of its last status message, a padding frame as the 32-byte header the sender carries
  it as (ADR 0045), scans the rebuild and the gap in bounded work per cycle,
  NAKs the gap after the NAK delay and every NAK retry, sends status messages on the
  three triggers with the slowest reader's position, cleans the log a term behind it,
  and retires an image its stream ended, that went quiet for the liveness timeout or
  whose last subscription left, which the conductor finishes with `unavailable image`
  and the log unlinked. A session record in one of four states keeps a second setup or
  a late retransmit from creating a second image, in a table of 4096. Every step has its
  counter, `receiver.*`, `image.*` and `channel.*`. `start recording` on a
  `cavalry:udp` channel records a publication sending on it or an image received on it,
  the recording one of the image's readers; on a stream with a live subscription and
  no image yet it arms the recording, answered with `operation success`, which starts
  at the image's join position when the stream's next image arrives, before the image
  is announced, and answers the command a second time with `recording ready`, or with
  `error` when that start fails or the stream's last subscription goes first, the four
  `recording.armed*` counters saying which (ADR 0059). The receiver idles with the driver's
  strategy while it holds an endpoint and parks while it holds none. `start replay` on
  a `cavalry:udp` channel plays the recording to that channel's endpoint: the replay's
  publication takes the sender's path as a client's does, with the recording's term
  length and MTU, and for the replay connect timeout it has a subscriber while its
  publication is connected, from the first status message without the send-setup flag
  until the connection timeout passes without one (ADR 0043).
- Adds a counter for a client that asks with `add counter`: a record of the counters
  file with the type id, the key and the label the client gave, its type one
  `counters.toml` gives the scope `client`, answered with `counter ready`. The client
  owns the counter and alone writes its value. The driver holds at most 256 of them,
  frees one when its owner removes it, closes or is timed out, and tells every client
  with `unavailable counter` before it does; another client's remove is code 5
  (`spec/client-protocol.md`, "Commands"; ADR 0062).
- Degrades under cgroup CPU throttling, shared-memory pressure or packet loss. It does
  not fall over, and every state transition increments a counter.

In Aeron, this is the media driver ([glossary](../../docs/glossary.md)).

## What it must never do

- Allocate, lock, make a syscall, await or panic per message (invariant 2). A message
  never triggers its own `sendmsg`; clock reads go through a cached clock. The
  conductor is not on the message path: a message goes from a publisher's claim to a
  subscriber's handler through the log buffer alone, and the conductor only moves the
  limit that bounds the publisher.
- Absorb back-pressure by growing. Every registry has a fixed capacity, a command past
  it is answered with error code 10, and the back-pressure result goes back to the
  publisher (invariant 3).
- Log on the data plane. Nothing here calls into `tracing` or `log`; it bumps a counter,
  and `doctor` explains the counter (invariant 4).
- Deliver a message twice or out of order, or lose one without reporting the gap to the
  subscriber (invariant 1).
- Remove a file whose creator is alive. A driver holds its directory by a lock of the
  operating system on the directory itself, from before it reads the directory until
  it stops, and the system drops the lock with the process, however it ends. A
  starter that is refused the lock answers `driver-active`, with the pid the holder
  wrote in the file `pid`, and changes nothing, so of two drivers started at one
  moment one runs and the other is refused. A starter that takes the lock and finds a
  pid there knows that driver has gone, and takes the directory at once (ADR 0079).
- Report a recording's position before the operating system holds the bytes, or a
  synced position before a sync made them durable; clean a recorded byte before the
  recorder has written it; or share a log directory with another driver (invariant 1).
- Play a byte of a recording a sync has not made durable, or a frame that fails a check,
  or a frame under another session and stream than the replay's own (invariant 1).

## Example

An embedded driver on a directory of its own: start it, see the files it created, see
a second driver refuse the live directory, close it, and start again on the directory
it left. This is the crate's doctest.

```rust
use cavalry_driver::{Driver, DriverError};

fn main() -> Result<(), DriverError> {
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("cavalry-driver-readme-{pid}"));
    // The durable log's directory, on disk and never under the driver directory, which
    // is memory; `Driver::default_log_dir()` is the rule a running driver uses.
    let log_dir = std::env::temp_dir().join(format!("cavalry-driver-readme-{pid}-log"));

    // Start: the directory is locked and holds this driver's pid and its three files,
    // the log directory holds its catalog under this driver's lock, and the conductor
    // and recorder are running.
    let driver = Driver::start(&dir, &log_dir)?;
    for file in ["pid", "counters", "command-ring", "broadcast-buffer"] {
        assert!(dir.join(file).is_file(), "{file}");
    }
    assert!(log_dir.join("catalog").is_file());

    // A second driver on the same directory is refused the lock this one holds.
    let refused = Driver::start(&dir, &log_dir).unwrap_err();
    assert_eq!(refused.reason(), "driver-active");
    println!("{refused}");

    // Close: the heartbeat becomes -1, the lock is released and the directory stays
    // for `doctor`. The next driver to start takes the lock, removes what this one
    // left and makes its files anew; the log directory it finds whole, with nothing
    // to repair.
    driver.close()?;
    let again = Driver::start(&dir, &log_dir)?;
    again.close()?;
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&log_dir).ok();
    Ok(())
}
```

Running it prints one line, the refusal with its reason, what to do and the docs anchor:

```text
driver refused (driver-active): another driver holds /tmp/cavalry-driver-readme-4242, running or starting there, pid 4242; stop that driver, or start this one on another directory; see docs/reference/client-protocol.md#the-driver-directory
```

## What to read next

- [`spec/client-protocol.md`](../../spec/client-protocol.md): the directory, the
  commands, the events, the timeouts and the sizes this driver implements, and
  [ADR 0019](../../docs/decisions/0019-adopt-aeron-control-protocol-as-the-client-protocol.md),
  why they are Aeron's.
- [`spec/log-buffer.md`](../../spec/log-buffer.md), "The driver": positions, cleaning,
  unblocking and closing, which `src/conductor.rs` runs on every duty cycle;
  [`spec/command-ring.md`](../../spec/command-ring.md) and
  [`spec/broadcast-buffer.md`](../../spec/broadcast-buffer.md), the two rings it
  reads and writes.
- [ADR 0007](../../docs/decisions/0007-build-ipc-pub-sub-first.md), the slice this
  driver is the fourth feature of; `tests/driver.rs`, a client attaching through
  `cavalry-shm` and `cavalry-proto` and exercising every command.
- [ADR 0025](../../docs/decisions/0025-record-a-stream-in-the-driver.md), the recorder
  thread, its queues and its counters; [`spec/durable-log.md`](../../spec/durable-log.md),
  what it writes; `src/recorder.rs`.
- [ADR 0030](../../docs/decisions/0030-replay-a-recording-into-a-subscription.md), the
  replayer thread, the replay's publication, the pacing and the end rules;
  `src/replayer.rs`.
- [`spec/udp.md`](../../spec/udp.md), "The sender" and "The receiver", and
  [ADR 0041](../../docs/decisions/0041-send-from-a-sender-thread-through-nix.md) and
  [ADR 0042](../../docs/decisions/0042-rebuild-an-image-on-a-receiver-thread.md), the
  sender and the receiver threads, their queues to the conductor and their counters;
  `src/sender.rs`, `src/receiver.rs`, `src/socket.rs`.
