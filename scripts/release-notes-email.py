#!/usr/bin/env python3
"""release-notes-email: the weekly release-notes email, laid out as the HTML an inbox shows.

    release-notes-email.py --in <email.md> --out <email.html> [--date YYYY-MM-DD]
    release-notes-email.py --check --in <notes.md>

The first form renders the Markdown that the writer agent drafted, the gate approved and
`scripts/release-notes.py finish` put the footer under. The second reads a draft and prints
every finding on its own line, `line <n>: <what is wrong>`, exiting 1 when there is one:
`scripts/release-notes.py lint` runs it on every draft, so a construct this layout would
refuse is a finding for the writer's fix cycle and never a surprise in an inbox (ADR 0026).

`.claude/agents/release-notes-email.md` asks the writer for the shape this layout is made
for: one `#` title, paragraphs, `##` sections, `###` leads, `-` bullets on one level, fenced
blocks with a label saying where the words are typed, `**bold**`, `` `code` `` and absolute
https links. Anything else is refused with its line number rather than guessed at: a table,
a quote, raw HTML, a nested or numbered list, a heading deeper than ###, a picture, a
relative link, a fence with no label. A `---` rule starts the footer, which the workflow
writes and the writer never does.

Email-client rules the HTML keeps: tables for layout, inline styles only, no <style>, no web
font, no script and no picture, so it reads the same with images off; `color-scheme: light`
and a solid background behind every text, so a dark-mode client does not put dark text on a
dark card; under 100 KB, because Gmail clips a longer message. Every character of the draft
is escaped on its way into the HTML. Standard library only, Python 3.9 (ADR 0003). Tested by
release-notes-email-test.py.
"""
from __future__ import annotations

import argparse
import datetime
import html
import re
import sys
import urllib.parse

MAX_BYTES = 100 * 1024    # Gmail clips a message over 102 KB and hides the rest, the footer included
MAX_NOTES = 50000         # characters of Markdown; a week's email is a few thousand

# The palette: ink on white, one accent, a dark terminal card. Every text colour is at least
# 4.5:1 on its background (WCAG AA); release-notes-email-test.py measures each pair.
PAGE = "#f4f5f7"          # behind the card
CARD = "#ffffff"
INK = "#1f2933"           # body text, headings, the terminal card's background
INK_SOFT = "#3e4c59"      # the intro
MUTED = "#616e7c"         # section labels, the masthead's date, the footer
LINE = "#e4e7eb"          # the card's border and the rules
ACCENT = "#1b4f8a"        # the masthead's name, links, the strip on top of the card
TERM_BG = INK
TERM_INK = "#e4e7eb"
TERM_LABEL = "#9aa5b1"
NOTE_BG = "#fdf6e3"       # the "Before you update" callout
NOTE_EDGE = "#c99a2e"     # its left border; a fill, never text
CODE_BG = "#eef1f4"       # behind an inline code span

