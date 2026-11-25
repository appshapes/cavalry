#!/usr/bin/env python3
"""The book's index of the decisions, and the check of the book's search index (work
item 474; ADR 0165).

The book's search leaves the decisions' own pages out (`book.toml`,
`[output.html.search.chapter]`): they were two thirds of an index at mdBook's warning,
and they are what grows. This script puts what a reader searches a decision for back,
at a tenth of the cost: pages that hold, for each decision, its number, its title and
its Decision section, with a link to the decision's page. It has two uses.

    decisions-index.py                      mdBook's preprocessor (book.toml)
    decisions-index.py supports <renderer>  mdBook's question before it; always yes
    decisions-index.py --check <book>       what `just docs` runs on the built book

**The preprocessor.** mdBook gives it the book on standard input, every chapter with
its text, and takes the book back on standard output. It finds the decisions among the
chapters, by their path under `decisions/` and a name that begins with four digits,
and gives the chapter `reference/decisions.md` one chapter below it for each
`PER_PAGE` numbers, 0001 to 0025 and so on, so that no page grows without bound and
the decisions of a page never change. Nothing it writes is in the repository, so
nothing can fall behind the decisions: the pages are made from them at every build. A
decision with no Decision section stops the build.

**The check.** It reads `searchindex-*.js` of the built book and fails unless: every
decision under `docs/decisions/` is in a generated page's search document by its
number and title and by the words of its Decision section; no decision's own page is
a search document; and the file is under the early bound, `EARLY_BOUND` bytes, which
is set well under the size at which mdBook warns and `just docs` fails. It prints the
file's size on every run. Passing the early bound is not a fault of the change that
passes it: the message says the two things to do, and ADR 0165 why.

Python 3.9 standard library only (ADR 0003).
"""

from __future__ import annotations

import glob
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DECISIONS = os.path.join(ROOT, "docs", "decisions")
INDEX_PATH = "reference/decisions.md"
MARKER = "<!-- decisions-index: scripts/decisions-index.py writes the list of pages here -->"
PER_PAGE = 25
HELP = "see docs/reference/decisions.md and docs/decisions/0165-index-the-decisions-in-the-books-search-by-what-each-decided-and-leave-their-pages-out.md"

# mdBook warns when its count of the index passes this, and `just docs` denies a warning.
MDBOOK_WARNS_AT = 10_000_000
# The early bound on the file `target/book/searchindex-<hash>.js`, and the one value it
# may be raised to, once, by the change that passes it (ADR 0165, "The early bound").
EARLY_BOUND = 7_000_000
RAISED_BOUND = 8_500_000


def fail(what):
    sys.stderr.write(f"decisions-index: {what}\n")
    sys.exit(1)


def decision_section(text, where):
    """The body of a decision's `## Decision` section, without the heading."""
    section = re.search(r"^## Decision[ \t]*\n(.*?)(?=^## |\Z)", text, flags=re.S | re.M)
    if section is None or not section.group(1).strip():
        fail(f"{where} has no `## Decision` section with text in it, and the book's search finds a decision by "
             f"that section; write it as docs/decisions/0000-template.md has it; {HELP}")
    return section.group(1).strip()


def moved(section, file):
    """A Decision section's text as it reads on a page of `reference/`: a relative link
    is written from the decision's own directory, so it gets that directory in front,
    and a link to a heading of the decision's page gets the page."""
    def target(match):
        opening, link = match.group(1), match.group(2)
        if re.match(r"[a-zA-Z][a-zA-Z0-9+.-]*:|/", link):
            return match.group(0)
        if link.startswith("#"):
            return f"{opening}../decisions/{file}{link})"
        return f"{opening}../decisions/{link})"
    return re.sub(r"(\]\()([^)\s]+)\)", target, section)


def chapters(items):
    """Every chapter of the book, the ones below another among them."""
    for item in items:
        chapter = item.get("Chapter") if isinstance(item, dict) else None
        if chapter is not None:
            yield chapter
            yield from chapters(chapter.get("sub_items", []))


def decisions_of(book):
    """`(number, title, file, section)` for each decision among the book's chapters, in
    the order of their numbers."""
    found = []
    for chapter in chapters(book["items"]):
        path = chapter.get("path") or ""
        name = re.fullmatch(r"(\d{4})\. (.+)", chapter.get("name", ""))
        if not path.startswith("decisions/") or name is None or name.group(1) == "0000":
            continue
        file = path[len("decisions/"):]
        found.append((int(name.group(1)), name.group(2), file,
                      moved(decision_section(chapter["content"], f"docs/{path}"), file)))
    found.sort()
    numbers = [number for number, _, _, _ in found]
    if len(set(numbers)) != len(numbers):
        twice = sorted({number for number in numbers if numbers.count(number) > 1})
        fail(f"two decisions in docs/SUMMARY.md have the number {twice[0]:04d}; a decision's number is its own; {HELP}")
    return found


