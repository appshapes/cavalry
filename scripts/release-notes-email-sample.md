# What's new in Cavalry, October 10 to 16, 2026

Cavalry is a brokerless, low-latency message transport with a durable log and Raft replication, written in Rust. **This email covers the week's commits on master**: the first slice runs end to end on one host, and the durable log's format is decided.

## What's new

### Publish and subscribe over shared memory on one host

`cavalry run` starts a driver, a Rust client publishes and subscribes on a stream through shared memory with back-pressure returned to the caller, and `cavalry doctor` explains the driver from its counters. The README's ten-minute path runs in CI on Linux and macOS.

```in a terminal
cargo build --release -p cavalry --bin cavalry --example quickstart
target/release/cavalry run &
target/release/examples/quickstart
target/release/cavalry doctor
```

[Step by step →](https://github.com/appshapes/cavalry/blob/master/docs/tutorials/publish-and-subscribe.md)

### Configure by environment variable, or not at all

Zero configuration is the supported configuration. The two keys that exist, `dir` and `conductor.idle.strategy`, are read from `CAVALRY_DIR` and `CAVALRY_CONDUCTOR_IDLE_STRATEGY`; `--dir` wins over the variable, and `doctor` says when a key is set wrong.

### Measure the data plane on a dedicated host

`just bench` writes HdrHistogram percentiles for every row a message passes through, and CI compares them with a baseline per runner. The latency gate runs on a dedicated bench host, so that a regression is told from a shared runner's noise.

[Step by step →](https://github.com/appshapes/cavalry/blob/master/docs/how-to/run-the-bench.md)

## Decided

- ADR 0023: the durable log and replay are the second slice, built as four features.
- ADR 0024: on disk, the durable log is a fixed-slot catalog, segment files of raw term bytes and a time index; `spec/durable-log.md` version 0 is the format. [Read the decision →](https://github.com/appshapes/cavalry/blob/master/docs/decisions/0024-lay-out-the-durable-log.md)
- ADR 0025, proposed: the driver records a stream to the durable log, with Aeron Archive's commands in the client protocol.

## Before you update

- The workspace's Rust floor moved from 1.85 to 1.89, for the standard library's file lock that the log directory holds.

## Try it

Not running it yet? [The ten-minute path](https://github.com/appshapes/cavalry#the-ten-minute-path) is five commands from a checkout.
