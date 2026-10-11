# Summary

The four kinds of page, in the order `.claude/rules/docs.md` defines them. Every page
under `docs/` is listed here; `just lint-docs` fails on one that is not.

# Explanation

- [Why Cavalry, and why not Aeron](explanation/why-cavalry.md)

# How-to

- [Ask the agent to make a change](how-to/ask-the-agent.md)

# Reference

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
