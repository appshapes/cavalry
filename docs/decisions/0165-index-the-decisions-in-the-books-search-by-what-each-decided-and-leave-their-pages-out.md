# 0165. Index the decisions in the book's search by what each decided, and leave their pages out

- **Status:** Accepted
- **Date:** 2026-11-25
- **Invariants affected:** 3, 6
- **Supersedes:** ADR 0120 on two points. It refused to leave the decisions out of
  the search: their own pages are now out, with pages made from them in their place.
  And its Revisit clause for what is indexed, a file size that nobody was watching,
  gives way to the early bound below. ADR 0120's setting, one search document for
  each page, stands.

## Context

`just docs` builds the book and denies every warning (ADR 0006), and mdBook warns
when its search index passes 10,000,000 bytes by its own count. ADR 0120 met that
warning, set the one setting that shrinks the index, one search document for each
page, and left the file at 7.3 MB. It is dated 2026-11-13. Twelve days later, on
`master` at cfe40bc2, the file is 10,006,987 bytes, about 24 KB under the warning by
mdBook's count. Work item 469's change, with one more ADR, is over it: `search index
is very large (10022521 bytes)`, and `just docs` exits 1. The next pull request with
an ADR fails the gate, whoever opens it.

The decisions are what grew. The commit that added ADR 0120 has 119 of them and
`master` has 164, about four more a day, and one adds about 46 KB to the file: work
item 469's is 12,208 bytes and added 46,576. With the pages under `docs/decisions/`
left out of the search, `master`'s file is 3,365,402 bytes: two thirds of the index
is the decisions.

ADR 0120 named this day. Its Revisit clause reads "when the file
`target/book/searchindex-<hash>.js` that `just docs` writes passes 9,500,000 bytes".
The file passed that at `master`'s commit 8387cba0, nine decisions ago, and no
session saw it: the clause is a sentence in one of 164 files, and its trigger is a
size that only a build shows, in a gate that passed.

ADR 0120 also refused the two quick ways out. Letting the warning pass leaves a file
that every reader's first search loads whole, with nothing to bound it: that refusal
holds as it was argued. Leaving the decisions out of the search takes away what a
stranger searches the book for, why a decision was made (invariant 6): that holds
too when nothing is put in their place, which is all ADR 0120 weighed.

## Decision

**The book's search indexes each decision by its number, its title and its Decision
section, on pages made from the decisions at every build, and no longer indexes the
decisions' own pages.**

- `book.toml` leaves every chapter under `decisions` out of the search
  (`[output.html.search.chapter]`). The pages stay in the book and in its contents,
  and `just docs` checks their links as before.
- A preprocessor, `scripts/decisions-index.py`, which mdBook runs at every build,
  gives the chapter `docs/reference/decisions.md` one page below it for each 25
  numbers: "Decisions 0001 to 0025, by what each decided", and so on. A page holds,
  for each of its decisions, a heading with the number and the title, a link to the
  decision's page, and the Decision section word for word, its relative links
  rewritten for the page it is now on. A decision is on the page of its number for
  good, and a page never holds more than 25.
- **None of those pages is in the repository.** They are made from the decisions
  each time, so they cannot fall behind one, no pull request has a generated file to
  bring up to date, and two pull requests that each add a decision meet in no new
  file. A decision with no Decision section stops the build. The page they go below,
  `docs/reference/decisions.md`, is in the repository and is generated too, by the
  same script's `--write`: its text does not change with the decisions.
- **`just docs` checks the built book** with the same script, `--check`, last of its
  steps. The check reads the built pages and not the Markdown. For every file under
  `docs/decisions/` it fails unless the decision's entry on its generated page holds
  the same text and the same links as the Decision section of the decision's own
  built page, unless the search index holds a document for that generated page with
  the decision's number and title in it, and when a decision's own page is in the
  index again. So the faults this change could have fail a gate: a decision that
  `docs/SUMMARY.md` does not list, a preprocessor that did not run or read the wrong
  section, a link it did not rewrite, the setting removed.
- **The early bound.** The check prints the index file's size on every run and fails
  when it passes 7,000,000 bytes, 3,000,000 under mdBook's warning. The failure says
  that nothing is wrong with the change that met it and names two things to do in
  that change: add a work item tagged `cavalry` and `decision`, "Decide what the
  book's search indexes", and raise the bound to 8,500,000, by one line that the
  message gives. That raise is the only one. A change that passes 8,500,000 is told
  that it cannot land until the work item's decision has.

## Alternatives considered

- **The first paragraph of each Decision section, not the whole section.** The
  template asks for a decision a reader can stop at in one paragraph. The file is
  then 3,846,696 bytes where the whole sections give 4,680,740, and a decision adds a
  quarter as much. It lost because the decisions as written do not fit a paragraph:
  the sections are 642,592 bytes and their first paragraphs 156,547, so three
  quarters of what was decided would leave the search, the names of the functions,
  codes and files a reader searches for among it. It is the first thing the work
  item of the early bound weighs again.
- **Generated pages in the repository, held by a `--check`, as
  `docs/reference/config.md` is.** It lost to what ADR 0075 found for the changelog:
  a file that every pull request with a decision appends to is a file in which they
  all meet, and each would regenerate it after every merge of `master`.
- **One generated page for all the decisions.** It lost to invariant 3: its HTML
  would be 0.9 MB today and grow with every decision. With 25 to a page the largest
  page is 195,438 bytes. The number of decisions to a page changes the index little:
  4,874,816 bytes at 10, 4,680,740 at 25, 4,582,067 at 50 and 4,499,980 at 100.
