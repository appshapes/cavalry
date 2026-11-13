# 0120. Index the book's search by page, so that its index stays under mdBook's warning

- **Status:** Accepted
- **Date:** 2026-11-13
- **Invariants affected:** 6
- **Supersedes:** none. Adds one setting to the book that ADR 0006's gate builds.

## Context

`just docs` builds the mdBook from `docs/` and denies every warning (ADR 0006). The
book has a search, and mdBook writes its index as one file that a reader's browser
loads whole at the first search. mdBook 0.5.4 warns when that file passes
10,000,000 bytes: "search index is very large".

On `master` at d3482b24 the index is 9,941,257 bytes, 58,743 under the warning. The
next change to add a page went over: with the fifth change of work item 229, an ADR
and a section of `spec/cluster.md`, the index is 10,003,716 bytes, mdBook warns, and
`just docs` exits 1 on a change that broke nothing. From here every change that adds
documentation fails the gate, whoever makes it.

What the index holds, measured on `master`. mdBook makes one search document for
each heading down to the third level, 1,142 of them, of which about three in four
are the sections of the ADRs: an ADR has seven headings, and there are 117 ADRs.
The stored text is 2.56 MB. The inverted index of the bodies is 4.04 MB, and the
inverted index of the breadcrumbs, a section's path of titles, is 4.14 MB: each of
an ADR's seven sections repeats the ADR's title, and the titles are sentences.

## Decision

The book's search makes one document for each page: `heading-split-level = 1` under
`[output.html.search]` in `book.toml`. A search result names a page and opens it
with the words highlighted. The index is 7,323,816 bytes on `master`, 148
documents, and 2.7 MB under the warning.

## Alternatives considered

- **`heading-split-level = 2`.** Lost: an ADR's sections are second-level headings,
  so it saves 1.2%, 9,819,187 bytes, and the next few pages go over again.
- **Let the gate pass this one warning.** Lost: the warning is right. A reader's
  browser loads the whole index, and with a document for each heading the one
  change that crossed the line added 84,403 bytes to it, with nothing to bound what
  the next ones add. A weakened gate also has to be remembered.
- **Leave the decisions out of the search.** mdBook can switch a chapter's
  indexing off. Lost: why a decision was made is what a stranger searches the book
  for (invariant 6).
- **Shorter titles in `docs/SUMMARY.md`.** The breadcrumbs repeat them. Lost: the
  contents list would stop saying what each of 117 decisions is, to shrink a file a
  setting shrinks more.
- **Two books, or the reference pages outside the book.** Lost: one book and one
  search is what the reader of invariant 6 is given.

## Evidence

mdBook 0.5.4, on a macOS laptop, each index built with
`MDBOOK_OUTPUT__HTML__SEARCH__HEADING_SPLIT_LEVEL=<n> mdbook build -d <a scratch
directory>`:

| Tree | Split level | Index, bytes | Documents | mdBook warns |
| --- | ---: | ---: | ---: | --- |
| `master` at d3482b24 | 3, the default | 9,941,257 | 1,142 | no |
| `master` at d3482b24 | 2 | 9,819,187 | | no |
| `master` at d3482b24 | 1 | 7,323,816 | 148 | no |
| with work item 229's fifth change | 3 | 10,025,660 | 1,150 | yes |
| with work item 229's fifth change | 1 | 7,379,733 | | no |

The warning's own count for the fourth row is 10,003,716 bytes: mdBook measures the
index before it wraps it in the file. At level 3 on `master` the stored text is
2,557,266 bytes and the inverted indexes 4,035,056 for the bodies, 4,141,972 for the
breadcrumbs and 104,252 for the titles; at level 1 they are 2,477,907, 2,752,748,
2,757,011 and 62,247.

`just docs` at this change: exit 0, and no line of mdBook's output is a warning.

## Consequences

Easier: a change that adds a page passes `just docs` again, with room for some
fifty to a hundred changes of the size of the last two: this ADR adds 26,904 bytes
to the index, and work item 229's fifth change 55,917. A reader's first search
loads a quarter less.

Harder: a result no longer names the heading the words are under. On a long page,
the cluster's reference page above all, the reader lands at its top and finds the
place by the highlight or the browser's own find. The index still grows with the
book, and reaches the warning again.

Revisit: when the index is within 0.5 MB of 10,000,000 bytes again, for what is
indexed and how the book is divided; when mdBook can split by heading for one
chapter and by page for another, for section results on the reference pages.

## References

- The Brigade Board's work item 229, whose fifth change, pull request 280, met the
  warning
- ADR 0006; `book.toml`; the `docs` recipe of the `justfile`
- mdBook's guide, "Configuration", `[output.html.search]`
  (<https://rust-lang.github.io/mdBook/format/configuration/renderers.html#outputhtmlsearch>)