FONT = "-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif"
MONO = "SFMono-Regular,Menlo,Consolas,'Liberation Mono',monospace"
# A long word wraps inside the card: without this it widens the card past a phone's screen.
WRAP = "word-wrap:break-word;overflow-wrap:anywhere;word-break:break-word"
ST_BODY = f"margin:0;padding:0;background:{PAGE};-webkit-text-size-adjust:100%"
ST_PREHEADER = f"display:none;max-height:0;overflow:hidden;font-size:1px;line-height:1px;color:{PAGE};opacity:0"
ST_MAST_NAME = f"vertical-align:middle;font-family:{FONT};font-size:18px;line-height:22px;font-weight:700;color:{ACCENT}"
ST_MAST_DATE = f"vertical-align:middle;font-family:{FONT};font-size:13px;line-height:18px;color:{MUTED}"
ST_CARD = f"background:{CARD};border:1px solid {LINE};border-top:4px solid {ACCENT};border-radius:12px;padding:32px 36px 28px;{WRAP}"
ST_H1 = f"margin:0 0 14px;font-family:{FONT};font-size:28px;line-height:34px;font-weight:700;color:{INK}"
ST_INTRO = f"margin:0 0 14px;font-family:{FONT};font-size:16px;line-height:25px;color:{INK_SOFT}"
ST_RULE = f"border-top:1px solid {LINE};font-size:0;line-height:0"
ST_H2 = f"margin:18px 0 6px;font-family:{FONT};font-size:13px;line-height:18px;font-weight:700;letter-spacing:.08em;text-transform:uppercase;color:{MUTED}"
ST_H3 = f"margin:22px 0 8px;font-family:{FONT};font-size:19px;line-height:25px;font-weight:600;color:{INK}"
ST_P = f"margin:0 0 12px;font-family:{FONT};font-size:15px;line-height:24px;color:{INK}"
ST_UL = f"margin:0 0 12px;padding:0 0 0 20px;font-family:{FONT};font-size:15px;line-height:24px;color:{INK}"
ST_LI = "margin:0 0 8px"
ST_LINK = f"color:{ACCENT};font-weight:600"
ST_CODE = f"font-family:{MONO};font-size:13px;background:{CODE_BG};color:{INK};padding:1px 5px;border-radius:4px"
ST_TERM = f"background:{TERM_BG};border-radius:8px;padding:12px 16px;{WRAP}"
ST_TERM_LABEL = f"margin:0 0 6px;font-family:{FONT};font-size:11px;line-height:14px;font-weight:700;letter-spacing:.08em;text-transform:uppercase;color:{TERM_LABEL}"
ST_TERM_CODE = f"margin:0;font-family:{MONO};font-size:13px;line-height:20px;color:{TERM_INK};white-space:pre-wrap"
ST_NOTE = f"background:{NOTE_BG};border-left:4px solid {NOTE_EDGE};border-radius:6px;padding:14px 18px 6px;{WRAP}"
ST_NOTE_H2 = f"margin:0 0 8px;font-family:{FONT};font-size:13px;line-height:18px;font-weight:700;letter-spacing:.08em;text-transform:uppercase;color:{INK}"
ST_FOOTER = f"padding:22px 16px 0;text-align:center;font-family:{FONT};font-size:12px;line-height:19px;color:{MUTED};{WRAP}"
ST_FOOTER_P = "margin:0 0 6px"
ST_FOOTER_A = f"color:{MUTED}"
TABLE = '<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0"'

# The masthead is the layout's, like the card: no draft can leave it out or reword it.
NAME = "Cavalry"
HOME = "https://github.com/appshapes/cavalry"
CALLOUT = "before you update"    # the `##` section the layout shows as a callout
MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August",
          "September", "October", "November", "December"]

RE_HEADING = re.compile(r"^(#{1,6})(?:[ \t]+(.*?))?[ \t]*$")
RE_FENCE = re.compile(r"^(`{3,}|~{3,})[ \t]*(.*?)[ \t]*$")
RE_BULLET = re.compile(r"^-[ \t]+(.*)$")
RE_OTHER_BULLET = re.compile(r"^[*+][ \t]+(.*)$")
RE_ORDERED = re.compile(r"^\d{1,9}[.)][ \t]+(.*)$")
RE_RULE = re.compile(r"^(?:-[ \t]*){3,}$|^(?:\*[ \t]*){3,}$|^(?:_[ \t]*){3,}$")
RE_SETEXT = re.compile(r"^(?:=+|-+)$")
RE_RAW_HTML = re.compile(r"^<[A-Za-z/!]")
RE_INDENTED = re.compile(r"^(?: {4}|\t)")
RE_BARE_URL = re.compile(r"""https://[^\s<>"'`]+""")
RE_DROPPED = re.compile(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f‪-‮⁦-⁩]")    # control and direction-override characters
ESCAPABLE = "\\`*_{}[]()#+-.!<>|"


# ---- parse ---------------------------------------------------------------------------

