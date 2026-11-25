#!/usr/bin/env python3
"""The tests of scripts/decisions-index.py (work item 474; ADR 0165; ADR 0026): the
preprocessor that gives the book its pages of the decisions, and the check that `just
docs` runs on the built book's search index.

- The preprocessor, on a made-up book as mdBook 0.5.4 hands one over: a page for each
  25 numbers below the index chapter, each decision in it by its number, its title, a
  link to its page and its Decision section word for word; the list of pages where the
  marker was; every other chapter as it was. A decision with no Decision section, a
  book without the index chapter or its marker, and a number used twice each stop the
  build with what to do. Run as mdBook runs it, it answers `supports` and turns the
  book on its standard input into the book on its standard output.
- The check, on made-up search indexes written in mdBook's form: it passes when every
  decision is in its page's document by number, title and the words of its Decision
  section, and fails, naming the decision, when the page, the heading or one word is
  missing, when a decision's own page is in the index, and when the file passes the
  early bound, before its one raise and after it.
- `book.toml` names the preprocessor and leaves the chapters under `decisions` out of
  the search.

`just docs` runs the check on the real book, which is the test that every decision of
the repository is found. `just test` runs this through `test-scripts`. Python 3.9
standard library only.
"""

import contextlib
import copy
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "decisions-index.py")
spec = importlib.util.spec_from_file_location("decisions_index", SCRIPT)
index = importlib.util.module_from_spec(spec)
spec.loader.exec_module(index)


def decision(number, title, section, rest="## Consequences\n\nOnly here: aardvark.\n"):
    text = (f"# {number:04d}. {title}\n\n- **Status:** Accepted\n\n## Context\n\nOnly here: wombat.\n\n"
            f"## Decision\n\n{section}\n\n{rest}")
    return {"Chapter": {"name": f"{number:04d}. {title}", "content": text, "number": None, "sub_items": [],
                        "path": f"decisions/{number:04d}-made-up.md", "source_path": f"decisions/{number:04d}-made-up.md",
                        "parent_names": []}}


def book(*decisions, marker=index.MARKER):
    """A book as mdBook 0.5.4 gives it to a preprocessor: chapters and part titles."""
    return {"items": [
        {"Chapter": {"name": "A how-to", "content": "# A how-to\n\nDo it.\n", "number": [1], "sub_items": [],
                     "path": "how-to/a.md", "source_path": "how-to/a.md", "parent_names": []}},
        {"PartTitle": "Reference"},
        {"Chapter": {"name": "Decisions, by what each decided", "content": f"# Decisions\n\nBefore.\n\n{marker}\n",
                     "number": [2], "sub_items": [], "path": "reference/decisions.md",
                     "source_path": "reference/decisions.md", "parent_names": []}},
        {"PartTitle": "Decisions"},
        {"Chapter": {"name": "ADR template", "content": "# NNNN. Short title\n\n## Decision\n\nWhat we are doing.\n",
                     "number": None, "sub_items": [], "path": "decisions/0000-template.md",
                     "source_path": "decisions/0000-template.md", "parent_names": []}},
        *decisions,
    ]}


def refused(test, call, *said):
    """Run `call`, which must exit 1, and hold its message to `said`."""
    with contextlib.redirect_stderr(io.StringIO()) as stderr, test.assertRaises(SystemExit) as raised:
        call()
    test.assertEqual(raised.exception.code, 1)
    for part in said:
        test.assertIn(part, stderr.getvalue())
    test.assertTrue(stderr.getvalue().startswith("decisions-index: "))
    return stderr.getvalue()