- **A check that reads the Markdown: every word of a decision's Decision section is
  in its page's search document.** It was built first. It lost to an independent
  read, which found that it failed honest Markdown (an HTML entity, emphasis inside a
  word, a footnote's label) with a message its author could not act on, and passed
  what the search would not find, since it shared the preprocessor's reading of the
  section. Comparing the two built pages asks mdBook, not a second reader of
  Markdown, what a section says.
- **Let the warning pass, with a bound of our own in its place.** Refused again, as
  in ADR 0120: the file was growing by about 0.2 MB a day.
- **Leave the decisions out of the search and put nothing in their place.** Refused
  again: invariant 6.
- **A Revisit clause for the next crossing.** It is what failed here. A clause is read
  when someone thinks to read it; a gate's failure is read by the session that
  caused it.
- **Warn at the early bound and pass.** A line in the output of a gate that passed is
  where the size already was. The early bound fails, so that it is read, and its
  message gives a way through that takes minutes, so that the change it stops is not
  held for a design.

## Evidence

mdBook 0.5.4 on a macOS laptop, the file `searchindex-<hash>.js` of each build. "This
change" is its tree without this ADR's own file, so with `master`'s 164 decisions.
Work item 469's ADR is the file of that work item's commit e6128115, 12,208 bytes,
whose Decision section is 2,136 bytes.

| Tree | File, bytes |
| --- | ---: |
| `master` at cfe40bc2 | 10,006,987 |
| the same with work item 469's ADR | 10,053,563 |
| `master`, the decisions out of the search and nothing in their place | 3,365,402 |
| this change | 4,680,740 |
| this change with work item 469's ADR | 4,689,063 |
| this change with first paragraphs in place of whole sections | 3,846,696 |

mdBook's own count for this change's index, the JSON before the file's escaping, is
4,664,717 bytes. One more decision adds 8,323 bytes where it added 46,576: 3.9 bytes
of index for each byte of its Decision section. The last twenty decisions' sections
are 3,351 bytes each at the mean, so about 13 KB each.

On that book the check passes for all 164 decisions: each entry's text and links are
those of its own page's Decision section. The decisions' files are 2,283,509 bytes
and their Decision sections 642,592, which is 28%.

The preprocessor's interface was read from mdBook 0.5.4 itself, with a made-up book:
it is given `[context, book]` as JSON, the book's chapters under `items` with each
chapter's text, and a chapter that the preprocessor adds below another is rendered,
listed in the contents and indexed. `mdbook test` runs it too, with the renderer
named `test`, and is given the book unchanged. A preprocessor that fails when mdBook
asks it `supports html` is skipped with nothing said; the check then fails, since the
built book has none of the generated pages.

## Consequences

- **What the book's search no longer finds.** A word that a decision has only outside
  its title and its Decision section: in its Context, its Alternatives considered,
  its Evidence, its Consequences or its References. That is most of a decision's
  text; the Decision sections are 28% of the files. A reader finds such a word in the
  files, `git grep -n -i "<words>" -- docs/decisions/` in a checkout or the search
  of the repository's site, and `docs/reference/decisions.md` says so. A result for
  a decision names a page of 25 decisions and opens it with the words marked; the
  decision's own page is one link further.
- **What the check does not show.** That the search's own matching finds a given
  word: mdBook's search splits a text at spaces and hyphens, so a word that a
  section has only inside a path, `spec/vectors/cluster.json`, is found by the path's
  beginning and not by `cluster`. That was so of the decisions' own pages as well.
- **The room.** The file is 2.3 MB under the early bound: about 180 decisions of the
  size of the last twenty, seven weeks at four a day. From the early bound to its
  raised value is 1.5 MB, about 115 decisions and four weeks, for the work item it
  asks for. The pages outside the decisions grow too, and count against the same
  room.
- **The next crossing** is seen by the session whose change passes 7,000,000 bytes,
  as a failure of `just docs` with the two steps in it, and by every session before
  that as the size in the gate's last line. That change is stopped for the minutes
  its two steps take.
- **A Decision section is now shown on two pages**, so it cannot use what Markdown
  takes from elsewhere in its file: a footnote, or a link whose target is defined
  outside the section. The check names the decision and says how to write it. No
  decision uses either today.
- We maintain a preprocessor and a reader of the built pages and of the search
  index's file, tied to mdBook 0.5.4's forms, which the `justfile` and CI pin
  together. `mdbook build` and `mdbook serve` need `python3` on the path.
- `CLAUDE.md`'s gate 6 and `.claude/rules/docs.md` name the decisions among the
  sources that reference pages are generated from.

Revisit: when mdBook can split the search by heading for one chapter and by page for
another, for results that name a decision and not a page of them; when the early
bound's work item is added, for what is indexed.

## References

- Work item 474, which carried this decision, and work item 469, whose ADR met the
  warning.
- [ADR 0120](0120-index-the-books-search-by-page-so-that-its-index-stays-under-mdbooks-warning.md),
  which this supersedes on two points; [ADR 0006](0006-build-the-docs-gate.md), the
  gate; [ADR 0075](0075-keep-each-changes-changelog-entry-in-a-file-of-its-own.md), on
  files that every change appends to.
- [`docs/reference/decisions.md`](../reference/decisions.md), the page the generated
  ones go below; `scripts/decisions-index.py`, `scripts/decisions-index-test.py`,
  `book.toml`, and the `docs` recipe of the `justfile`.
