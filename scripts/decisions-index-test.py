#!/usr/bin/env python3
"""The tests of scripts/decisions-index.py (work item 474; ADR 0165; ADR 0026): the
preprocessor that gives the book its pages of the decisions, and the check that `just
docs` runs on the built book.

- The preprocessor, on a made-up book as mdBook 0.5.4 hands one over: a page for each
  25 numbers below the index chapter, each decision in it by the number and the title
  of its file's heading, a link to its page and its Decision section word for word, in
  the order of the numbers; the list of pages where the marker was; every other
  chapter as it was. The Decision section is read past code blocks, and its relative
  links are moved with it, outside code alone. A decision without its heading or its
  Decision section, a book without the index chapter or its marker, and a number used
  twice each stop the build with what to do. Run as mdBook runs it, it answers
  `supports`, turns the HTML book on its standard input into the book on its standard
  output, and hands any other renderer's book back as it came.
- The check, on made-up built books: pages in mdBook's HTML and a search index in its
  form. It passes when every decision's entry reads as its own page's Decision section
  does, text and links, and the search index holds the generated page with the
  decision's heading and none of the decisions' own pages. It fails, naming the
  decision, when the entry's text or a link differs, when the entry, a page or the
  heading is missing, when a decision's own page is in the index, when the reference
  page is not the generated one, and when the file passes the early bound, before its
  one raise and after it.
- `book.toml` names the preprocessor and leaves the chapters under `decisions` out of
  the search, the reference page in the repository is the one the script writes, and
  the `docs` recipe runs the check.

`just docs` runs the check on the real book, which is the test that every decision of
the repository is found. `just test` runs this through `test-scripts`. Python 3.9
standard library only.
"""

import contextlib
import copy
import html
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


def decision(number, title, section, rest="## Consequences\n\nOnly here: aardvark.\n", name=None, file=None):
    text = (f"# {number:04d}. {title}\n\n- **Status:** Accepted\n\n## Context\n\nOnly here: wombat.\n\n"
            f"## Decision\n\n{section}\n\n{rest}")
    path = f"decisions/{file or f'{number:04d}-made-up.md'}"
    return {"Chapter": {"name": name or f"{number:04d}. {title}", "content": text, "number": None, "sub_items": [],
                        "path": path, "source_path": path, "parent_names": []}}


def book(*decisions, marker=index.MARKER):
    """A book as mdBook 0.5.4 gives it to a preprocessor: chapters and part titles."""
    return {"items": [
        {"Chapter": {"name": "A how-to", "content": "# A how-to\n\nDo it.\n", "number": [1], "sub_items": [],
                     "path": "how-to/a.md", "source_path": "how-to/a.md", "parent_names": []}},
        {"PartTitle": "Reference"},
        {"Chapter": {"name": index.INDEX_NAME, "content": f"# Decisions\n\nBefore.\n\n{marker}\n",
                     "number": [2], "sub_items": [], "path": "reference/decisions.md",
                     "source_path": "reference/decisions.md", "parent_names": []}},
        {"PartTitle": "Decisions"},
        {"Chapter": {"name": "ADR template", "content": "# NNNN. Short title\n\n## Decision\n\nWhat we are doing.\n",
                     "number": None, "sub_items": [], "path": "decisions/0000-template.md",
                     "source_path": "decisions/0000-template.md", "parent_names": []}},
        *decisions,
    ]}


