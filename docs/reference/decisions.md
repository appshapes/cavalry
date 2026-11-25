# Decisions, by what each decided

Every decision of Cavalry is a page of its own under "Decisions" in the contents, an
ADR (architecture decision record): its context, the decision, the alternatives that
lost, the evidence and the consequences. The pages below this one hold the Decision
section of each, 25 numbers to a page, and are made from the decisions at every build
by `scripts/decisions-index.py`, so none of them is in the repository and none can
fall behind.

They are how the book's search finds a decision. The search does not index the
decisions' own pages: with them in, its index was past the size at which a browser's
first search is slow and mdBook, the tool that builds this book, warns
([ADR 0165](../decisions/0165-index-the-decisions-in-the-books-search-by-what-each-decided-and-leave-their-pages-out.md)).
So a search finds a decision by its number, by its title and by the words of its
Decision section, and its result opens one of the pages below, where a link under the
decision's heading opens its page.

A word that a decision has only in its Context, its Alternatives considered, its
Evidence or its Consequences is not in the search. Search the files for it instead, in
a checkout or on the repository's site:

```sh
git grep -n -i "the words" -- docs/decisions/
```

<!-- decisions-index: scripts/decisions-index.py writes the list of pages here -->
