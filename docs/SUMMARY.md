# Summary

The four kinds of page, in the order `.claude/rules/docs.md` defines them. Every page
under `docs/` is listed here; `just lint-docs` fails on one that is not.

# Explanation

- [Why Cavalry, and why not Aeron](explanation/why-cavalry.md)
- [Should ZigBolt's design inform Cavalry's?](explanation/should-zigbolt-inform-cavalry.md)

# How-to

- [Ask the agent to make a change](how-to/ask-the-agent.md)
- [Implement a feature, from card to commit](how-to/implement-a-feature.md)
- [Run the bench and compare it with the baseline](how-to/run-the-bench.md)
- [Set up a bench host for the latency gate](how-to/set-up-a-bench-host.md)

# Reference

- [Data frame header](reference/frame.md)
- [Counters file](reference/counters-file.md)
- [Counters](reference/counters.md)
- [Log buffer](reference/log-buffer.md)
- [Command ring](reference/command-ring.md)
- [Broadcast buffer](reference/broadcast-buffer.md)
- [Glossary](glossary.md)

# Decisions

- [ADR template](decisions/0000-template.md)
- [0001. Record decisions as ADRs in the repository](decisions/0001-record-decisions-as-adrs.md)
- [0002. Make `cavalry` the one crate users touch, and reserve the crate family](decisions/0002-one-crate-named-cavalry.md)
- [0003. Lay out the workspace: one version, no publishing yet, lints in three places](decisions/0003-lay-out-the-workspace.md)
- [0004. Run agents on branches, with the owner's token out of every agent's job](decisions/0004-run-agents-on-branches.md)
- [0005. Review pull requests in the base repository's context](decisions/0005-review-in-the-base-context.md)
- [0006. Build the documentation gate from mdBook, rustdoc and a link script](decisions/0006-build-the-docs-gate.md)
- [0007. Build publish/subscribe over shared memory first, in five features](decisions/0007-build-ipc-pub-sub-first.md)
- [0008. Adopt Aeron's data frame header, field for field, as version 0](decisions/0008-adopt-aeron-data-frame-header.md)
- [0009. Read and write wire structs with zerocopy](decisions/0009-read-wire-structs-with-zerocopy.md)
- [0010. Validate every peer-written field of a shared-memory layout before using it](decisions/0010-validate-peer-written-shared-memory.md)
- [0011. Pad peer-shared hot fields to 128 bytes](decisions/0011-pad-peer-shared-fields-to-128-bytes.md)
- [0012. Lay out the counters file as Aeron's counter records behind a Cavalry header](decisions/0012-lay-out-the-counters-file.md)
- [0013. Lay out the log buffer as Aeron's three terms and a metadata page](decisions/0013-lay-out-the-log-buffer.md)
- [0014. Lay out the command ring and the broadcast buffer as Agrona's ring buffers behind a Cavalry header](decisions/0014-lay-out-the-command-ring-and-broadcast-buffer.md)
- [0015. Measure the data plane in process, with a baseline per runner](decisions/0015-measure-in-process-with-per-runner-baselines.md)
- [0016. Run the latency gate on a dedicated host](decisions/0016-run-the-latency-gate-on-a-dedicated-host.md)
- [0017. Gate the rows a message passes through, and report the host probes](decisions/0017-gate-the-message-rows-and-report-the-host-probes.md)
- [0018. Commit with a compare-and-swap in the command ring](decisions/0018-commit-with-a-compare-and-swap-in-the-command-ring.md)