def pages_of(made):
    return [item["Chapter"] for item in index.preprocess(made)["items"][2]["Chapter"]["sub_items"]]


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
    def test_each_25_numbers_are_a_page_below_the_index_chapter_in_the_order_of_the_numbers(self):
        # The book lists them out of order, as a SUMMARY.md after a merge may.
        made = book(decision(26, "Twenty-six", "Do the twenty-sixth thing."),
                    decision(25, "Twenty-five", "Do the last thing of the page."),
                    decision(1, "One", "Do the first thing."), decision(51, "Fifty-one", "Skip a page."))
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
            self.assertEqual(page["parent_names"], [index.INDEX_NAME])
            self.assertEqual(page["source_path"], "reference/decisions.md")
            self.assertEqual(page["sub_items"], [])
        first = pages[0]["content"]
        self.assertTrue(first.startswith("# Decisions 0001 to 0025, by what each decided\n\nThis page is made"))
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

    def test_a_decision_is_known_by_its_path_and_named_by_its_files_heading(self):
        made = book(decision(7, "The file's own title", "x", name="Seven, as the summary has it"),
                    decision(8, "Eight", "y", file="README.md"),
                    decision(9, "Nine", "z", file="notes/0009-deeper.md"))
        # A file numbered 0000 is the template whatever it is called and whatever its heading.
        made["items"][4]["Chapter"].update(name="0000. A template", content="# 0000. A template\n\n## Decision\n\nxylophone\n")
        pages = pages_of(made)
        self.assertEqual(len(pages), 1)
        self.assertIn("## 0007. The file's own title\n", pages[0]["content"])
        for absent in ("Seven, as", "Eight", "Nine", "xylophone", "0000"):
            self.assertNotIn(absent, pages[0]["content"].replace("0001 to 0025", ""))

    def test_a_page_holds_the_decision_section_alone_word_for_word(self):
        section = ("**The rule.** A `client` never does *that*.\n\n### A part\n\n- one\n- two\n\n"
                   "| a | b |\n| --- | --- |\n| 1 | 2 |")
        page = pages_of(book(decision(7, "Seven", section)))[0]
        self.assertIn(f"[The page of decision 0007](../decisions/0007-made-up.md).\n\n{section}\n", page["content"])
        for elsewhere in ("wombat", "aardvark", "Status", "What we are doing"):
            self.assertNotIn(elsewhere, page["content"])

    def test_the_section_is_read_past_code_blocks(self):
        quoted = decision(3, "Three", "The real one.\n\n```md\n## Not a heading\n# Nor this\n```\n\nStill the decision.\n\n"
                                     "~~~~\n## Nor in a longer fence\n~~~\n~~~~\n\nAnd its end.")
        # The template quoted in the Context, before the section itself.
        quoted["Chapter"]["content"] = quoted["Chapter"]["content"].replace(
            "Only here: wombat.", "Only here: wombat.\n\n```md\n## Decision\n\nWhat we are doing.\n```")
        content = pages_of(book(quoted))[0]["content"]
        self.assertIn("The real one.\n\n```md\n## Not a heading\n# Nor this\n```\n\nStill the decision.\n\n"
                      "~~~~\n## Nor in a longer fence\n~~~\n~~~~\n\nAnd its end.\n", content)
        for elsewhere in ("What we are doing", "aardvark"):
            self.assertNotIn(elsewhere, content)

    def test_the_section_ends_at_a_heading_of_the_first_or_second_level_or_at_the_files_end(self):
        for rest, kept in (("## Consequences\n\naardvark\n", True), ("# Another title\n\naardvark\n", True),
                           ("### A part of the decision\n\naardvark\n", False), ("", True)):
            content = pages_of(book(decision(3, "Three", "The decision.", rest=rest)))[0]["content"]
            self.assertIn("The decision.", content)
            self.assertEqual("aardvark" not in content, kept, rest)
        spaced = decision(4, "Four", "A heading with a space after it.")
        spaced["Chapter"]["content"] = spaced["Chapter"]["content"].replace("## Decision\n", "## Decision \t\n")
        self.assertIn("A heading with a space after it.", pages_of(book(spaced))[0]["content"])

    def test_a_relative_link_of_a_decision_is_written_from_the_pages_directory(self):
        section = ("See [another](0002-other.md), [a page](../how-to/a.md#steps), [here](#evidence), "
                   "[out](https://example.org/x.md), [mail](mailto:a@example.org) and [root](/x.md).\n"
                   "A [titled](0003-t.md \"T\") one, an [angled](<0004 a.md>) one, ![a picture](p.png).\n\n"
                   "[defined]: 0005-defined.md\n[elsewhere]: https://example.org/")
        content = pages_of(book(decision(9, "Nine", section)))[0]["content"]
        self.assertIn("See [another](../decisions/0002-other.md), [a page](../decisions/../how-to/a.md#steps), "
                      "[here](../decisions/0009-made-up.md#evidence), [out](https://example.org/x.md), "
                      "[mail](mailto:a@example.org) and [root](/x.md).\n"
                      "A [titled](../decisions/0003-t.md \"T\") one, an [angled](<../decisions/0004 a.md>) one, "
                      "![a picture](../decisions/p.png).\n\n"
                      "[defined]: ../decisions/0005-defined.md\n[elsewhere]: https://example.org/", content)

    def test_what_looks_like_a_link_inside_code_is_left_as_it_is(self):
        section = ("A span `a](b.md)` and one over a line's\nend `c](d.md)\nmore`, a doubled ``e](f.md) ` ``, then\n\n"
                   "```\n[g](h.md)\n```\n\n    [indented](i.md)\n\nand a real [link](j.md).")
        content = pages_of(book(decision(9, "Nine", section)))[0]["content"]
        for kept in ("`a](b.md)`", "`c](d.md)\nmore`", "``e](f.md) ` ``", "```\n[g](h.md)\n```"):
            self.assertIn(kept, content)
        self.assertIn("a real [link](../decisions/j.md).", content)

    def test_a_book_the_pages_cannot_be_made_from_stops_the_build(self):
        no_section = decision(4, "Four", "x")
        no_section["Chapter"]["content"] = "# 0004. Four\n\n## Context\n\nNo decision.\n\n## Consequences\n\nNone.\n"
        refused(self, lambda: index.preprocess(book(no_section)), "docs/decisions/0004-made-up.md has no `## Decision` "
                "section with text in it", "docs/decisions/0000-template.md", "docs/reference/decisions.md")
        refused(self, lambda: index.preprocess(book(decision(5, "Five", ""))),
                "docs/decisions/0005-made-up.md has no `## Decision`")
        only_quoted = decision(5, "Five", "x")
        only_quoted["Chapter"]["content"] = "# 0005. Five\n\n```\n## Decision\n\nQuoted.\n```\n"
        refused(self, lambda: index.preprocess(book(only_quoted)), "docs/decisions/0005-made-up.md has no `## Decision`")
        for content in ("## Decision\n\nNo title.\n", "# 0007. Another number\n\n## Decision\n\nx\n",
                        "# Six, with no number\n\n## Decision\n\nx\n"):
            untitled = decision(6, "Six", "x")
            untitled["Chapter"]["content"] = content
            refused(self, lambda: index.preprocess(book(untitled)),
                    "docs/decisions/0006-made-up.md does not begin with `# 0006. <title>`")
        refused(self, lambda: index.preprocess(book(decision(6, "Six", "a"), decision(6, "Six again", "b", file="0006-b.md"))),
                "two decisions in docs/SUMMARY.md have the number 0006")
        without = book(decision(1, "One", "x"))
        del without["items"][2]
        refused(self, lambda: index.preprocess(without), "the book has no chapter docs/reference/decisions.md",
                "list it in docs/SUMMARY.md")
        refused(self, lambda: index.preprocess(book(decision(1, "One", "x"), marker="no marker")),
                "does not hold the line `<!-- decisions-index:", "once", "decisions-index.py --write")
        refused(self, lambda: index.preprocess(book(decision(1, "One", "x"), marker=index.MARKER + "\n" + index.MARKER)),
                "does not hold the line `<!-- decisions-index:")

    def test_run_as_mdbook_runs_it(self):
        made = book(decision(1, "One", "Do the first thing."))
        for renderer in ("html", "test"):
            asked = subprocess.run([sys.executable, SCRIPT, "supports", renderer], capture_output=True, text=True)
            self.assertEqual((asked.returncode, asked.stdout, asked.stderr), (0, "", ""))
        given = json.dumps([{"root": "/made/up", "renderer": "html", "mdbook_version": "0.5.4"}, made])
        ran = subprocess.run([sys.executable, SCRIPT], input=given, capture_output=True, text=True)
        self.assertEqual((ran.returncode, ran.stderr), (0, ""))
        self.assertEqual(json.loads(ran.stdout), index.preprocess(copy.deepcopy(made)))
        # `mdbook test` and any other renderer get the book as it came: a decision's code
        # block is then tested once, on the decision's own page.
        given = json.dumps([{"renderer": "test"}, made])
        ran = subprocess.run([sys.executable, SCRIPT], input=given, capture_output=True, text=True)
        self.assertEqual((ran.returncode, ran.stderr, json.loads(ran.stdout)), (0, "", made))
        bad = decision(2, "Two", "x")
        bad["Chapter"]["content"] = "# 0002. Two\n"
        ran = subprocess.run([sys.executable, SCRIPT], input=json.dumps([{"renderer": "html"}, book(bad)]),
                             capture_output=True, text=True)
        self.assertEqual((ran.returncode, ran.stdout), (1, ""))
        self.assertIn("has no `## Decision` section", ran.stderr)
        usage = subprocess.run([sys.executable, SCRIPT, "--check"], capture_output=True, text=True)
        self.assertEqual(usage.returncode, 2)
        self.assertIn("usage: decisions-index.py", usage.stderr)