def page_of(number):
    """The first and the last number of the page that holds a decision's number."""
    first = (number - 1) // PER_PAGE * PER_PAGE + 1
    return first, first + PER_PAGE - 1


def page_path(first, last):
    return f"reference/decisions-{first:04d}-{last:04d}.md"


def page_name(first, last):
    return f"Decisions {first:04d} to {last:04d}, by what each decided"


def page_content(first, last, entries):
    out = [f"# {page_name(first, last)}", "",
           "This page is generated at every build from the decisions it names "
           "(`scripts/decisions-index.py`). It holds each one's Decision section and nothing else of it, so "
           "that the book's search finds a decision by what it decided. Follow the link under a heading for "
           "the decision's context, the alternatives that lost, its evidence and its consequences.", ""]
    for number, title, file, section in entries:
        out += [f"## {number:04d}. {title}", "", f"[The page of decision {number:04d}](../decisions/{file}).", "",
                section, ""]
    return "\n".join(out)


def preprocess(book):
    """Give the index chapter its pages, and the list of them where its marker is."""
    index = next((chapter for chapter in chapters(book["items"]) if chapter.get("path") == INDEX_PATH), None)
    if index is None:
        fail(f"the book has no chapter docs/{INDEX_PATH}, which the pages of the decisions go below; list it in "
             f"docs/SUMMARY.md; {HELP}")
    if index["content"].count(MARKER) != 1:
        fail(f"docs/{INDEX_PATH} does not hold the line `{MARKER}` once, which is where the list of pages is "
             f"written; restore it; {HELP}")
    pages = {}
    for entry in decisions_of(book):
        pages.setdefault(page_of(entry[0]), []).append(entry)
    listing, below = [], []
    for at, ((first, last), entries) in enumerate(sorted(pages.items()), start=1):
        held = f"{entries[0][0]:04d}" if len(entries) == 1 else f"{entries[0][0]:04d} to {entries[-1][0]:04d}"
        listing.append(f"- [{page_name(first, last)}]({os.path.basename(page_path(first, last))}): {len(entries)} "
                       f"{'decision' if len(entries) == 1 else 'decisions'}, {held}.")
        below.append({"Chapter": {
            "name": page_name(first, last),
            "content": page_content(first, last, entries),
            "number": (index.get("number") or []) + [at] if index.get("number") is not None else None,
            "sub_items": [],
            "path": page_path(first, last),
            # The page a reader who follows "edit" is given: the one that says these are made.
            "source_path": INDEX_PATH,
            "parent_names": index.get("parent_names", []) + [index["name"]],
        }})
    index["content"] = index["content"].replace(MARKER, "\n".join(listing))
    index["sub_items"] = index.get("sub_items", []) + below
    return book


def tokens(text):
    """A text's runs of letters and digits, lower case: how a search document and a
    Decision section are compared, since mdBook stores a page's text without the marks
    of Markdown and with typographic quotes."""
    return re.findall(r"[a-z0-9]+", text.lower())


def words(markdown):
    """The words a reader would search a Decision section for: the runs of four letters
    or more in its prose. What is in a code span, a code block, a tag or a link's
    target is left out, since each is written another way in a search document."""
    prose = re.sub(r"^```.*?^```[ \t]*$", " ", markdown, flags=re.S | re.M)
    # A code span may run over a line's end, and may be fenced by more than one backtick.
    prose = re.sub(r"(`+).*?\1", " ", prose, flags=re.S)
    prose = re.sub(r"<[^>\n]*>|\]\([^)\s]*\)", " ", prose)
    return sorted({token for token in tokens(prose) if token.isalpha() and len(token) >= 4})


def entries_of(body):
    """`{number: (tokens before, tokens of the entry)}` for a generated page's search
    document: an entry runs from the line the preprocessor writes under its heading,
    "The page of decision NNNN", to the next such line."""
    all_tokens = tokens(body)
    starts = [(at, int(all_tokens[at + 4])) for at in range(len(all_tokens) - 4)
              if all_tokens[at:at + 4] == ["the", "page", "of", "decision"] and re.fullmatch(r"\d{4}", all_tokens[at + 4])]
    found = {}
    for place, (at, number) in enumerate(starts):
        end = starts[place + 1][0] if place + 1 < len(starts) else len(all_tokens)
        found.setdefault(number, (all_tokens[:at], set(all_tokens[at + 5:end])))
    return found


