# Publish and subscribe in ten minutes

A tutorial: in ten minutes, on a laptop or a cloud VM, start a driver, record a stream
and publish 1,000 messages on it from a program that reads them back, replay the
recording from the durable log, and read `doctor`. You type six commands and see a
result after each. Nothing is configured. In Aeron, this is the media driver and Archive
with a client that uses `tryClaim`, `poll` and `replay` ([glossary](../glossary.md)).

The same path is `just quickstart`, which CI runs on Linux and macOS on every commit, so
what this page says happens is what the repository checks.

## Before you start

- A Rust toolchain and a checkout of this repository. There is no release yet, so the
  binary and the program are built from the checkout; the commands below run the binaries
  that `cargo build --release` puts under `target/release/`, where a release would put
  `cavalry` on the PATH.
- On Linux, `/dev/shm` mounted, which it is on every common distribution.

## 1. Build

```sh
cargo build --release -p cavalry --bin cavalry --example quickstart
```

You will see `Finished` and nothing else. The first build takes a minute or two.

## 2. Start the driver

In one terminal:

```sh
target/release/cavalry run
```

You will see one line: `cavalry run: driver started on /dev/shm/cavalry-<user> (pid
<pid>), recording into ~/.local/share/cavalry/log, conductor idle strategy
backoff; Ctrl-C or SIGTERM stops it`. On macOS the
directory is under the temporary directory. Leave it running.

## 3. Run the program

In a second terminal:

```sh
target/release/examples/quickstart
```

You will see three lines: `quickstart: client <id> attached to the driver at <directory>`,
then `quickstart: 1000 messages published on stream 1001 by session <session> and
received once, in order, in <n> ms`, then `quickstart: recording 0 of stream 1001
stopped; cavalry replay --recording 0 --from 0 plays it back`. The program is
[`crates/cavalry/examples/quickstart.rs`](../../crates/cavalry/examples/quickstart.rs):
it connects with `Cavalry::connect()`, adds a subscription and a publication on stream
1001, starts a recording of the stream with `start_recording`, claims a frame per
message and writes it in place, and polls the subscription, which hands each message to
a closure as a view into the shared log; then it stops the recording. Until the
subscription's image joins, the claim is refused with `not-connected`; a full window is
`back-pressured`; the program polls and tries again, which is what every refusal means.
The recording is the driver's: its recorder thread wrote every committed frame to the
log directory, `~/.local/share/cavalry/log` unless `CAVALRY_LOG_DIR` says otherwise, and
it outlived the program. The recording id is 0 on a fresh log directory; a directory
that holds earlier recordings gives the next id, which is the one the program printed.

## 4. Replay the recording

```sh
target/release/cavalry replay --recording 0 --from 0 | tail -2
```

You will see the last message and the summary: `63936 e703000000000000`, the position of
the thousandth message in the recording and its body, the number 999 as eight
little-endian bytes in hex, then `replay <n> of recording 0 ended at position 64000: 1000
messages from position 0`. Without `tail` the command prints all 1,000 lines, one per
message, each a position and a body: the driver's replayer played the recording from
position 0 into a replay stream of the command's own, and the command read it through an
ordinary subscription, as any program would. [Replay a recording](../how-to/replay-a-recording.md)
has the command's other forms, from and to an instant among them.

## 5. Read doctor

```sh
target/release/cavalry doctor
```

You will see a report whose first line is `cavalry doctor: <directory>, checks run: <n>`
and whose last line is `0 failed, <n> warnings`: the driver is running, its counters are
healthy, and on Linux the host's governor, huge pages and isolated CPUs are reported.
[Run a driver and read doctor](../how-to/run-the-driver.md) explains every line.

## 6. Stop the driver

Ctrl-C in the first terminal. You will see `cavalry run: stopping` and `cavalry run:
closed; <directory> stays for doctor, and the next driver remakes it`. `doctor` now exits
1 and says the driver closed.

## What you did

One driver per host owns the shared-memory files; a client maps them and publishes by
claim and commit, with no copy, no allocation and no lock on the message path; a
subscription reads committed frames where its position says and tells the driver how far
it got through a counter; the driver moves the publisher's limit behind the slowest
subscriber, so a publisher that gets ahead is told, never buffered; the driver's recorder
wrote the stream to the durable log as one more reader, and its replayer played it back
as one more publication, so the same subscription code read the live stream and the
replay. Every refusal is a result the program decides on, and every state is a counter
`doctor` reads.

## What to read next

- [Run a driver and read doctor](../how-to/run-the-driver.md): the binary's flags, the
  two configuration keys and every `doctor` line.
- [Record a stream to the durable log](../how-to/record-a-stream.md) and
  [Replay a recording](../how-to/replay-a-recording.md): the two commands from a shell,
  what the log directory holds, and replaying between two instants.
- [`crates/cavalry-client/README.md`](../../crates/cavalry-client/README.md): the client
  API, with the same program as its doctest, and what the crate must never do.
- [`spec/client-protocol.md`](../../spec/client-protocol.md) and
  [`spec/log-buffer.md`](../../spec/log-buffer.md): what the client and the driver say to
  each other, and how a message gets from a claim to a handler.