def parse(src):
    """Split the draft into blocks, each with the 1-based line it starts on, and return every
    finding rather than the first: headings, paragraphs, `-` lists, labelled fences and rules
    are the blocks; everything the shape forbids is a finding and dropped."""
    lines = src.replace("\r\n", "\n").split("\n")
    blocks, findings = [], []
    para = lst = None

    def fail(n, msg):
        findings.append((n, msg))

    def close():
        nonlocal para, lst
        if para:
            blocks.append(para)
            para = None
        if lst:
            blocks.append(lst)
            lst = None

    i = 0
    while i < len(lines):
        raw = lines[i]
        n = i + 1
        i += 1
        line = raw.rstrip(" \t")
        text = line.lstrip(" \t")
        indent = len(line) - len(text)
        if not text:
            close()
            continue
        fence = RE_FENCE.match(text)
        if fence and indent == 0:
            close()
            mark, info = fence.group(1), fence.group(2)
            if "`" in info:
                fail(n, "a fence's label may not contain a backtick")
            elif not info:
                fail(n, "a fenced block needs a label saying where the words are typed, for example ```in a terminal")
            code, closed = [], False
            while i < len(lines):
                closing = lines[i].rstrip(" \t").lstrip(" ")
                i += 1
                if closing and set(closing) == {mark[0]} and len(closing) >= len(mark):
                    closed = True
                    break
                code.append(lines[i - 1])
            if not closed:
                fail(n, "the fenced block is never closed")
            blocks.append({"kind": "fence", "line": n, "info": info, "code": "\n".join(code)})
            continue
        bullet = RE_BULLET.match(text)
        if lst is not None and indent:
            if bullet or RE_OTHER_BULLET.match(text) or RE_ORDERED.match(text):
                fail(n, "a nested list is not supported; keep one level of `-` bullets")
            else:
                lst["items"][-1] += " " + text    # a continuation line of the current item
            continue
        if bullet and indent == 0:
            if para:
                blocks.append(para)
                para = None
            if lst is None:
                lst = {"kind": "list", "line": n, "items": []}
            lst["items"].append(bullet.group(1))
            continue
        if RE_OTHER_BULLET.match(text) and indent == 0:
            fail(n, "a bullet starts with `- `; `*` and `+` bullets are not supported")
            close()
            continue
        if RE_ORDERED.match(text) and indent == 0:
            fail(n, "a numbered list is not supported; use `-` bullets or a paragraph")
            close()
            continue
        heading = RE_HEADING.match(text) if text.startswith("#") else None
        if heading:
            close()
            level, words = len(heading.group(1)), (heading.group(2) or "").strip()
            if level > 3:
                fail(n, "a heading deeper than ### is not supported")
                level = 3
            if not words:
                fail(n, "a heading has no text")
            blocks.append({"kind": "heading", "line": n, "level": level, "text": words})
            continue
        if RE_RULE.match(text):
            if para:
                fail(n, "a line of dashes under text is read as a heading by Markdown; put a blank line before a rule")
                close()
                continue
            close()
            blocks.append({"kind": "rule", "line": n})
            continue
        if para and RE_SETEXT.match(text):
            fail(n, "an underlined (setext) heading is not supported; write `## ` before the heading")
            close()
            continue
        if text.startswith("!["):
            fail(n, "a picture is not supported; the email reads the same with images off")
            close()
            continue
        if text.startswith(">"):
            fail(n, "a block quote is not supported")
            close()
            continue
        if text.startswith("|"):
            fail(n, "a table is not supported")
            close()
            continue
        if RE_RAW_HTML.match(text):
            fail(n, "raw HTML is not supported; the layout is the renderer's")
            close()
            continue
        if para is None and lst is None and RE_INDENTED.match(raw):
            fail(n, "an indented code block is not supported; use a labelled fence")
            continue
        if lst is not None:
            blocks.append(lst)    # a line at column 0 under a list that is not an item ends the list
            lst = None
        if para is None:
            para = {"kind": "para", "line": n, "text": text}
        else:
            para["text"] += " " + text
    close()
    return blocks, findings


def analyze(blocks, draft):
    """Arrange the blocks into the email's shape and report what the shape forbids: one `#`
    title first, an intro, `##` sections, and after the one rule a footer of paragraphs. A
    draft carries no rule: the footer is the workflow's."""
    doc = {"title": "", "title_line": 1, "intro": [], "sections": [], "footer": []}
    findings = []
    in_footer = False
    if not blocks or blocks[0]["kind"] != "heading" or blocks[0]["level"] != 1:
        findings.append((1, "the first line must be the `# ` title the workflow gave"))
    for i, b in enumerate(blocks):
        if in_footer:
            if b["kind"] == "para":
                doc["footer"].append(b)
            elif b["kind"] == "rule":
                findings.append((b["line"], "a second rule; the footer is everything after the first"))
            else:
                findings.append((b["line"], "the footer may carry paragraphs only"))
        elif b["kind"] == "rule":
            if draft:
                findings.append((b["line"], "the draft carries a rule (---); the footer is the workflow's and goes under the draft later"))
            in_footer = True
        elif b["kind"] == "heading" and b["level"] == 1:
            if i == 0:
                doc["title"], doc["title_line"] = b["text"], b["line"]
            else:
                findings.append((b["line"], "a second # heading; the email has one title"))
        elif b["kind"] == "heading" and b["level"] == 2:
            doc["sections"].append({"heading": b["text"], "line": b["line"], "blocks": []})
        elif doc["sections"]:
            doc["sections"][-1]["blocks"].append(b)
        else:
            if b["kind"] == "heading":
                findings.append((b["line"], "a ### heading before the first ## section"))
            doc["intro"].append(b)
    return doc, findings