def search_documents(book_directory):
    """The built book's search index: its file, and `{url: body}` for its documents."""
    files = glob.glob(os.path.join(book_directory, "searchindex-*.js")) + \
        glob.glob(os.path.join(book_directory, "searchindex.js"))
    if len(files) != 1:
        fail(f"{book_directory} holds {len(files)} search index files, searchindex-<hash>.js, and one is what "
             f"`mdbook build` writes; run `just docs`, which builds the book before this check")
    with open(files[0], encoding="utf-8") as f:
        text = f.read()
    wrapped = re.fullmatch(r"window\.search = Object\.assign\(window\.search, JSON\.parse\('(.*)'\)\);\s*", text,
                           flags=re.S)
    if wrapped is None:
        fail(f"{files[0]} is not `window.search = Object.assign(window.search, JSON.parse('...'));`, the form "
             f"mdBook 0.5.4 writes and this check reads; a new mdBook needs this reader changed with it "
             f"(the justfile's `docs` recipe names the version)")
    index = json.loads(re.sub(r"\\(.)", r"\1", wrapped.group(1), flags=re.S))
    stored = index["index"]["documentStore"]["docs"]
    urls = index["doc_urls"]
    documents = {}
    for key, document in stored.items():
        # A document's URL names its page and the heading it starts at.
        page = urls[int(key)].split("#", 1)[0]
        documents[page] = documents.get(page, "") + " " + document["body"]
    return files[0], documents


def check(book_directory):
    file, documents = search_documents(book_directory)
    size = os.path.getsize(file)
    own = sorted(url for url in documents if url.startswith("decisions/"))
    if own:
        fail(f"the search index holds {len(own)} of the decisions' own pages, {own[0]} among them; book.toml "
             f"leaves the chapters under `decisions` out of the search, under [output.html.search.chapter], "
             f"and the generated pages are how a decision is found; restore that table; {HELP}")
    files = sorted(glob.glob(os.path.join(DECISIONS, "[0-9][0-9][0-9][0-9]-*.md")))
    count = 0
    for path in files:
        number = int(os.path.basename(path)[:4])
        if number == 0:
            continue
        with open(path, encoding="utf-8") as f:
            text = f.read()
        where = f"docs/decisions/{os.path.basename(path)}"
        title = re.match(r"# (\d{4})\. (.+)\n", text)
        if title is None or int(title.group(1)) != number:
            fail(f"{where} does not begin with `# {number:04d}. <title>`, the heading the index reads; {HELP}")
        first, last = page_of(number)
        url = page_path(first, last)[:-3] + ".html"
        body = documents.get(url)
        if body is None:
            fail(f"the search index has no document for {url}, the generated page that holds decision "
                 f"{number:04d}: is {where} listed in docs/SUMMARY.md, and is the preprocessor in book.toml? {HELP}")
        heading = tokens(f"{number:04d}. {title.group(2)}")
        before, entry = entries_of(body).get(number, ([], set()))
        if before[-len(heading):] != heading:
            fail(f"the search document of {url} does not hold `{number:04d}. {title.group(2)}`, the heading of "
                 f"{where}, with the decision's entry under it: is it listed in docs/SUMMARY.md under that title? "
                 f"{HELP}")
        missing = [word for word in words(decision_section(text, where)) if word not in entry]
        if missing:
            fail(f"the search document of {url} lacks {len(missing)} of the words of {where}'s Decision section, "
                 f"{', '.join(missing[:8])} among them, so a search for them does not find the decision; {HELP}")
        count += 1
    share = 100 * size // MDBOOK_WARNS_AT
    if size > EARLY_BOUND:
        if EARLY_BOUND < RAISED_BOUND:
            fail(f"the book's search index, {os.path.relpath(file, ROOT)}, is {size:,} bytes, past the early bound "
                 f"of {EARLY_BOUND:,}. Nothing is wrong with this change: the bound is set well under the "
                 f"{MDBOOK_WARNS_AT:,} at which mdBook warns and `just docs` fails, so that the crossing is seen "
                 f"while there is room. Do two things in this change. Add a work item tagged `cavalry` and "
                 f"`decision`, \"Decide what the book's search indexes\", with this size in it (`brigade work-item "
                 f"add --tag cavalry --tag decision`). Then set EARLY_BOUND in scripts/decisions-index.py to "
                 f"RAISED_BOUND, {RAISED_BOUND:,}, and name the work item in the commit message; {HELP}")
        fail(f"the book's search index, {os.path.relpath(file, ROOT)}, is {size:,} bytes, past the early bound "
             f"after its one raise, {EARLY_BOUND:,}. The work item that the first crossing added, \"Decide what "
             f"the book's search indexes\", is due before this change lands: the next bound is mdBook's own "
             f"warning at {MDBOOK_WARNS_AT:,}, which stops every change that adds a page; {HELP}")
    print(f"decisions-index: {count} decisions are in the book's search by number, title and the words of their "
          f"Decision sections, none by its own page; the search index is {size:,} bytes, {share}% of the "
          f"{MDBOOK_WARNS_AT:,} at which mdBook warns, and the early bound is {EARLY_BOUND:,}")
    return 0


def main(argv):
    if len(argv) >= 2 and argv[1] == "supports":
        return 0
    if len(argv) == 3 and argv[1] == "--check":
        return check(argv[2])
    if len(argv) != 1:
        sys.stderr.write("usage: decisions-index.py [supports <renderer> | --check <built book's directory>]\n")
        return 2
    _, book = json.load(sys.stdin)
    json.dump(preprocess(book), sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