def page(body):
    """A built page in mdBook's shape: a sidebar with a heading of its own, then `<main>`."""
    return ("<!DOCTYPE HTML>\n<html><body><nav><h2>Keyboard shortcuts</h2><p>Not the page. "
            "<a href=\"elsewhere.html\">x</a></p></nav>\n<main>\n" + body + "\n</main><h2>After</h2><p>Nor this.</p></body></html>\n")


def heading(level, text):
    anchor = "-".join(index.tokens(text))
    return f'<h{level} id="{anchor}"><a class="header" href="#{anchor}">{html.escape(text)}</a></h{level}>'


class Check(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = os.path.realpath(directory.name)
        self.decisions = os.path.join(self.root, "docs", "decisions")
        self.book = os.path.join(self.root, "target", "book")
        for made in (self.decisions, os.path.join(self.root, "docs", "reference"), os.path.join(self.book, "decisions"),
                     os.path.join(self.book, "reference")):
            os.makedirs(made)
        for name, value in (("DECISIONS", self.decisions), ("ROOT", self.root)):
            patch = mock.patch.object(index, name, value)
            patch.start()
            self.addCleanup(patch.stop)
        with open(os.path.join(self.root, "docs", "reference", "decisions.md"), "w", encoding="utf-8") as f:
            f.write(index.INDEX_PAGE)
        self.titles = {1: "Adopt the leader's first rule", 2: "Keep `zebrafish` apart", 26: "Start another page"}
        # Each Decision section as mdBook renders it on the decision's own page.
        self.sections = {
            1: ('<p>The <strong>first</strong> rule, with <code>code</code> &amp; a leader’s answer, as '
                '<a href="0002-made-up.html">decision 0002</a> and <a href="../how-to/a.html#steps">a how-to</a> '
                'say, and <a href="#evidence">below</a>.</p>\n' + heading(3, "A part") + "\n<ul><li>one</li>\n<li>two</li></ul>"),
            2: '<p>Zebrafish stay apart. See <a href="https://example.org/z">the site</a>.</p>',
            26: "<p>Another page’s decision.</p>\n<pre><code>## not a heading\n</code></pre>",
        }
        self.documents = {
            "reference/decisions-0001-0025.html#decisions-0001-to-0025-by-what-each-decided":
                "This page is made. 0001. Adopt the leader’s first rule The page of decision 0001. The first rule. "
                "0002. Keep zebrafish apart The page of decision 0002. Zebrafish stay apart.",
            "reference/decisions-0026-0050.html#decisions-0026-to-0050-by-what-each-decided":
                "0026. Start another page The page of decision 0026. Another page’s decision.",
            "how-to/a.html#a-how-to": "Do it.",
        }
        self.entries = {number: self.moved(section) for number, section in self.sections.items()}
        self.write()

    @staticmethod
    def moved(section):
        """A section as it is rendered in its entry: the same text, its links moved."""
        return (section.replace('href="0002-made-up.html"', 'href="../decisions/0002-made-up.html"')
                .replace('href="../how-to/a.html#steps"', 'href="../decisions/../how-to/a.html#steps"')
                .replace('href="#evidence"', 'href="../decisions/0001-made-up.html#evidence"'))

    def write(self, name="searchindex-0a1b2c3d.js", padding=0):
        """Write the decisions' files, their built pages, the generated pages and the index."""
        for number, section in self.sections.items():
            with open(os.path.join(self.decisions, f"{number:04d}-made-up.md"), "w", encoding="utf-8") as f:
                f.write(f"# {number:04d}. {self.titles[number]}\n\n## Decision\n\nIn Markdown, unread by the check.\n")
            title = f"{number:04d}. {self.titles[number]}".replace("`", "")
            with open(os.path.join(self.book, "decisions", f"{number:04d}-made-up.html"), "w", encoding="utf-8") as f:
                f.write(page(heading(1, title) + "\n" + heading(2, "Context") + "\n<p>Unsearched: wombat.</p>\n"
                             + heading(2, "Decision") + "\n" + section + "\n" + heading(2, "Consequences")
                             + '\n<p>None. <a href="unread.html">x</a></p>'))
        with open(os.path.join(self.decisions, "0000-template.md"), "w", encoding="utf-8") as f:
            f.write("# NNNN. Short title\n\n## Decision\n\nxylophone\n")
        for first in (1, 26):
            body = heading(1, f"Decisions {first:04d} to {first + 24:04d}, by what each decided") + "\n<p>This page is made.</p>\n"
            for number in sorted(self.entries):
                if first <= number < first + 25 and self.entries[number] is not None:
                    title = f"{number:04d}. {self.titles[number]}".replace("`", "")
                    body += (heading(2, title) + f'\n<p><a href="../decisions/{number:04d}-made-up.html">The page of '
                             f"decision {number:04d}</a>.</p>\n" + self.entries[number] + "\n")
            with open(os.path.join(self.book, "reference", f"decisions-{first:04d}-{first + 24:04d}.html"), "w",
                      encoding="utf-8") as f:
                f.write(page(body))
        urls = list(self.documents)
        stored = {str(at): {"id": str(at), "title": "t", "breadcrumbs": "b", "body": self.documents[url]}
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

    def test_every_decision_that_reads_in_its_entry_as_on_its_own_page_passes_and_the_size_is_printed(self):
        size = os.path.getsize(os.path.join(self.book, "searchindex-0a1b2c3d.js"))
        self.assertEqual(self.check(),
                         f"decisions-index: 3 decisions read on the pages below docs/reference/decisions.md as on "
                         f"their own, text and links, and the search finds each there and none by its own page; the "
                         f"search index is {size:,} bytes, 0% of the 10,000,000 at which mdBook warns, and the early "
                         f"bound is {index.EARLY_BOUND:,}\n")

    def test_an_entry_whose_text_is_not_the_sections_fails_with_where_they_differ(self):
        whole = self.entries[1]
        for entry, said in ((whole.split("</p>")[0] + "</p>", "say, and below. A part one two` on the decision's page"),
                            (whole.replace("first", "second"), "`The second rule, with code"),
                            (whole + "<p>More than was decided.</p>", "More than was decided.` in its entry"),
                            (whole.replace("<code>code</code>", ""), "`The first rule, with & a leader’s answer")):
            self.entries[1] = entry
            self.write()
            refused(self, lambda: index.check(self.book), "the Decision section of docs/decisions/0001-made-up.md does "
                    "not read on reference/decisions-0001-0025.html as it reads on the decision's own page", said,
                    "write a footnote as a sentence", "`[words](target)`")

    def test_a_link_that_opens_another_page_from_the_entry_fails_and_names_both(self):
        whole = self.entries[1]
        # Left as it was written, a relative link opens a page of reference/, which is not there.
        self.entries[1] = whole.replace('href="../decisions/0002-made-up.html"', 'href="0002-made-up.html"')
        self.write()
        refused(self, lambda: index.check(self.book), "a link of docs/decisions/0001-made-up.md's Decision section opens "
                "decisions/0002-made-up.html on the decision's own page and reference/0002-made-up.html on "
                "reference/decisions-0001-0025.html", "Write the link as `[words](target)`")
        self.entries[1] = whole.replace('href="../decisions/0001-made-up.html#evidence"', 'href="#evidence"')
        self.write()
        refused(self, lambda: index.check(self.book), "opens decisions/0001-made-up.html#evidence on the decision's own "
                "page and reference/decisions-0001-0025.html#evidence on")
        # A link that the entry lacks, with its words still there.
        self.entries[1] = whole.replace('<a href="../decisions/../how-to/a.html#steps">a how-to</a>', "a how-to")
        self.write()
        refused(self, lambda: index.check(self.book), "opens how-to/a.html#steps on the decision's own page and "
                "decisions/0001-made-up.html#evidence on")
        self.entries[1] = whole.replace('<a href="../decisions/0001-made-up.html#evidence">below</a>', "below")
        self.write()
        refused(self, lambda: index.check(self.book), "opens decisions/0001-made-up.html#evidence on the decision's own "
                "page and None on")
        self.entries[1] = whole
        self.write()
        self.check()

    def test_a_decision_with_no_entry_no_page_or_another_heading_fails(self):
        self.entries[2] = None
        self.write()
        refused(self, lambda: index.check(self.book), "docs/decisions/0002-made-up.md has 1 sections headed `Decision` on "
                "its built page and 0 entries on reference/decisions-0001-0025.html")
        self.entries[2] = self.moved(self.sections[2])
        self.titles[2] = "Keep them apart"
        self.write()
        with open(os.path.join(self.decisions, "0002-made-up.md"), "w", encoding="utf-8") as f:
            f.write("# 0002. Keep `zebrafish` apart\n\n## Decision\n\nx\n")
        refused(self, lambda: index.check(self.book), "the entry of docs/decisions/0002-made-up.md on "
                "reference/decisions-0001-0025.html is headed `0002. Keep them apart`, not `0002. Keep `zebrafish` apart`")
        self.titles[2] = "Keep `zebrafish` apart"
        self.write()
        os.remove(os.path.join(self.book, "reference", "decisions-0026-0050.html"))
        refused(self, lambda: index.check(self.book), "the built book has no page reference/decisions-0026-0050.html for "
                "docs/decisions/0026-made-up.md", "listed in docs/SUMMARY.md", "the preprocessor `decisions-index` in book.toml")
        self.write()
        os.remove(os.path.join(self.book, "decisions", "0026-made-up.html"))
        refused(self, lambda: index.check(self.book), "the built book has no page decisions/0026-made-up.html")
        self.write()
        with open(os.path.join(self.book, "decisions", "0026-made-up.html"), "w", encoding="utf-8") as f:
            f.write(page(heading(1, "0026. Start another page") + "\n" + heading(2, "Context") + "\n<p>No decision.</p>"))
        refused(self, lambda: index.check(self.book), "docs/decisions/0026-made-up.md has 0 sections headed `Decision`")
        with open(os.path.join(self.book, "decisions", "0026-made-up.html"), "w", encoding="utf-8") as f:
            f.write(page(heading(2, "Decision") + "\n" + heading(2, "Consequences")))
        refused(self, lambda: index.check(self.book), "docs/decisions/0026-made-up.md's section `Decision` is empty")

    def test_a_decision_file_the_index_cannot_read_fails(self):
        with open(os.path.join(self.decisions, "0003-no-heading.md"), "w", encoding="utf-8") as f:
            f.write("# Three\n\n## Decision\n\nx\n")
        refused(self, lambda: index.check(self.book), "docs/decisions/0003-no-heading.md does not begin with "
                "`# 0003. <title>`")

    def test_a_generated_page_that_the_search_does_not_hold_fails(self):
        page_26 = "reference/decisions-0026-0050.html#decisions-0026-to-0050-by-what-each-decided"
        body = self.documents.pop(page_26)
        self.write()
        refused(self, lambda: index.check(self.book), "the search index has no document for "
                "reference/decisions-0026-0050.html with `0026. Start another page` in it",
                "left out of the search in book.toml")
        self.documents[page_26] = body.replace("0026. Start another page", "0026. Start a page")
        self.write()
        refused(self, lambda: index.check(self.book), "with `0026. Start another page` in it")
        # The heading in another page's document is not where the reader is sent.
        self.documents[page_26] = "nothing"
        self.documents["reference/decisions-0001-0025.html#x"] = body
        self.write()
        refused(self, lambda: index.check(self.book), "no document for reference/decisions-0026-0050.html")

    def test_a_decisions_own_page_in_the_index_fails(self):
        self.documents["decisions/0001-made-up.html#0001-adopt"] = "the page itself"
        self.write()
        refused(self, lambda: index.check(self.book), "holds 1 of the decisions' own pages, "
                "decisions/0001-made-up.html among them", "[output.html.search.chapter]")

    def test_the_reference_page_is_the_generated_one(self):
        with open(os.path.join(self.root, "docs", "reference", "decisions.md"), "a", encoding="utf-8") as f:
            f.write("\nA line added by hand.\n")
        refused(self, lambda: index.check(self.book), "docs/reference/decisions.md is not what scripts/decisions-index.py "
                "writes", "python3 scripts/decisions-index.py --write")
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(index.main(["decisions-index.py", "--write"]), 0)
        self.check()

    def test_the_early_bound_fails_with_the_two_things_to_do_and_after_its_raise_with_the_work_item_due(self):
        path = self.write(padding=3_000)
        size = os.path.getsize(path)
        with mock.patch.object(index, "EARLY_BOUND", size - 1):
            said = refused(self, lambda: index.check(self.book),
                           f"target/book/searchindex-0a1b2c3d.js, is {size:,} bytes, past the early bound of {size - 1:,}",
                           "Nothing is wrong with this change", "`brigade work-item add --tag cavalry,decision`",
                           "\"Decide what the book's search indexes\", a blank line and this message on its standard input",
                           f"change the line `EARLY_BOUND = {size - 1:_}` to `EARLY_BOUND = RAISED_BOUND`",
                           "name the work item in the commit message", "RAISED_BOUND is ADR 0165's")
            self.assertNotIn("cannot land", said)
        # What the first message says to do: EARLY_BOUND is RAISED_BOUND from then on.
        with mock.patch.object(index, "EARLY_BOUND", size - 1), mock.patch.object(index, "RAISED_BOUND", size - 1):
            said = refused(self, lambda: index.check(self.book), "past the early bound after its one raise",
                           "This change cannot land as it is", "`brigade work-item list --tag decision --all` finds it",
                           "an ADR that supersedes ADR 0165's bounds")
            self.assertNotIn("Nothing is wrong", said)
        with mock.patch.object(index, "EARLY_BOUND", size):
            self.check()

    def test_the_bounds_are_the_adrs_before_the_raise_and_after_it(self):
        self.assertEqual((index.RAISED_BOUND, index.MDBOOK_WARNS_AT), (8_500_000, 10_000_000),
                         "ADR 0165 sets the raised bound, and mdBook its warning: neither moves without a new ADR")
        self.assertIn(index.EARLY_BOUND, (7_000_000, index.RAISED_BOUND),
                      "the early bound is ADR 0165's 7,000,000, or RAISED_BOUND after its one raise")
        with open(SCRIPT, encoding="utf-8") as f:
            text = f.read()
        self.assertLess(text.index("\nRAISED_BOUND = "), text.index("\nEARLY_BOUND = "),
                        "RAISED_BOUND is defined first, so that `EARLY_BOUND = RAISED_BOUND` is the whole raise")

    def test_a_book_with_no_index_file_two_of_them_or_another_form_fails(self):
        os.remove(os.path.join(self.book, "searchindex-0a1b2c3d.js"))
        refused(self, lambda: index.check(self.book), "holds 0 search index files", "run `just docs`")
        self.write()
        self.write(name="searchindex-ffffffff.js")
        refused(self, lambda: index.check(self.book), "holds 2 search index files")
        os.remove(os.path.join(self.book, "searchindex-ffffffff.js"))
        with open(os.path.join(self.book, "searchindex-0a1b2c3d.js"), "w", encoding="utf-8") as f:
            f.write("Object.assign(window.search, {});")
        refused(self, lambda: index.check(self.book), "is not `window.search = Object.assign(window.search, "
                "JSON.parse('...'));`", "mdBook 0.5.4")

    def test_the_index_files_escapes_are_undone(self):
        self.documents["how-to/a.html#a-how-to"] = "It's a \\ and a \"quote\", and a line\nbreak."
        self.write()
        _, documents = index.search_documents(self.book)
        self.assertEqual(documents["how-to/a.html"], " It's a \\ and a \"quote\", and a line\nbreak.")


class Repository(unittest.TestCase):
    def read(self, *path):
        with open(os.path.join(HERE, "..", *path), encoding="utf-8") as f:
            return f.read()

    def test_book_toml_names_the_preprocessor_and_leaves_the_decisions_pages_out_of_the_search(self):
        text = self.read("book.toml")
        self.assertIn('\n[preprocessor.decisions-index]\ncommand = "python3 scripts/decisions-index.py"\n', text)
        self.assertIn('\n[output.html.search.chapter]\n"decisions" = { enable = false }\n', text)
        self.assertIn("\n[output.html.search]\nheading-split-level = 1\n", text)

    def test_the_index_chapter_is_generated_is_in_the_summary_and_holds_the_marker_once(self):
        self.assertEqual(self.read("docs", "SUMMARY.md").count(f"- [{index.INDEX_NAME}](reference/decisions.md)\n"), 1)
        self.assertEqual(self.read("docs", "reference", "decisions.md"), index.INDEX_PAGE,
                         "run `python3 scripts/decisions-index.py --write`")
        self.assertEqual(index.INDEX_PAGE.count(index.MARKER), 1)
        self.assertTrue(index.INDEX_PAGE.startswith("<!-- Generated by scripts/decisions-index.py. Do not edit"))
        self.assertTrue(os.path.isfile(os.path.join(HERE, "..", index.ADR)), "the ADR every message names")

    def test_just_docs_runs_the_check_on_the_built_book(self):
        self.assertIn("\n    python3 scripts/decisions-index.py --check target/book\n", self.read("justfile"))


if __name__ == "__main__":
    unittest.main()
