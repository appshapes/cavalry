# 0120. Index the book's search by page, so that its index stays under mdBook's warning

- **Status:** Accepted
- **Date:** 2026-11-13
- **Invariants affected:** 6
- **Supersedes:** none. Adds one setting to the book that ADR 0006's gate builds.

## Context

`just docs` builds the mdBook from `docs/` and denies every warning (ADR 0006). The
book has a search, and mdBook writes its index as one file,
`target/book/searchindex-<hash>.js`, that a reader's browser loads whole at the first
search. The file is the index as JSON, escaped into a JavaScript string. mdBook
0.5.4 warns when the JSON passes 10,000,000 bytes: "search index is very large". It
counts the JSON before the escaping, so the file is larger than mdBook's count, by
about 22 KB here (Evidence).

On `master` at d3482b24 mdBook counts 9,919,553 bytes, 80,447 under the warning, and
the file is 9,941,257 bytes. The next change to add a page went over: with the fifth
change of work item 229, an ADR and a section of `spec/cluster.md`, mdBook counts
10,003,716 bytes, warns, and `just docs` exits 1 on a change that broke nothing.
From here every change that adds documentation fails the gate, whoever makes it.

What the index holds, measured on `master`. mdBook makes one search document for
each heading down to the third level, 1,142 of them, of which about three in four
are the sections of the ADRs: an ADR has seven headings, and there are 117 ADRs.
Of the 9.92 MB that mdBook counts, 2.64 MB is the stored text of the documents. Two
inverted indexes, which list for each word every document that holds it, are 7.04
MB: 3.47 MB for the bodies, and 3.57 MB for the breadcrumbs, a section's path of
titles. mdBook 0.5.4 indexes each section's body under its breadcrumbs as well, so
the breadcrumbs index lists every word of the bodies again. A word in five sections
of one ADR is listed five times in each of the two, once for each section. So the
two indexes grow with the number of documents the pages are split into, not only
with the text. The other 0.24 MB is the titles' own index and each document's URL
and word counts.

## Decision

The book's search makes one document for each page: `heading-split-level = 1` under
`[output.html.search]` in `book.toml`. A search result names a page and opens it
with the words highlighted. On `master` mdBook then counts 7,302,584 bytes for 148
documents, 2.7 MB under the warning, in a file of 7,323,816 bytes.

## Alternatives considered

- **`heading-split-level = 2`.** Lost: an ADR's sections are second-level headings,
  so it saves 1.2%, a file of 9,819,187 bytes, and the next few pages go over again.
- **Let the gate pass this one warning.** Lost: the warning is right. A reader's
  browser loads the whole file, and with a document for each heading the one change
  that crossed the line added 84,403 bytes to it, with nothing to bound what the
  next ones add. A weakened gate also has to be remembered.
- **Leave the decisions out of the search.** mdBook can switch a chapter's
  indexing off. Lost: why a decision was made is what a stranger searches the book
  for (invariant 6).
- **Shorter titles in `docs/SUMMARY.md`.** The breadcrumbs repeat them. Lost: the
  titles are a small part of the index. On `master` the stored titles and
  breadcrumbs are 126,268 bytes and the titles' own index 87,249, and the breadcrumbs
  index lists 120,589 pairs of a word and a document against the bodies' 117,252:
  the titles add 3,337. Shorter titles would stop the contents list saying what each
  of 117 decisions is, to shrink a file a setting shrinks more.
- **Two books, or the reference pages outside the book.** Lost: one book and one
  search is what the reader of invariant 6 is given.

## Evidence

mdBook 0.5.4, on a macOS laptop, each index built with
`MDBOOK_OUTPUT__HTML__SEARCH__HEADING_SPLIT_LEVEL=<n> mdbook build -d <a scratch
directory>`:

| Tree | Split level | File, bytes | Documents | mdBook warns |
| --- | ---: | ---: | ---: | --- |
| `master` at d3482b24 | 3, the default | 9,941,257 | 1,142 | no |
| `master` at d3482b24 | 2 | 9,819,187 | | no |
| `master` at d3482b24 | 1 | 7,323,816 | 148 | no |
| with work item 229's fifth change | 3 | 10,025,660 | 1,150 | yes |
| with work item 229's fifth change | 1 | 7,379,733 | | no |

