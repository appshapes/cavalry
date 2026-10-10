# Cavalry

Send in the cavalry. Aeron-class latency without Aeron-class expertise.

Cavalry is a brokerless, low-latency message transport with a durable log and Raft
replication, written in Rust. One daemon per host; clients map shared-memory log
buffers; IPC and UDP share one API. Encrypted, observable and highly available in the
open core, with first-class clients in every language.

**Status:** pre-code. This repository holds the invariants, the decisions and the
competitive brief that the implementation will be built from. There is nothing to run
yet.

## Start here

| If you want to | Read |
| --- | --- |
| Understand why Cavalry exists and what it must beat | [`docs/explanation/why-cavalry.md`](docs/explanation/why-cavalry.md) |
| Work on the code, human or agent | [`CLAUDE.md`](CLAUDE.md), the invariants and gates |
| Know what a word means | [`docs/glossary.md`](docs/glossary.md) |
| Know why something was decided | [`docs/decisions/`](docs/decisions/) |
| Write documentation | [`.claude/rules/docs.md`](.claude/rules/docs.md) |

## The ten-minute path

Not yet. When it exists, it will be here, and `just quickstart` will run it in CI.

## License

Apache-2.0, with no relicensing clause. See [`LICENSE`](LICENSE).
