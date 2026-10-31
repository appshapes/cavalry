# Summary

The four kinds of page, in the order `.claude/rules/docs.md` defines them. Every page
under `docs/` is listed here; `just lint-docs` fails on one that is not.

# Tutorials

- [Publish and subscribe in ten minutes](tutorials/publish-and-subscribe.md)

# Explanation

- [Why Cavalry, and why not Aeron](explanation/why-cavalry.md)
- [Should ZigBolt's design inform Cavalry's?](explanation/should-zigbolt-inform-cavalry.md)

# How-to

- [Ask the agent to make a change](how-to/ask-the-agent.md)
- [Implement a feature, from card to commit](how-to/implement-a-feature.md)
- [Run the bench and compare it with the baseline](how-to/run-the-bench.md)
- [Run a driver and read doctor](how-to/run-the-driver.md)
- [Record a stream to the durable log](how-to/record-a-stream.md)
- [Replay a recording into a subscription](how-to/replay-a-recording.md)
- [Set up a bench host for the latency gate](how-to/set-up-a-bench-host.md)
- [Send the weekly release-notes email](how-to/send-the-release-notes.md)

# Reference

- [Data frame header](reference/frame.md)
- [Counters file](reference/counters-file.md)
- [Counters](reference/counters.md)
- [Log buffer](reference/log-buffer.md)
- [Command ring](reference/command-ring.md)
- [Broadcast buffer](reference/broadcast-buffer.md)
- [Client protocol](reference/client-protocol.md)
- [Durable log](reference/durable-log.md)
- [Configuration](reference/config.md)
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
- [0019. Adopt Aeron's control protocol as the client protocol](decisions/0019-adopt-aeron-control-protocol-as-the-client-protocol.md)
- [0020. Configure by environment variable, and register every key in config.toml](decisions/0020-configure-by-environment-variable.md)
- [0021. Shape the client API after Aeron's, with a conductor thread and no lock](decisions/0021-shape-the-client-api-after-aerons.md)
- [0022. Load a publication's producer position at most once per millisecond](decisions/0022-load-the-producer-position-once-per-millisecond.md)
- [0023. Build the durable log and replay second, in four features](decisions/0023-build-the-durable-log-second.md)
- [0024. Lay out the durable log as a fixed-slot catalog, Aeron's segment files and a time index](decisions/0024-lay-out-the-durable-log.md)
- [0025. Record a stream in the driver, with Aeron Archive's commands in the client protocol](decisions/0025-record-a-stream-in-the-driver.md)
- [0026. Email the week's changes to a list, drafted by an agent and gated before it is sent](decisions/0026-email-the-weeks-changes.md)
- [0027. Pre-touch every shared mapping when it is made, with no key to turn it off](decisions/0027-pre-touch-shared-mappings-at-map-time.md)
- [0028. Stamp bench samples with the time stamp counter, calibrated once](decisions/0028-stamp-bench-samples-with-the-time-stamp-counter.md)
- [0029. Fail a gated row only beyond one clock step as well as beyond 5 percent](decisions/0029-fail-a-gated-row-only-beyond-one-clock-step.md)
- [0030. Replay a recording into a subscription, with Aeron Archive's replay commands](decisions/0030-replay-a-recording-into-a-subscription.md)
- [0031. Truncate, purge and delete a stopped recording with Aeron Archive's commands](decisions/0031-truncate-purge-and-delete-a-stopped-recording.md)
- [0032. Load a publication's producer position at most once every two milliseconds](decisions/0032-load-the-producer-position-every-two-milliseconds.md)
- [0033. Record a bench host's baseline from every process it has run at one tree](decisions/0033-record-a-bench-baseline-from-every-process-at-one-tree.md)
- [0034. Run the latency gate's second host on grip and the soak host on kip](decisions/0034-run-the-gate-on-grip-and-the-soak-on-kip.md)
- [0035. End a replay with nothing to play at once, without a segment file](decisions/0035-end-a-replay-with-nothing-to-play-at-once.md)
- [0036. Hand the replayer a recording's stop and bound the replay publications](decisions/0036-hand-the-replayer-the-stop-and-bound-the-replay-publications.md)
- [0037. Read the clock step from the counter's jump when most readings show no advance](decisions/0037-read-a-coarse-counters-step-from-its-jump.md)
- [0038. Repair a retention command's slot after a failed catalog write as after a file error](decisions/0038-repair-a-retention-commands-slot-after-a-failed-catalog-write.md)
