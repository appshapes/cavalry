# Cavalry

[![ci](https://github.com/appshapes/cavalry/actions/workflows/ci.yml/badge.svg)](https://github.com/appshapes/cavalry/actions/workflows/ci.yml)

Send in the cavalry. Aeron-class latency without Aeron-class expertise.

Cavalry is a brokerless, low-latency message transport with a durable log and Raft
replication, written in Rust. One daemon per host; clients map shared-memory log
buffers; IPC and UDP share one API. Encrypted, observable and highly available in the
open core, with first-class clients in every language.

**Status:** scaffold. The Cargo workspace exists with the nine crates from the
repository map in `CLAUDE.md`, all empty. The `cavalry` binary reports that it is not
built yet and exits 1. The invariants, the decisions and the competitive brief are what
the implementation will be built from.

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
| Read the documentation as a book | `just docs`, then open `target/book/index.html` |

## The ten-minute path

Not yet. When it exists, it will be here, and `just quickstart` will run it in CI.

## License

Apache-2.0, with no relicensing clause. See [`LICENSE`](LICENSE).
