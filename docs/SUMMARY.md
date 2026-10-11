# Summary

The four kinds of page, in the order `.claude/rules/docs.md` defines them. Every page
under `docs/` is listed here; `just lint-docs` fails on one that is not.

# Explanation

- [Why Cavalry, and why not Aeron](explanation/why-cavalry.md)
- [Should ZigBolt's design inform Cavalry's?](explanation/should-zigbolt-inform-cavalry.md)

# How-to

- [Ask the agent to make a change](how-to/ask-the-agent.md)
- [Implement a feature, from card to commit](how-to/implement-a-feature.md)

# Reference

- [Data frame header](reference/frame.md)
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