class Preprocessor(unittest.TestCase):
    def test_each_25_numbers_are_a_page_below_the_index_chapter(self):
        made = book(decision(26, "Twenty-six", "Do the twenty-sixth thing."), decision(1, "One", "Do the first thing."),
                    decision(25, "Twenty-five", "Do the last thing of the page."), decision(51, "Fifty-one", "Skip a page."))
        before = copy.deepcopy(made)
        after = index.preprocess(made)
        chapter = after["items"][2]["Chapter"]
        pages = [item["Chapter"] for item in chapter["sub_items"]]
        self.assertEqual([page["path"] for page in pages], ["reference/decisions-0001-0025.md",
                                                            "reference/decisions-0026-0050.md",
                                                            "reference/decisions-0051-0075.md"])
        self.assertEqual([page["name"] for page in pages], ["Decisions 0001 to 0025, by what each decided",
                                                            "Decisions 0026 to 0050, by what each decided",
                                                            "Decisions 0051 to 0075, by what each decided"])
        self.assertEqual([page["number"] for page in pages], [[2, 1], [2, 2], [2, 3]])
        for page in pages:
            self.assertEqual(page["parent_names"], ["Decisions, by what each decided"])
            self.assertEqual(page["source_path"], "reference/decisions.md")
            self.assertEqual(page["sub_items"], [])
        # A page holds its decisions in the order of their numbers, whatever the book's.
        first = pages[0]["content"]
        self.assertTrue(first.startswith("# Decisions 0001 to 0025, by what each decided\n\nThis page is generated"))
        self.assertLess(first.index("## 0001. One"), first.index("## 0025. Twenty-five"))
        self.assertIn("## 0001. One\n\n[The page of decision 0001](../decisions/0001-made-up.md).\n\n"
                      "Do the first thing.\n", first)
        self.assertNotIn("Twenty-six", first)
        self.assertIn("## 0026. Twenty-six\n", pages[1]["content"])
        # The list of pages is where the marker was, and the rest of the chapter stays.
        self.assertEqual(chapter["content"],
                         "# Decisions\n\nBefore.\n\n"
                         "- [Decisions 0001 to 0025, by what each decided](decisions-0001-0025.md): 2 decisions, 0001 to 0025.\n"
                         "- [Decisions 0026 to 0050, by what each decided](decisions-0026-0050.md): 1 decision, 0026.\n"
                         "- [Decisions 0051 to 0075, by what each decided](decisions-0051-0075.md): 1 decision, 0051.\n")
        # Nothing else of the book changed: not the other chapters, not the decisions.
        for at in (0, 1, 3, 4, 5, 6, 7, 8):
            self.assertEqual(after["items"][at], before["items"][at])

    def test_a_page_holds_the_decision_section_alone_word_for_word(self):
        section = ("**The rule.** A `client` never does *that*.\n\n### A part\n\n- one\n- two\n\n"
                   "| a | b |\n| --- | --- |\n| 1 | 2 |")
        page = index.preprocess(book(decision(7, "Seven", section)))["items"][2]["Chapter"]["sub_items"][0]["Chapter"]
        self.assertIn(f"[The page of decision 0007](../decisions/0007-made-up.md).\n\n{section}\n", page["content"])
        for elsewhere in ("wombat", "aardvark", "Status", "What we are doing"):
            self.assertNotIn(elsewhere, page["content"])

    def test_the_last_section_of_a_file_and_a_decision_below_another_chapter_are_read(self):
        nested = book()
        last = decision(3, "Three", "The decision is the file's last section.", rest="")
        nested["items"][0]["Chapter"]["sub_items"].append(last)
        page = index.preprocess(nested)["items"][2]["Chapter"]["sub_items"][0]["Chapter"]
        self.assertIn("## 0003. Three\n", page["content"])
        self.assertIn("The decision is the file's last section.", page["content"])

    def test_a_relative_link_of_a_decision_is_written_from_the_pages_directory(self):
        section = ("See [another](0002-other.md), [a page](../how-to/a.md#steps), [here](#evidence), "
                   "[out](https://example.org/x.md), [mail](mailto:a@example.org) and [root](/x.md).")
        page = index.preprocess(book(decision(9, "Nine", section)))["items"][2]["Chapter"]["sub_items"][0]["Chapter"]
        self.assertIn("See [another](../decisions/0002-other.md), [a page](../decisions/../how-to/a.md#steps), "
                      "[here](../decisions/0009-made-up.md#evidence), [out](https://example.org/x.md), "
                      "[mail](mailto:a@example.org) and [root](/x.md).", page["content"])

    def test_a_book_the_pages_cannot_be_made_from_stops_the_build(self):
        no_section = decision(4, "Four", "x")
        no_section["Chapter"]["content"] = "# 0004. Four\n\n## Context\n\nNo decision.\n\n## Consequences\n\nNone.\n"
        refused(self, lambda: index.preprocess(book(no_section)), "docs/decisions/0004-made-up.md has no `## Decision` "
                "section with text in it", "docs/decisions/0000-template.md", "docs/reference/decisions.md")
        empty = decision(5, "Five", "")
        refused(self, lambda: index.preprocess(book(empty)), "docs/decisions/0005-made-up.md has no `## Decision`")
        refused(self, lambda: index.preprocess(book(decision(6, "Six", "a"), decision(6, "Six again", "b"))),
                "two decisions in docs/SUMMARY.md have the number 0006")
        without = book(decision(1, "One", "x"))
        del without["items"][2]
        refused(self, lambda: index.preprocess(without), "the book has no chapter docs/reference/decisions.md",
                "list it in docs/SUMMARY.md")
        refused(self, lambda: index.preprocess(book(decision(1, "One", "x"), marker="no marker")),
                "does not hold the line `<!-- decisions-index:", "once")
        refused(self, lambda: index.preprocess(book(decision(1, "One", "x"), marker=index.MARKER + "\n" + index.MARKER)),
                "does not hold the line `<!-- decisions-index:")

    def test_run_as_mdbook_runs_it(self):
        made = book(decision(1, "One", "Do the first thing."))
        asked = subprocess.run([sys.executable, SCRIPT, "supports", "html"], capture_output=True, text=True)
        self.assertEqual((asked.returncode, asked.stdout, asked.stderr), (0, "", ""))
        given = json.dumps([{"root": "/made/up", "renderer": "html", "mdbook_version": "0.5.4"}, made])
        ran = subprocess.run([sys.executable, SCRIPT], input=given, capture_output=True, text=True)
        self.assertEqual((ran.returncode, ran.stderr), (0, ""))
        self.assertEqual(json.loads(ran.stdout), index.preprocess(copy.deepcopy(made)))
        bad = decision(2, "Two", "x")
        bad["Chapter"]["content"] = "# 0002. Two\n"
        given = json.dumps([{}, book(bad)])
        ran = subprocess.run([sys.executable, SCRIPT], input=given, capture_output=True, text=True)
        self.assertEqual((ran.returncode, ran.stdout), (1, ""))
        self.assertIn("has no `## Decision` section", ran.stderr)
        usage = subprocess.run([sys.executable, SCRIPT, "--check"], capture_output=True, text=True)
        self.assertEqual(usage.returncode, 2)
        self.assertIn("usage: decisions-index.py", usage.stderr)


