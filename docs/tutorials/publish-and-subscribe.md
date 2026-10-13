# Publish and subscribe in ten minutes

A tutorial: in ten minutes, on a laptop or a cloud VM, start a driver, publish 1,000
messages from a program and read them back in the same program, and read `doctor`. You
type five commands and see a result after each. Nothing is configured. In Aeron, this is
the media driver with a client that uses `tryClaim` and `poll` ([glossary](../glossary.md)).

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
<pid>), conductor idle strategy backoff; Ctrl-C or SIGTERM stops it`. On macOS the
directory is under the temporary directory. Leave it running.

## 3. Run the program

In a second terminal:

```sh
target/release/examples/quickstart
```

You will see two lines: `quickstart: client <id> attached to the driver at <directory>`,
then `quickstart: 1000 messages published on stream 1001 by session <session> and
received once, in order, in <n> ms`. The program is
[`crates/cavalry/examples/quickstart.rs`](../../crates/cavalry/examples/quickstart.rs):
it connects with `Cavalry::connect()`, adds a subscription and a publication on stream
1001, claims a frame per message and writes it in place, and polls the subscription,
which hands each message to a closure as a view into the shared log. Until the
subscription's image joins, the claim is refused with `not-connected`; a full window is
`back-pressured`; the program polls and tries again, which is what every refusal means.

## 4. Read doctor

```sh
target/release/cavalry doctor
```

You will see a report whose first line is `cavalry doctor: <directory>, checks run: <n>`
and whose last line is `0 failed, <n> warnings`: the driver is running, its counters are
healthy, and on Linux the host's governor, huge pages and isolated CPUs are reported.
[Run a driver and read doctor](../how-to/run-the-driver.md) explains every line.

## 5. Stop the driver

Ctrl-C in the first terminal. You will see `cavalry run: stopping` and `cavalry run:
closed; <directory> stays for doctor, and the next driver remakes it`. `doctor` now exits
1 and says the driver closed.

## What you did

One driver per host owns the shared-memory files; a client maps them and publishes by
claim and commit, with no copy, no allocation and no lock on the message path; a
subscription reads committed frames where its position says and tells the driver how far
it got through a counter; the driver moves the publisher's limit behind the slowest
subscriber, so a publisher that gets ahead is told, never buffered. Every refusal is a
result the program decides on, and every state is a counter `doctor` reads.

## What to read next

- [Run a driver and read doctor](../how-to/run-the-driver.md): the binary's flags, the
  two configuration keys and every `doctor` line.
- [`crates/cavalry-client/README.md`](../../crates/cavalry-client/README.md): the client
  API, with the same program as its doctest, and what the crate must never do.
- [`spec/client-protocol.md`](../../spec/client-protocol.md) and
  [`spec/log-buffer.md`](../../spec/log-buffer.md): what the client and the driver say to
  each other, and how a message gets from a claim to a handler.
