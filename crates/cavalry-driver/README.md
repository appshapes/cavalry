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
  at most once per millisecond so that the publisher's cache line is not read on every
  cycle (ADR 0022). The strategy it runs is stored once in the
  `conductor.idle_strategy` counter, so that `doctor` can name it from the counters file.
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
- Will run dedicated threads for the sender and the receiver, pinned when the operating
  system allows, when the UDP slice arrives: a receiver that detects a gap sends a NAK,
  the sender retransmits from its own term buffer, and status messages drive flow
  control with the max, min and tagged policies.
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