The warning's own count for the fourth row is 10,003,716 bytes. The file is
`window.search = Object.assign(window.search, JSON.parse('…'));`, 61 bytes around the
JSON, and inside the quotes the JSON has a backslash before each `\` and `'`.
mdBook's count is the JSON before that escaping.

The parts of `master`'s index at levels 3 and 1, each one the length of that part of
the JSON. The parts add up to mdBook's count, and the count, the escapes and the 61
bytes add up to the file:

| Part of the index | Path in the JSON | Level 3, bytes | Level 1, bytes |
| --- | --- | ---: | ---: |
| Stored text of the documents | `.index.documentStore.docs` | 2,643,237 | 2,506,936 |
| Inverted index of the bodies | `.index.index.body` | 3,470,779 | 2,356,210 |
| Inverted index of the breadcrumbs | `.index.index.breadcrumbs` | 3,567,588 | 2,360,173 |
| Inverted index of the titles | `.index.index.title` | 87,249 | 51,702 |
| Each document's URL | `.doc_urls` | 98,624 | 20,305 |
| Each document's word counts | `.index.documentStore.docInfo` | 51,629 | 6,812 |
| The options, version and names of fields | everything else | 447 | 446 |
| mdBook's count, the JSON | `.` | 9,919,553 | 7,302,584 |
| A backslash before each `\` and `'` | | 21,643 | 21,171 |
| The 61 bytes around the JSON | | 61 | 61 |
| The file | | 9,941,257 | 7,323,816 |

These were measured on Linux with mdBook 0.5.4 and jq 1.7.1, from books that `just
docs` built from `master`'s `docs/`: with `master`'s `book.toml` for level 3, and with
this change's for level 1. Both files are the size, to the byte, that the table above
gives for them on macOS. The JSON comes out of the file with:

```sh
sed -e "s/^window\.search = Object\.assign(window\.search, JSON\.parse('//" \
    -e "s/'));\$//" -e 's/\\\(.\)/\1/g' target/book/searchindex-*.js > index.json
```

`jq -cj . index.json | cmp - index.json` prints nothing: jq writes the JSON back byte
for byte. So a part's length is, for the bodies' index,
`jq '.index.index.body | tojson | utf8bytelength' index.json`, and the same with
each path above.

The breadcrumbs index lists every word of the bodies. At both levels, every document
that the bodies' index lists for a word, the breadcrumbs index lists for that word
too, and this prints 0, the number of pairs of a word and a document that the
bodies' index has and the breadcrumbs index lacks:

```sh
jq '.index.index as $i | [$i.body.root | paths(type == "object" and has("docs"))]
  | map(. as $p | ($i.body.root | getpath($p + ["docs"]) | keys)
      - ($i.breadcrumbs.root | getpath($p + ["docs"]) // {} | keys) | length)
  | add' index.json
```

At level 1 the word `cgroup` is in no title, yet
`jq -c '.index.index.breadcrumbs.root.c.g.r.o.u.p.docs | keys' index.json` prints
the same seven documents as the same path under `.index.index.body`. A section's
stored breadcrumbs are only its titles: 102,681 bytes for all 1,142 documents at
level 3.

`just docs` at this change: exit 0, and no line of mdBook's output is a warning.

## Consequences

Easier: a change that adds a page passes `just docs` again, with room for about
fifty changes of the size of the last two: this ADR adds 49,778 bytes to the file,
and work item 229's fifth change 55,917. A reader's first search loads a quarter
less.

Harder: a result no longer names the heading the words are under. On a long page,
the cluster's reference page above all, the reader lands at its top and finds the
place by the highlight or the browser's own find. The index still grows with the
book, and reaches the warning again.

Revisit, for what is indexed and how the book is divided: when the file
`target/book/searchindex-<hash>.js` that `just docs` writes passes 9,500,000 bytes.
The measure is the file, because `ls -l target/book/searchindex-*.js` shows its size.
mdBook's count, which the warning compares, is smaller, so the file passes 9,500,000
bytes first. Revisit, for section results on the reference pages: when mdBook can
split by heading for one chapter and by page for another.

## References

- The Brigade Board's work item 229, whose fifth change, pull request 280, met the
  warning
- ADR 0006; `book.toml`; the `docs` recipe of the `justfile`
- mdBook's guide, "Configuration", `[output.html.search]`
  (<https://rust-lang.github.io/mdBook/format/configuration/renderers.html#outputhtmlsearch>)
