# 0150. Say on the C ABI's page that a snapshot's adds and commands wait the response timeout

- **Status:** Accepted
- **Date:** 2026-11-22
- **Invariants affected:** 3, 6
- **Supersedes:** none. It corrects one sentence that ADR 0146 added to
  `spec/c-abi.md`, "Threads", and the doc comments and READMEs that say the same.

## Context

`spec/c-abi.md`, "Threads", says of `cavalry_cluster_service_snapshot` that it "sends
commands with [the client] and waits for their answers, a snapshot for up to the
snapshot timeout, 1 s, at each step". A caller reads that as the bound of the call:
a few seconds at the most.

It is not the bound. A service's first snapshot adds two counters and two
publications, and every snapshot starts and stops a recording
(`crates/cavalry-client/src/service.rs`, `ClusterService::snapshot` and
`write_snapshot`). Each of those is an add or a command of the client, and a client
waits for the driver's answer to one for up to the response timeout, 10 s
(`spec/client-protocol.md`, "Liveness and timeouts";
`crates/cavalry-client/src/conductor.rs`, `RESPONSE_TIMEOUT`; the page's own
"Functions" says so of every add and command). That is not the driver timeout,
which is also 10 s and is the age of a heartbeat at which a client closes. The
snapshot timeout, 1 s (`spec/cluster.md`, "Values"), bounds the steps between the
commands: a message a publication takes, the recorder's write, the stop announced.

The difference matters to a caller because the call holds the client, and in the
five clients the client's lock, for as long as it runs. Two independent reads of
this work item's changes found the sentence and the places that repeat it: the five
clients' READMEs and doc comments, the doc comment of the Rust
`ClusterService::snapshot` and its crate's README. `spec/` changes with an Accepted
ADR in the same change (`CLAUDE.md`, "Gates", invariant 6), a sentence included.

## Decision

The page gives each bound by its name: an add or a command of a snapshot or of a
start waits up to the response timeout, 10 s, where the driver does not answer; a
snapshot waits up to the snapshot timeout, 1 s, at each step of its own; a start
waits for its snapshot's replay, up to the service snapshot load timeout, 5 s,
between two of its messages. The page names a snapshot's steps no more than that:
`spec/cluster.md` is where they are specified. The doc comment of
`cavalry_cluster_service_snapshot`, and so the header's, says the same, and so do
the doc comment of the Rust `ClusterService::snapshot`, its crate's README and the
five clients' words. The ABI's version stays 6: no function, type, constant, layout
or code changes, and no behaviour does. The page now says what version 6 has done
since it landed.

## Alternatives considered

- **Bound the whole call at the snapshot timeout.** Lost. It would be a change of
  behaviour, and a wrong one: a driver that takes 2 s to answer an add under load
  would fail a snapshot that it is about to take, where every other add of the
  client waits for it.
- **Leave the sentence, since the five READMEs are right.** Lost. The page is what a
  sixth client's author reads, and an unspecified wait is a bug in the spec
  (`CLAUDE.md`, "Protocol").
- **A new version of the ABI.** Lost. A version tells a built program that the
  library differs from its header. Nothing a program is built against differs.

## Evidence

- `crates/cavalry-client/src/service.rs`, `ClusterService::snapshot`: `add_counter`
  twice and `SnapshotPublications::add` on a service's first snapshot, then
  `write_snapshot`, which starts and stops a recording. Each goes through the
  client's `request`, to a deadline of `RESPONSE_TIMEOUT`, 10 s
  (`crates/cavalry-client/src/client.rs`).
- The same file's waits against `SNAPSHOT_TIMEOUT_NS`, 1 s: `offer_within`, a
  publication that takes no message, the snapshot's own or the one its ack is
  written on; and `stop_at`, a recorder that writes nothing more, and a stop that is
  not announced once the driver has answered it.
- What an unanswered command ends as, by reading. An add of a counter or of a
  publication, or the recording's start, ends the call with the client's error. The
  recording's stop ends it with `ClusterError::SnapshotFailed` at the stop step,
  after the response timeout, and `write_snapshot` then asks for the stop once more
  without waiting for its announcement, which is a second wait of up to 10 s: about
  20 s in all, with a message that says "within a second".
- No measurement: the change is to words. No test changes, since no behaviour does.

## Consequences

- A caller budgets a snapshot's call by the response timeout for each of its adds
  and commands where its driver may not answer, and gives a service whose snapshots
  must not hold its other threads a client of its own, as the page already says.
- A stop that the driver does not answer is told as a failed stop step, in a message
  that says "within a second" after 10 s. This ADR changes no behaviour and leaves
  the message; it is noted on work item 335 as open.
- `just lint`'s header check holds the header to the doc comment, so the two cannot
  part.
- Revisit: when a snapshot gains a bound of its own for the whole call, or the
  stop's message is corrected, each with whatever version of the ABI it needs.

## References

- Work item 335, "Take a service's snapshot through the C ABI and the five clients".
- ADR 0146, which took the C ABI to version 6 and wrote the sentence.
- ADR 0149, the five clients' calls, whose READMEs give both bounds.
- `spec/c-abi.md`, "Threads"; `spec/client-protocol.md`, "Liveness and timeouts";
  `spec/cluster.md`, "Values", the snapshot timeout and the service snapshot load
  timeout.
