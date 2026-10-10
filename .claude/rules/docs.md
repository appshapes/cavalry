---
paths:
  - "docs/**"
  - "**/README.md"
  - "CHANGELOG.md"
  - "spec/**"
---

# Writing documentation for Cavalry

The reader is a competent engineer who has never seen Aeron, works in another time
zone, and has nobody to ask. Many readers are not native English speakers. Everything
below follows from that.

## Pick the kind of page first

Each page in `docs/` is exactly one of these. If you cannot name the kind, you are
not ready to write the page.

| Kind | Directory | Answers | Shape |
| --- | --- | --- | --- |
| Tutorial | `docs/tutorials/` | "Teach me, by doing" | Numbered steps, a stated time budget, a visible result after every step, no choices offered |
| How-to | `docs/how-to/` | "I need to do X" | Prerequisites, steps, how to verify it worked, link to the troubleshooting page |
| Reference | `docs/reference/` | "What exactly does Y do" | Generated from `spec/`, `counters.toml`, `config.toml`, rustdoc. Do not edit by hand; edit the source |
| Explanation | `docs/explanation/` | "Why is it like this" | The question in the title, the answer first, then the reasoning, then links to the ADRs |
| Decision | `docs/decisions/` | "Why did we choose this" | The ADR template in `docs/decisions/0000-template.md` |

A page that answers two of these questions is split into two pages that link to each
other.

## Style

- Short sentences. One idea per sentence. Active voice.
- Define a term on first use, or link to `docs/glossary.md`. Never both.
- No "simply", "just", "obviously", "of course", or "easy". If it were, the page
  would not exist.
- No idioms, no humour that depends on English, no cultural references.
- Numbers carry units and percentiles: "p99.99 of 41 µs on c7i.4xlarge", never
  "fast".
- Name the exact command, flag, file or counter. "Configure the driver" is not an
  instruction; `cavalry run --shm-size 1g` is.
- Say what the reader will see after a step, so they know whether it worked.
- When a thing has an Aeron equivalent, say so once, in a sentence beginning "In
  Aeron, this is …", and link the glossary entry. Then never mention Aeron again on
  that page.

## Code in docs

- Every code block is run by `just docs`. A block that cannot run is marked
  `ignore` with a comment saying why, and that is rare.
- Examples are complete: imports, setup, the call, the output. The reader copies the
  block and it works.
- A crate README's example is the crate's doctest. Changing one changes the other.

## Glossary

`docs/glossary.md` is one table: Cavalry term, one-sentence definition, Aeron term
(or "none"), link to the reference page. Every term that appears in a public API, a
counter name, a config key or an error message has a row. `just lint-docs` checks
that.

## Decisions (ADRs)

- One decision per file, numbered, `NNNN-short-title.md`, copied from
  `0000-template.md`.
- Status is one of Proposed, Accepted, Superseded by NNNN, Rejected. Accepted ADRs
  are not edited except to change Status.
- Required for any change to `spec/`, the on-disk log format, the C ABI, a
  dependency in a data-plane crate, or an invariant in `CLAUDE.md`. Encouraged for
  anything a new team member would ask "why" about.
- Record the alternatives that lost and the evidence (histograms, measurements,
  links). A decision without alternatives is a preference.
- A conversation in chat, a call or a commit message is not a decision. Write the
  ADR and link the conversation from it.

## Changelog

`CHANGELOG.md` follows Keep a Changelog. Every user-visible change adds a line under
`Unreleased` in the same commit, in the reader's words ("`cavalry doctor` now reports
cgroup CPU throttling"), linking the ADR or docs page where one exists.

## Before committing

1. `just docs` passes with no warnings.
2. `just lint-docs` passes.
3. You read the page once as the reader in the first paragraph. If they would have to
   ask a question, answer it on the page.
