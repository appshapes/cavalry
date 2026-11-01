# cavalry-driver

The daemon: conductor, sender, receiver and idle strategies. Data plane.

## What this does

- Runs as one driver per host, on one directory: `/dev/shm/cavalry-<user>` on Linux,
  `<tmp>/cavalry-<user>` elsewhere. It creates the counters file, the command ring and
  the broadcast buffer there, and a log buffer per publication; clients attach through
  `cavalry-shm`, and it never sits in the message path as a broker. The directory
  protocol, the commands it reads and the events it answers with are
  `spec/client-protocol.md` (ADR 0019).
- Runs the conductor on a dedicated thread with an idle strategy, what the thread does
  when it has no work: busy-spin, yield or backoff. Backoff is the default; `doctor`
  recommends busy-spin on isolated cores. On every duty cycle the conductor stores its
  heartbeat, reads and answers commands, unblocks the ring, times out clients, and keeps
  every publication's positions, cleaning, unblocking and closing as
  `spec/log-buffer.md`, "The driver" says, loading a publication's producer position
  at most once every two milliseconds so that the publisher's cache line is not read on every
  cycle (ADR 0022). The strategy it runs is stored once in the
  `conductor.idle_strategy` counter, so that `doctor` can name it from the counters file.
  `Driver::start_with_threads` takes a hook each thread runs first, named the thread it
  is on, where a caller that owns the host's CPUs pins the conductor to a CPU of its own
  and leaves the recorder, the replayer and the sender on the housekeeping CPUs, as the
  bench does under `--pin`; otherwise every thread keeps the affinity of the thread
  that started the driver.
- Records a stream to the durable log on a second thread, the recorder, with the same
  idle strategy: `start recording` and `stop recording` (`spec/client-protocol.md`,
  "Commands") name a stream, the conductor hands the recorder each start and stop
  through a bounded queue and never touches the disk, the recorder takes blocks of
  committed frames from the publication's log buffer and writes them into the log
  directory's segment files with one positional write each, syncing once per second,
  and its position, `recording.position`, bounds the publisher's limit, so that a slow
  disk back-pressures the publisher and never loses a byte (ADR 0025). The log
  directory is opened under a lock at start and recovered (`spec/durable-log.md`, "The
  driver"); a second driver on it refuses to start.
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
  refusal is answered. Each outcome moves its counter: `recording.truncated`,
  `recording.segments_purged`, `recording.deleted` and `recording.retention_failed`,
  the last for a file the operating system refused and nothing else.
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
  cleans the log a term behind the sender position so that the resend window stays, and
  lingers a closed publication until the receiver acknowledges the end of stream or the
  linger passes. Every step has its counter, `sender.*`, `publication.connected` and
  `channel.*`, and the socket calls `std` lacks come through `nix`. The sender idles
  with the driver's strategy while it holds an endpoint or a publication and parks
  while it holds none, unparked by the conductor's next request, so a driver that
  sends nothing wakes no CPU for it, as it wakes none for the replayer. The receiver, the
  image and `start recording` on a `cavalry:udp` channel follow (card 181): a
  subscription on one is refused with code 11 until then.
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
- Remove a file whose creator is alive. A directory with a fresh heartbeat is
  `driver-active`, and the driver refuses to start.
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

    // Start: the directory and its three files exist, the log directory holds its
    // catalog under this driver's lock, and the conductor and recorder are running.
    let driver = Driver::start(&dir, &log_dir)?;
    for file in ["counters", "command-ring", "broadcast-buffer"] {
        assert!(dir.join(file).is_file(), "{file}");
    }
    assert!(log_dir.join("catalog").is_file());

    // A second driver on the same directory finds this one's heartbeat and refuses.
    let refused = Driver::start(&dir, &log_dir).unwrap_err();
    assert_eq!(refused.reason(), "driver-active");
    println!("{refused}");

    // Close: the heartbeat becomes -1 and the directory stays for `doctor`. The next
    // driver to start finds a dead driver there and makes the directory anew; the log
    // directory it finds whole, with nothing to repair.
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
driver refused (driver-active): /tmp/cavalry-driver-readme-4242 holds a driver whose heartbeat is fresh, created by pid 4242; stop that driver, or start this one on another directory; see docs/reference/client-protocol.md#the-driver-directory
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
