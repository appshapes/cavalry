#!/usr/bin/env python3
"""Tests of release-notes-email.py: the sample draft is laid out as designed, every construct the
shape forbids is a finding with its line, and nothing raw from the draft reaches the HTML.
Standard library only; `just test-scripts` runs it.

    python3 scripts/release-notes-email-test.py
"""
import datetime
import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "release-notes-email.py")
spec = importlib.util.spec_from_file_location("release_notes_email", SCRIPT)
email = importlib.util.module_from_spec(spec)
spec.loader.exec_module(email)

with open(os.path.join(HERE, "release-notes-email-sample.md"), encoding="utf-8") as f:
    SAMPLE = f.read()
TITLE = "# What's new in Cavalry, October 10 to 16, 2026\n\n"
FOOTER = ("\n\n---\n\nEvery change: [CHANGELOG.md](https://github.com/appshapes/cavalry/blob/master/CHANGELOG.md)\n\n"
          "You are on the list for the release notes of [appshapes/cavalry](https://github.com/appshapes/cavalry). "
          "To stop these emails, reply to this one and say so.\n")
DAY = datetime.date(2026, 10, 19)
# Every tag the layout writes. Anything else in the HTML came from the draft and should have been escaped.
OWN_TAGS = {"!doctype", "html", "head", "meta", "title", "body", "div", "table", "tr", "td", "p", "h1", "h2",
            "h3", "ul", "li", "a", "strong", "em", "code", "pre"}


def contrast(a, b):
    def luminance(colour):
        channels = [int(colour[i:i + 2], 16) / 255 for i in (1, 3, 5)]
        r, g, b = [c / 12.92 if c <= 0.03928 else ((c + 0.055) / 1.055) ** 2.4 for c in channels]
        return 0.2126 * r + 0.7152 * g + 0.0722 * b
    light, dark = sorted((luminance(a), luminance(b)), reverse=True)
    return (light + 0.05) / (dark + 0.05)


def card(notes):
    """The card's HTML for a draft written under the usual title."""
    return email.render(TITLE + notes, DAY).split("</h1>", 1)[1]


def findings(notes, draft=True):
    return [msg for _, msg in email.check(notes, draft)]


def lines(notes, draft=True):
    return [n for n, _ in email.check(notes, draft)]


class Layout(unittest.TestCase):
    def setUp(self):
        self.html = email.render(SAMPLE + FOOTER, DAY)

    def test_the_sample_is_a_clean_draft(self):
        self.assertEqual(email.check(SAMPLE), [])

    def test_it_is_safe_for_email_clients(self):
        for banned in ("<style", "<script", "<img", "<link", "class=", "@media", "url(", "<font", "javascript:"):
            self.assertNotIn(banned, self.html)
        self.assertIn('<meta name="color-scheme" content="light">', self.html)
        self.assertLess(len(self.html.encode("utf-8")), email.MAX_BYTES)

    def test_a_long_word_cannot_widen_the_card(self):
        self.assertIn("max-width:600px;table-layout:fixed", self.html)
        for style in (email.ST_CARD, email.ST_TERM, email.ST_NOTE, email.ST_FOOTER):
            self.assertIn("overflow-wrap:anywhere", style)

    def test_every_text_colour_reads_on_its_background(self):
        pairs = [(email.INK, email.CARD), (email.INK_SOFT, email.CARD), (email.MUTED, email.CARD), (email.ACCENT, email.CARD),
                 (email.MUTED, email.PAGE), (email.ACCENT, email.PAGE), (email.INK, email.CODE_BG),
                 (email.TERM_INK, email.TERM_BG), (email.TERM_LABEL, email.TERM_BG), (email.INK, email.NOTE_BG)]
        for ink, background in pairs:
            self.assertGreaterEqual(contrast(ink, background), 4.5, f"{ink} on {background}")

    def test_the_masthead_and_the_title(self):
        self.assertIn(f'<a href="{email.HOME}"', self.html)
        self.assertIn("Release notes &middot; October 19, 2026", self.html)
        self.assertIn("<title>What&#x27;s new in Cavalry, October 10 to 16, 2026</title>", self.html)
        self.assertIn('<h1 style="', self.html)
        self.assertEqual(self.html.count("<h1 "), 1)

    def test_the_preheader_is_the_leads(self):
        self.assertIn("Publish and subscribe over shared memory on one host · Configure by environment variable, or not at all", self.html)

    def test_sections_leads_bullets_and_code(self):
        self.assertIn(">What&#x27;s new</h2>", self.html)
        self.assertIn(">Decided</h2>", self.html)
        self.assertIn(">Publish and subscribe over shared memory on one host</h3>", self.html)
        self.assertIn("<li style=", self.html)
        self.assertIn(f'<code style="{email.ST_CODE}">cavalry run</code>', self.html)

    def test_a_fence_is_a_terminal_card_with_its_label(self):
        self.assertIn(f'<p style="{email.ST_TERM_LABEL}">in a terminal</p>', self.html)
        self.assertIn("target/release/cavalry run &amp;\n", self.html)
        self.assertIn(f'<pre style="{email.ST_TERM_CODE}">', self.html)

    def test_before_you_update_is_a_callout(self):
        self.assertIn(f'<td style="{email.ST_NOTE}">', self.html)
        self.assertIn(f'<h2 style="{email.ST_NOTE_H2}">Before you update</h2>', self.html)
        self.assertEqual(self.html.count(email.NOTE_BG), 1)

    def test_links_are_links(self):
        self.assertIn('<a href="https://github.com/appshapes/cavalry/blob/master/docs/how-to/run-the-bench.md"', self.html)
        self.assertIn(">Step by step →</a>", self.html)

    def test_the_footer_follows_the_rule(self):
        self.assertIn(f'<td style="{email.ST_FOOTER}">', self.html)
        self.assertIn("reply to this one and say so.</p>", self.html)
        self.assertIn(f'<a href="https://github.com/appshapes/cavalry" style="{email.ST_FOOTER_A}">appshapes/cavalry</a>', self.html)
        self.assertLess(self.html.index("</h1>"), self.html.index(email.ST_FOOTER))

    def test_no_footer_without_a_rule(self):
        self.assertNotIn(email.ST_FOOTER, email.render(SAMPLE, DAY))

    def test_only_the_layouts_own_tags_appear(self):
        import re
        tags = {m.lower() for m in re.findall(r"</?([A-Za-z!][A-Za-z0-9]*)", self.html)}
        self.assertEqual(tags - OWN_TAGS, set())

    def test_the_date_is_optional(self):
        self.assertIn(">Release notes</td>", email.render(SAMPLE, None))