# ---- inline --------------------------------------------------------------------------

def safe_url(url):
    """True for an absolute https address with nothing odd in it."""
    if any(c.isspace() or c in "\"'<>\\`" for c in url):
        return False
    try:
        parts = urllib.parse.urlsplit(url)
        return parts.scheme == "https" and bool(parts.hostname) and "@" not in parts.netloc
    except ValueError:
        return False


def inline(src, line, findings, link_style=ST_LINK, links=True):
    """The inline Markdown of a heading, a paragraph or a bullet: **bold**, *emphasis*,
    `code`, [text](https://…), <https://…>, a bare https:// address and backslash escapes.
    Everything else is text, HTML-escaped. A link that is not absolute https is a finding and
    is shown as its text alone. links=False is for a link's own text, which holds no second
    link, and for the plain words of a heading."""
    out, text, i = [], [], 0

    def flush():
        if text:
            out.append(autolink("".join(text), link_style) if links else html.escape("".join(text)))
            text.clear()

    def link(url, label):
        label = label or html.escape(url)
        if links and safe_url(url):
            return f'<a href="{html.escape(url)}" style="{link_style}">{label}</a>'
        if links:
            findings.append((line, f'the link "{url}" is not an absolute https URL'))
        return label

    while i < len(src):
        c = src[i]
        if c == "\\" and i + 1 < len(src) and src[i + 1] in ESCAPABLE:
            text.append(src[i + 1])
            i += 2
        elif c == "`":
            ticks = len(src[i:]) - len(src[i:].lstrip("`"))
            end = src.find("`" * ticks, i + ticks)
            while end > 0 and end + ticks < len(src) and src[end + ticks] == "`":
                end = src.find("`" * ticks, end + 1)    # a longer run is not the closing one
            if end < 0:
                text.append("`" * ticks)
                i += ticks
                continue
            code = src[i + ticks:end]
            if ticks > 1 and code.startswith(" ") and code.endswith(" ") and code.strip():
                code = code[1:-1]
            flush()
            out.append(f'<code style="{ST_CODE}">{html.escape(code)}</code>')
            i = end + ticks
        elif src.startswith("***", i) and src.find("***", i + 3) > i + 3:
            end = src.find("***", i + 3)
            flush()
            out.append("<strong><em>" + inline(src[i + 3:end], line, findings, link_style, links) + "</em></strong>")
            i = end + 3
        elif src.startswith("**", i) and src.find("**", i + 2) > i + 2:
            end = src.find("**", i + 2)
            flush()
            out.append("<strong>" + inline(src[i + 2:end], line, findings, link_style, links) + "</strong>")
            i = end + 2
        elif c in "*_" and i + 1 < len(src) and src[i + 1] not in " *_" and (c == "*" or i == 0 or not is_word(src[i - 1])):
            end = src.find(c, i + 1)
            if end < 0 or src[end - 1] == " " or (c == "_" and end + 1 < len(src) and is_word(src[end + 1])):
                text.append(c)
                i += 1
                continue
            flush()
            out.append("<em>" + inline(src[i + 1:end], line, findings, link_style, links) + "</em>")
            i = end + 1
        elif c == "[" or src.startswith("![", i):
            start = i + (c == "!")
            label_end = src.find("]", start)
            url_end = closing(src, label_end + 2) if label_end > 0 and src.startswith("](", label_end) else -1
            if url_end < 0:
                text.append(c)
                i += 1
                continue
            url = src[label_end + 2:url_end].strip()
            flush()
            label = inline(src[start + 1:label_end], line, findings, link_style, links=False)
            if c == "!":
                findings.append((line, "a picture is not supported; the email reads the same with images off"))
                out.append(label)
            else:
                out.append(link(url, label))
            i = url_end + 1
        elif src.startswith("<https://", i) and src.find(">", i) > 0:
            end = src.find(">", i)
            flush()
            out.append(link(src[i + 1:end], html.escape(src[i + 1:end])))
            i = end + 1
        else:
            text.append(c)
            i += 1
    flush()
    return "".join(out)


