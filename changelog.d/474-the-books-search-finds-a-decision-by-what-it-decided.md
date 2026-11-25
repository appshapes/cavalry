- The book's search now finds a decision by its number, its title and the text of its
  Decision section, on pages under "Decisions, by what each decided" that the build
  makes from the decisions, 25 to a page, and it no longer indexes the decisions' own
  pages. A word that a decision has only in its context, its alternatives, its
  evidence or its consequences is found in the files instead, with `git grep`. The
  search index was 24 KB under the size at which mdBook warns and `just docs` fails,
  and is now under half of it; one more decision adds about a sixth of what it did
  ([Decisions, by what each decided](docs/reference/decisions.md);
  [ADR 0165](docs/decisions/0165-index-the-decisions-in-the-books-search-by-what-each-decided-and-leave-their-pages-out.md)).
- `just docs` now checks the built book's index of the decisions: each decision reads
  on its generated page as its Decision section reads on its own page, text and
  links, the search holds those pages, and the search index's file is under an early
  bound of 7,000,000 bytes, whose failure says what to do.
- A page's "edit" link in the book now opens the page's file: the link had `docs/`
  twice in its path.