class Inline(unittest.TestCase):
    def test_text_is_escaped(self):
        out = card("Some <b>bold</b> & \"quoted\" words.\n")
        self.assertIn("Some &lt;b&gt;bold&lt;/b&gt; &amp; &quot;quoted&quot; words.", out)
        self.assertNotIn("<b>", out)

    def test_bold_code_and_emphasis(self):
        out = card("Type **this** and *that* in `cavalry run`.\n")
        self.assertIn("<strong>this</strong>", out)
        self.assertIn("<em>that</em>", out)
        self.assertIn(f'<code style="{email.ST_CODE}">cavalry run</code>', out)

    def test_a_code_span_is_escaped_and_double_ticks_hold_a_backtick(self):
        out = card("A `<tag>` and a `` ` `` tick.\n")
        self.assertIn(">&lt;tag&gt;</code>", out)
        self.assertIn(">`</code>", out)

    def test_an_unclosed_tick_is_text(self):
        self.assertIn("a ` tick", card("Just a ` tick.\n"))

    def test_links_and_bare_addresses(self):
        out = card("See [the page](https://github.com/appshapes/cavalry/blob/master/README.md) or https://github.com/appshapes/cavalry.\n")
        self.assertIn('<a href="https://github.com/appshapes/cavalry/blob/master/README.md"', out)
        self.assertIn('<a href="https://github.com/appshapes/cavalry"', out)
        self.assertIn("</a>.</p>", out)

    def test_an_address_in_a_link_is_escaped(self):
        out = card("[x](https://github.com/appshapes/cavalry?a=1&b=2)\n")
        self.assertIn('href="https://github.com/appshapes/cavalry?a=1&amp;b=2"', out)

    def test_backslash_escapes(self):
        self.assertIn("*not emphasis*", card("\\*not emphasis\\*\n"))

    def test_a_heading_loses_its_trailing_stop(self):
        self.assertIn(">Do the thing</h3>", card("## S\n\n### Do the thing.\n\nWords.\n"))