class Words(unittest.TestCase):
    def test_the_words_are_the_proses_of_four_letters_or_more(self):
        self.assertEqual(index.words("The **Decision** is a rule, RULE and Rules: it's set by the tool's own word, "
                                     "at p99.99 by utf8bytelength and sha256sum."),
                         ["decision", "rule", "rules", "tool", "word"])

    def test_code_tags_and_link_targets_are_left_out(self):
        text = ("Run `cargo build --release` and read [the page](../decisions/0001-elsewhere.md), then\n"
                "<details>fold</details>\n\n```sh\nmdbook serve --hostname\n```\n\nafter.")
        self.assertEqual(index.words(text), ["after", "fold", "page", "read", "then"])
        # A code span that runs over a line's end, and one fenced by two backticks.
        text = "It `claims a\nslice` and `commit`s or `abort`s, with ``a ` tick`` inside, then stops."
        self.assertEqual(index.words(text), ["inside", "stops", "then", "with"])


class Check(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.decisions = os.path.join(directory.name, "docs", "decisions")
        self.book = os.path.join(directory.name, "target", "book")
        os.makedirs(self.decisions)
        os.makedirs(self.book)
        for name, value in (("DECISIONS", self.decisions), ("ROOT", directory.name)):
            patch = mock.patch.object(index, name, value)
            patch.start()
            self.addCleanup(patch.stop)
        self.sections = {1: "The **first** rule, with `code` in it and a leader's answer.",
                         26: "Another page's decision holds zebrafish."}
        self.titles = {1: "Adopt the leader's first rule", 26: "Keep `zebrafish` apart"}
        for number in self.sections:
            with open(os.path.join(self.decisions, f"{number:04d}-made-up.md"), "w", encoding="utf-8") as f:
                f.write(f"# {number:04d}. {self.titles[number]}\n\n## Context\n\nUnsearched: wombat.\n\n## Decision\n\n"
                        f"{self.sections[number]}\n\n## Consequences\n\nNone.\n")
        with open(os.path.join(self.decisions, "0000-template.md"), "w", encoding="utf-8") as f:
            f.write("# NNNN. Short title\n\n## Decision\n\nxylophone\n")
        # As mdBook stores a page: no Markdown marks, typographic quotes.
        self.documents = {
            "reference/decisions-0001-0025.html#decisions-0001-to-0025-by-what-each-decided":
                "This page is generated. 0001. Adopt the leader’s first rule The page of decision 0001. The first "
                "rule, with code in it and a leader’s answer.",
            "reference/decisions-0026-0050.html#decisions-0026-to-0050-by-what-each-decided":
                "0026. Keep zebrafish apart The page of decision 0026. Another page’s decision holds zebrafish.",
            "how-to/a.html#a-how-to": "Do it.",
        }

    def write(self, documents=None, name="searchindex-0a1b2c3d.js", padding=0):
        documents = self.documents if documents is None else documents
        urls = list(documents)
        stored = {str(at): {"id": str(at), "title": "t", "breadcrumbs": "b", "body": documents[url]}
                  for at, url in enumerate(urls)}
        text = json.dumps({"doc_urls": urls, "index": {"documentStore": {"docs": stored}}, "padding": "x" * padding})
        path = os.path.join(self.book, name)
        with open(path, "w", encoding="utf-8") as f:
            f.write("window.search = Object.assign(window.search, JSON.parse('"
                    + text.replace("\\", "\\\\").replace("'", "\\'") + "'));")
        return path

    def check(self):
        with contextlib.redirect_stdout(io.StringIO()) as stdout:
            self.assertEqual(index.check(self.book), 0)
        return stdout.getvalue()

    def test_every_decision_in_its_pages_document_passes_and_the_size_is_printed(self):
        path = self.write()
        size = os.path.getsize(path)
        self.assertEqual(self.check(),
                         f"decisions-index: 2 decisions are in the book's search by number, title and the words of "
                         f"their Decision sections, none by its own page; the search index is {size:,} bytes, 0% of "
                         f"the 10,000,000 at which mdBook warns, and the early bound is 7,000,000\n")

    def test_a_decision_that_the_search_would_not_find_fails_and_is_named(self):
        whole = dict(self.documents)
        page = "reference/decisions-0026-0050.html#decisions-0026-to-0050-by-what-each-decided"
        del self.documents[page]
        self.write()
        refused(self, lambda: index.check(self.book), "no document for reference/decisions-0026-0050.html",
                "decision 0026", "docs/decisions/0026-made-up.md listed in docs/SUMMARY.md",
                "docs/reference/decisions.md")
        self.documents = dict(whole)
        self.documents[page] = "0026. Keep them apart The page of decision 0026. Another page’s decision holds zebrafish."
        self.write()
        refused(self, lambda: index.check(self.book), "does not hold `0026. Keep `zebrafish` apart`, the heading of "
                "docs/decisions/0026-made-up.md")
        # A word of the heading is not a word of the entry: the Decision section's own must be under it.
        self.documents = dict(whole)
        self.documents[page] = "0026. Keep zebrafish apart The page of decision 0026. Another page’s decision holds."
        self.write()
        refused(self, lambda: index.check(self.book), "lacks 1 of the words of docs/decisions/0026-made-up.md's "
                "Decision section, zebrafish among them")
        # Nor is a word of the next decision's entry on the page, or a longer word that holds it.
        self.documents[page] = ("0026. Keep zebrafish apart The page of decision 0026. Another page’s decisions hold "
                                "zebrafish. 0027. Next The page of decision 0027. A decision holds this.")
        self.write()
        refused(self, lambda: index.check(self.book), "lacks 2 of the words of docs/decisions/0026-made-up.md's "
                "Decision section, decision, holds among them")
        # A decision in the wrong page's document is not found where a reader is sent.
        self.documents = dict(whole)
        self.documents["reference/decisions-0001-0025.html#x"] = whole[page]
        self.documents[page] = "nothing"
        self.write()
        refused(self, lambda: index.check(self.book), "reference/decisions-0026-0050.html does not hold `0026.")

    def test_a_decisions_own_page_in_the_index_fails(self):
        self.documents["decisions/0001-made-up.html#0001-adopt"] = "the page itself"
        self.write()
        refused(self, lambda: index.check(self.book), "holds 1 of the decisions' own pages, "
                "decisions/0001-made-up.html among them", "[output.html.search.chapter]")

    def test_a_decision_file_the_index_cannot_read_fails(self):
        self.write()
        with open(os.path.join(self.decisions, "0002-no-heading.md"), "w", encoding="utf-8") as f:
            f.write("# Two\n\n## Decision\n\nx\n")
        refused(self, lambda: index.check(self.book), "docs/decisions/0002-no-heading.md does not begin with "
                "`# 0002. <title>`")

    def test_the_early_bound_fails_with_the_two_things_to_do_and_then_with_the_work_item_due(self):
        path = self.write(padding=3_000)
        size = os.path.getsize(path)
        with mock.patch.object(index, "EARLY_BOUND", size - 1), mock.patch.object(index, "RAISED_BOUND", size + 500):
            said = refused(self, lambda: index.check(self.book),
                           f"target/book/searchindex-0a1b2c3d.js, is {size:,} bytes, past the early bound of {size - 1:,}",
                           "Nothing is wrong with this change", "Add a work item tagged `cavalry` and `decision`",
                           f"set EARLY_BOUND in scripts/decisions-index.py to RAISED_BOUND, {size + 500:,}",
                           "name the work item in the commit message")
            self.assertNotIn("is due before this change lands", said)
        with mock.patch.object(index, "EARLY_BOUND", size - 1), mock.patch.object(index, "RAISED_BOUND", size - 1):
            said = refused(self, lambda: index.check(self.book), "past the early bound after its one raise",
                           "is due before this change lands")
            self.assertNotIn("Nothing is wrong", said)
        with mock.patch.object(index, "EARLY_BOUND", size):
            self.check()

    def test_the_bounds_are_in_order_under_mdbooks_warning(self):
        self.assertLess(index.EARLY_BOUND, index.RAISED_BOUND)
        self.assertLess(index.RAISED_BOUND, index.MDBOOK_WARNS_AT)
        self.assertEqual(index.MDBOOK_WARNS_AT, 10_000_000)

    def test_a_book_with_no_index_file_two_of_them_or_another_form_fails(self):
        refused(self, lambda: index.check(self.book), "holds 0 search index files", "run `just docs`")
        self.write()
        self.write(name="searchindex-ffffffff.js")
        refused(self, lambda: index.check(self.book), "holds 2 search index files")
        os.remove(os.path.join(self.book, "searchindex-ffffffff.js"))
        with open(os.path.join(self.book, "searchindex-0a1b2c3d.js"), "w", encoding="utf-8") as f:
            f.write("Object.assign(window.search, {});")
        refused(self, lambda: index.check(self.book), "is not `window.search = Object.assign(window.search, "
                "JSON.parse('...'));`", "mdBook 0.5.4")


class Book(unittest.TestCase):
    def test_book_toml_names_the_preprocessor_and_leaves_the_decisions_pages_out_of_the_search(self):
        with open(os.path.join(HERE, "..", "book.toml"), encoding="utf-8") as f:
            text = f.read()
        self.assertIn('\n[preprocessor.decisions-index]\ncommand = "python3 scripts/decisions-index.py"\n', text)
        self.assertIn('\n[output.html.search.chapter]\n"decisions" = { enable = false }\n', text)
        self.assertIn("\n[output.html.search]\nheading-split-level = 1\n", text)

    def test_the_index_chapter_is_in_the_summary_and_holds_the_marker_once(self):
        with open(os.path.join(HERE, "..", "docs", "SUMMARY.md"), encoding="utf-8") as f:
            self.assertEqual(f.read().count("](reference/decisions.md)\n"), 1)
        with open(os.path.join(HERE, "..", "docs", "reference", "decisions.md"), encoding="utf-8") as f:
            self.assertEqual(f.read().count(index.MARKER), 1)


if __name__ == "__main__":
    unittest.main()