def closing(src, start):
    """Where the ) that ends a link's address is, counting brackets inside the address; -1
    when it is not within 2,000 characters, which no address is."""
    depth = 0
    for at in range(start, min(len(src), start + 2000)):
        if src[at] == "(":
            depth += 1
        elif src[at] == ")":
            if not depth:
                return at
            depth -= 1
    return -1


def autolink(text, link_style):
    """Escape text and turn each bare https:// address in it into a link. Trailing
    punctuation stays text."""
    out, last = [], 0
    for m in RE_BARE_URL.finditer(text):
        url = m.group(0).rstrip(".,;:!?)")
        out.append(html.escape(text[last:m.start()]))
        shown = html.escape(url)
        out.append(f'<a href="{shown}" style="{link_style}">{shown}</a>' if safe_url(url) else shown)
        last = m.start() + len(url)
    out.append(html.escape(text[last:]))
    return "".join(out)


def is_word(c):
    return c.isalnum() or c == "_"


def plain(src):
    """The words of a heading, a paragraph or a bullet, without their Markdown."""
    return html.unescape(re.sub(r"<[^>]+>", "", inline(src, 0, [], links=False)))


# ---- layout --------------------------------------------------------------------------

def block(b, paragraph, findings):
    """One block as HTML; paragraph is the paragraph style of the place."""
    if b["kind"] == "heading":
        return f'<h3 style="{ST_H3}">{inline(lead(b["text"]), b["line"], findings)}</h3>\n'
    if b["kind"] == "para":
        return f'<p style="{paragraph}">{inline(b["text"], b["line"], findings)}</p>\n'
    if b["kind"] == "list":
        items = "".join(f'<li style="{ST_LI}">{inline(item, b["line"], findings)}</li>\n' for item in b["items"])
        return f'<ul style="{ST_UL}">\n{items}</ul>\n'
    label = f'<p style="{ST_TERM_LABEL}">{html.escape(b["info"])}</p>\n' if b["info"] else ""
    return (f'{TABLE} style="margin:4px 0 14px"><tr><td style="{ST_TERM}">\n{label}'
            f'<pre style="{ST_TERM_CODE}">{html.escape(b["code"])}</pre>\n</td></tr></table>\n')


def lead(text):
    """A group's lead as a heading: no full stop or colon after it."""
    return text.rstrip(" .:")


def preheader(doc):
    """The hidden first line an inbox shows after the subject: the `###` leads, like a table
    of contents; the first sentence under the title when there are none. At most 140
    characters."""
    leads = [plain(lead(b["text"])) for s in doc["sections"] for b in s["blocks"] if b["kind"] == "heading"]
    firsts = [b["text"] if b["kind"] == "para" else b["items"][0]
              for b in doc["intro"] + [b for s in doc["sections"] for b in s["blocks"]]
              if b["kind"] in ("para", "list")]
    text = " · ".join(leads) or (plain(firsts[0]) if firsts else "")
    if len(text) > 140:
        cut = text.rfind(" ", 0, 140)
        text = text[:cut if cut >= 60 else 140].rstrip(" ·,;") + "…"
    return text


