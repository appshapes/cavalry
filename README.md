# Cavalry

[![ci](https://github.com/appshapes/cavalry/actions/workflows/ci.yml/badge.svg)](https://github.com/appshapes/cavalry/actions/workflows/ci.yml)

Send in the cavalry. Aeron-class latency without Aeron-class expertise.

Cavalry is a brokerless, low-latency message transport with a durable log and Raft
replication, written in Rust. One daemon per host; clients map shared-memory log
buffers; IPC and UDP share one API. Encrypted, observable and highly available in the
open core, with first-class clients in every language.

**Status:** the first slice works on one host. `cavalry run` starts a driver, a Rust
client publishes and subscribes over shared memory with back-pressure returned to the
caller, `cavalry doctor` explains the driver from its counters, and the ten-minute path
below runs in CI on Linux and macOS. Not yet: UDP, the durable log, Raft, the C ABI and
the language clients, and a release on crates.io
([ADR 0007](docs/decisions/0007-build-ipc-pub-sub-first.md)).

## Start here

| If you want to | Read |
| --- | --- |
| Understand why Cavalry exists and what it must beat | [`docs/explanation/why-cavalry.md`](docs/explanation/why-cavalry.md) |
| Work on the code, human or agent | [`CLAUDE.md`](CLAUDE.md), the invariants and gates |
| Know what a word means | [`docs/glossary.md`](docs/glossary.md) |
| Know what each crate does and must never do | [`crates/*/README.md`](crates/) |
| Know why something was decided | [`docs/decisions/`](docs/decisions/) |
| Write documentation | [`.claude/rules/docs.md`](.claude/rules/docs.md) |
| Ask the agent to make a change | [`docs/how-to/ask-the-agent.md`](docs/how-to/ask-the-agent.md) |
| Implement a feature, from card to commit | [`docs/how-to/implement-a-feature.md`](docs/how-to/implement-a-feature.md) |
| Publish and subscribe, in ten minutes | [`docs/tutorials/publish-and-subscribe.md`](docs/tutorials/publish-and-subscribe.md) |
| Read the documentation as a book | `just docs`, then open `target/book/index.html` |

## The ten-minute path

Five commands, from a checkout, on a laptop or a cloud VM; `just quickstart` runs the
same five in CI on Linux and macOS, and
[the tutorial](docs/tutorials/publish-and-subscribe.md) says what each one shows.

```sh
cargo build --release -p cavalry --bin cavalry --example quickstart
target/release/cavalry run &            # one driver per host; Ctrl-C or SIGTERM stops it
target/release/examples/quickstart      # publishes 1,000 messages and reads them back
target/release/cavalry doctor           # explains the driver from its counters; exit 0
kill -TERM %1                           # the driver closes and leaves its directory for doctor
```

The program is [`crates/cavalry/examples/quickstart.rs`](crates/cavalry/examples/quickstart.rs),
thirty lines with `cavalry::Cavalry`: connect, add a subscription and a publication on a
stream, claim a frame and write in place, poll the subscription, every message once and in
order. No configuration: the driver's directory is `/dev/shm/cavalry-<user>` on Linux and
under the temporary directory elsewhere, and `CAVALRY_DIR` moves it
([configuration](docs/reference/config.md)).

## License

Apache-2.0, with no relicensing clause. See [`LICENSE`](LICENSE).
