# 0001. Record decisions as ADRs in the repository

- **Status:** Accepted
- **Date:** 2026-10-10
- **Invariants affected:** 6
- **Supersedes:** none

## Context

Cavalry is built by a widely distributed team working asynchronously, with a large
share of the code written by coding agents. Neither group can walk over to a desk
and ask why something is the way it is. Aeron, the system Cavalry displaces, keeps
much of its reasoning in the heads of a small team and sells it back as training and
consulting. Cavalry's position is that a stranger can understand the system from the
repository alone (`CLAUDE.md`, invariant 6).

## Decision

Every decision that a new team member would ask "why" about is recorded as an
Architecture Decision Record in `docs/decisions/`, numbered, copied from
`0000-template.md`. A decision discussed in chat, on a call or in a pull request
thread does not exist until an ADR records it. Accepted ADRs are immutable except
for their Status line; a change of mind is a new ADR that supersedes the old one.

An ADR is required for any change to `spec/`, the on-disk log format, the C ABI, a
dependency in a data-plane crate, or an invariant in `CLAUDE.md`.

## Alternatives considered

- **Design docs in a wiki or Notion.** Lost: not versioned with the code, not
  reviewable in the same PR, invisible to agents working from the repository.
- **Rationale in code comments only.** Lost: comments explain a line, not a choice
  between alternatives, and they do not record what was rejected.
- **Decisions in PR descriptions.** Lost: PRs are hard to find later and are
  written for the reviewer of that change, not for the reader two years on.

## Evidence

None measured. This is a process decision made before code exists.

## Consequences

Easier: onboarding, agent context, auditing why an invariant exists. Harder: every
protocol or format change carries a writing step before the code step. We revisit
this if ADRs stop being read, which `just lint-docs` can detect by checking that
explanation pages link to them.

## References

- `CLAUDE.md`, invariant 6 and the Documentation rules
- `.claude/rules/docs.md`