def layout(doc, date):
    """The HTML of the arranged email, and the findings the inline Markdown raised."""
    findings = []
    sent = f"{MONTHS[date.month - 1]} {date.day}, {date.year}" if date else ""
    pre = preheader(doc)
    title = doc["title"] or NAME

    o = ['<!doctype html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n']
    o.append('<meta name="viewport" content="width=device-width, initial-scale=1">\n')
    o.append('<meta name="color-scheme" content="light">\n<meta name="supported-color-schemes" content="light">\n')
    o.append(f"<title>{html.escape(plain(title))}</title>\n</head>\n")
    o.append(f'<body style="{ST_BODY}">\n')
    if pre:
        o.append(f'<div style="{ST_PREHEADER}">{html.escape(pre)}</div>\n')
    o.append(f'{TABLE} style="background:{PAGE}">\n<tr><td align="center" style="padding:28px 12px 36px;background:{PAGE}">\n')
    o.append('<table role="presentation" cellpadding="0" cellspacing="0" border="0" width="600" style="width:100%;max-width:600px;table-layout:fixed">\n')

    # The masthead: outside the card, small. Words only, so it reads the same with pictures off.
    o.append(f'<tr><td style="padding:0 4px 16px">\n{TABLE}><tr>\n')
    o.append(f'<td style="{ST_MAST_NAME}"><a href="{HOME}" style="color:{ACCENT};text-decoration:none">{NAME}</a></td>\n')
    o.append(f'<td align="right" style="{ST_MAST_DATE}">Release notes{" &middot; " + sent if sent else ""}</td>\n')
    o.append("</tr></table>\n</td></tr>\n")

    # The card: the title, the intro, then the sections.
    o.append(f'<tr><td style="{ST_CARD}">\n')
    o.append(f'<h1 style="{ST_H1}">{inline(title, doc["title_line"], findings)}</h1>\n')
    o.extend(block(b, ST_INTRO, findings) for b in doc["intro"])
    for s in doc["sections"]:
        if s["heading"].lower().rstrip(" .:") == CALLOUT:
            o.append(f'{TABLE} style="margin:24px 0 0"><tr><td style="{ST_NOTE}">\n')
            o.append(f'<h2 style="{ST_NOTE_H2}">{inline(s["heading"], s["line"], findings)}</h2>\n')
            o.extend(block(b, ST_P, findings) for b in s["blocks"])
            o.append("</td></tr></table>\n")
            continue
        o.append(f'{TABLE} style="margin:22px 0 0"><tr><td style="{ST_RULE}">&nbsp;</td></tr></table>\n')
        o.append(f'<h2 style="{ST_H2}">{inline(s["heading"], s["line"], findings)}</h2>\n')
        o.extend(block(b, ST_P, findings) for b in s["blocks"])
    o.append("</td></tr>\n")

    # The footer: small and grey, outside the card; the paragraphs after the rule.
    if doc["footer"]:
        o.append(f'<tr><td style="{ST_FOOTER}">\n')
        o.extend(f'<p style="{ST_FOOTER_P}">{inline(b["text"], b["line"], findings, ST_FOOTER_A)}</p>\n' for b in doc["footer"])
        o.append("</td></tr>\n")
    o.append("</table>\n</td></tr>\n</table>\n</body>\n</html>\n")
    return "".join(o), findings


# ---- the two entry points ------------------------------------------------------------

def check(src, draft=True):
    """Every finding of a draft, sorted by line; empty when it renders as the email."""
    if len(src) > MAX_NOTES:
        return [(1, f"the notes are {len(src)} characters; the layout takes up to {MAX_NOTES}")]
    blocks, findings = parse(RE_DROPPED.sub("", src))
    doc, more = analyze(blocks, draft)
    findings += more
    if not findings:
        _, more = layout(doc, None)    # a dry run: the inline findings, a relative link say, surface here
        findings += more
    return sorted(set(findings))


def render(src, date=None):
    """The email of the finished Markdown, footer included. Raises ValueError, naming every
    finding, when it does not render or the email is too long."""
    findings = check(src, draft=False)
    if findings:
        raise ValueError("\n".join(f"line {n}: {msg}" for n, msg in findings))
    blocks, _ = parse(RE_DROPPED.sub("", src))
    doc, _ = analyze(blocks, draft=False)
    page, _ = layout(doc, date)
    if len(page.encode("utf-8")) > MAX_BYTES:
        raise ValueError(f"the email is {len(page.encode('utf-8'))} bytes; Gmail clips a message over {MAX_BYTES}")
    return page


def main(argv):
    ap = argparse.ArgumentParser(description="Lay out the weekly release-notes email, or check a draft against its shape.")
    ap.add_argument("--in", dest="src", metavar="MARKDOWN", required=True, help="the email, or the draft to check")
    ap.add_argument("--out", metavar="HTML", help="where to write the email (not with --check)")
    ap.add_argument("--check", action="store_true", help="read a draft and print every finding, one a line; write nothing")
    ap.add_argument("--date", help="the day the email goes out, YYYY-MM-DD, for the masthead (default: today, UTC)")
    args = ap.parse_args(argv)
    try:
        with open(args.src, encoding="utf-8") as f:
            src = f.read()
    except OSError as e:
        print(f"release-notes-email: {e}", file=sys.stderr)
        return 1
    if args.check:
        findings = check(src, draft=True)
        for n, msg in findings:
            print(f"line {n}: {msg}")
        return 1 if findings else 0
    if not args.out:
        ap.error("--out is required unless --check is given")
    try:
        date = datetime.date.fromisoformat(args.date) if args.date else datetime.datetime.now(datetime.timezone.utc).date()
        page = render(src, date)
        with open(args.out, "w", encoding="utf-8") as f:
            f.write(page)
    except (ValueError, OSError) as e:
        print(f"release-notes-email: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