class Check(unittest.TestCase):
    """Every construct the shape forbids is a finding that names its line."""

    def finding(self, notes, want, line=None):
        got = email.check(TITLE + notes)
        self.assertTrue(any(want in msg for _, msg in got), f"no finding says {want!r} in {got}")
        if line is not None:
            self.assertIn(line, [n for n, msg in got if want in msg], got)

    def test_a_table(self):
        self.finding("## S\n\n| a | b |\n| - | - |\n", "a table is not supported", line=5)

    def test_a_quote(self):
        self.finding("## S\n\n> wise words\n", "a block quote is not supported", line=5)

    def test_raw_html(self):
        self.finding("## S\n\n<div>hi</div>\n", "raw HTML is not supported", line=5)

    def test_a_nested_list(self):
        self.finding("## S\n\n- one\n  - two\n", "a nested list is not supported", line=6)

    def test_a_numbered_list(self):
        self.finding("## S\n\n1. one\n", "a numbered list is not supported", line=5)

    def test_a_star_bullet(self):
        self.finding("## S\n\n* one\n", "`*` and `+` bullets are not supported", line=5)

    def test_a_deep_heading(self):
        self.finding("## S\n\n#### deep\n", "a heading deeper than ###", line=5)

    def test_an_empty_heading(self):
        self.finding("## S\n\n###\n", "a heading has no text", line=5)

    def test_a_picture_on_its_own_line_and_inline(self):
        self.finding("## S\n\n![alt](https://x/y.png)\n", "a picture is not supported", line=5)
        self.finding("## S\n\nSee ![alt](https://x/y.png) here.\n", "a picture is not supported", line=5)

    def test_a_relative_or_http_link(self):
        self.finding("## S\n\n[page](docs/page.md)\n", 'the link "docs/page.md" is not an absolute https URL', line=5)
        self.finding("## S\n\n[page](http://example.com/)\n", "is not an absolute https URL", line=5)
        self.finding("## S\n\n[x](javascript:alert(1))\n", "is not an absolute https URL", line=5)

    def test_an_unlabelled_fence(self):
        self.finding("## S\n\n```\ncavalry run\n```\n", "a fenced block needs a label", line=5)

    def test_a_backtick_in_a_label(self):
        self.finding("## S\n\n```in `x`\ncavalry run\n```\n", "may not contain a backtick", line=5)

    def test_an_unclosed_fence(self):
        self.finding("## S\n\n```in a terminal\ncavalry run\n", "never closed", line=5)

    def test_a_rule_in_a_draft(self):
        self.finding("## S\n\nWords.\n\n---\n\nFooter.\n", "the draft carries a rule", line=7)

    def test_a_rule_under_text(self):
        self.finding("## S\n\nWords\n---\n", "a line of dashes under text", line=6)

    def test_a_setext_heading(self):
        self.finding("## S\n\nWords\n===\n", "setext", line=6)

    def test_an_indented_code_block(self):
        self.finding("## S\n\n    cavalry run\n", "an indented code block is not supported", line=5)

    def test_a_missing_or_second_title(self):
        self.assertIn("the first line must be the `# ` title", findings("Words.\n")[0])
        self.assertIn("the first line must be the `# ` title", findings("")[0])
        self.finding("## S\n\n# Again\n", "a second # heading", line=5)

    def test_a_lead_before_the_first_section(self):
        self.finding("### Lead\n", "a ### heading before the first ## section", line=3)

    def test_a_footer_may_carry_paragraphs_only(self):
        got = findings(TITLE + "## S\n\nWords.\n\n---\n\nFooter.\n\n- a bullet\n\n---\n", draft=False)
        self.assertIn("the footer may carry paragraphs only", got)
        self.assertIn("a second rule; the footer is everything after the first", got)

    def test_too_long(self):
        self.assertIn("characters", findings(TITLE + "x" * email.MAX_NOTES)[0])

    def test_findings_are_sorted_by_line_and_unique(self):
        got = email.check(TITLE + "## S\n\n| t |\n\n> q\n\n| t |\n")
        self.assertEqual([n for n, _ in got], sorted(n for n, _ in got))
        self.assertEqual(len(got), len(set(got)))

    def test_control_characters_are_dropped(self):
        self.assertNotIn("‮", card("Words‮ here.\n"))

    def test_render_refuses_what_check_refuses(self):
        with self.assertRaises(ValueError) as cm:
            email.render(TITLE + "## S\n\n| t |\n", DAY)
        self.assertIn("line 5: a table is not supported", str(cm.exception))


class CommandLine(unittest.TestCase):
    def run_it(self, *args):
        return subprocess.run([sys.executable, "-I", SCRIPT, *args], capture_output=True, text=True)

    def test_check_is_quiet_when_clean_and_lists_findings_otherwise(self):
        with tempfile.TemporaryDirectory() as d:
            good = os.path.join(d, "good.md")
            with open(good, "w", encoding="utf-8") as f:
                f.write(SAMPLE)
            res = self.run_it("--check", "--in", good)
            self.assertEqual((res.returncode, res.stdout, res.stderr), (0, "", ""))
            bad = os.path.join(d, "bad.md")
            with open(bad, "w", encoding="utf-8") as f:
                f.write(TITLE + "## S\n\n| t |\n\n> q\n")
            res = self.run_it("--check", "--in", bad)
            self.assertEqual(res.returncode, 1)
            self.assertEqual(res.stdout, "line 5: a table is not supported\nline 7: a block quote is not supported\n")

    def test_render_writes_the_email_and_refuses_a_draft_with_findings(self):
        with tempfile.TemporaryDirectory() as d:
            src, out = os.path.join(d, "email.md"), os.path.join(d, "email.html")
            with open(src, "w", encoding="utf-8") as f:
                f.write(SAMPLE + FOOTER)
            res = self.run_it("--in", src, "--out", out, "--date", "2026-10-19")
            self.assertEqual((res.returncode, res.stderr), (0, ""))
            with open(out, encoding="utf-8") as f:
                self.assertIn("October 19, 2026", f.read())
            with open(src, "w", encoding="utf-8") as f:
                f.write(TITLE + "| t |\n")
            res = self.run_it("--in", src, "--out", out)
            self.assertEqual(res.returncode, 1)
            self.assertIn("line 3: a table is not supported", res.stderr)

    def test_a_missing_file_or_out_is_an_error(self):
        self.assertEqual(self.run_it("--check", "--in", "/nonexistent/notes.md").returncode, 1)
        self.assertEqual(self.run_it("--in", SCRIPT).returncode, 2)


if __name__ == "__main__":
    unittest.main()
