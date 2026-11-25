- The book's search now finds a decision by its number, its title and the words of its
  Decision section, on pages under "Decisions, by what each decided" that the build
  makes from the decisions, 25 to a page, and it no longer indexes the decisions' own
  pages. A word that a decision has only in its context, its alternatives, its
  evidence or its consequences is found in the files instead, with `git grep`. The
  search index is 4.7 MB where it was 10.0 MB, at the size where mdBook warns and
  `just docs` fails, and one more decision adds 8 KB where it added 46 KB
  ([Decisions, by what each decided](docs/reference/decisions.md);
  [ADR 0165](docs/decisions/0165-index-the-decisions-in-the-books-search-by-what-each-decided-and-leave-their-pages-out.md)).
- `just docs` now checks the built book's search index: every decision is found
  through those pages, and the index's file is under an early bound of 7,000,000
  bytes, whose failure says what to do.
- A page's "edit" link in the book now opens the page's file: the link had `docs/`
  twice in its path.
