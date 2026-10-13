#!/usr/bin/env python3
"""lint-docs: the documentation gate CLAUDE.md names for invariant 6 (ADR 0006).

Two modes:

  lint-docs.py          every check below, including the change check over a diff range
  lint-docs.py links    the pages, links and ADR checks only; what `just docs` runs

Checks:

  pages   every page under docs/ is listed in docs/SUMMARY.md, and every page SUMMARY.md
          lists exists, so the book has no orphan and no hole
  links   every relative link in docs/, README.md and crates/*/README.md resolves to a
          file or directory, and every anchor names a heading in its target
  adrs    every file in docs/decisions/ is NNNN-title.md with a matching "# NNNN." title
          and a Status of Proposed, Accepted, Superseded by NNNN or Rejected; numbers
          are unique
  glossary
          the first dotted segment of every counter name in counters.toml, underscores
          read as spaces, is a term in docs/glossary.md or the first word of one
          (`counters.allocated` needs "counters file", `log_buffer.rejected_frame` needs
          "log buffer"), so every counter name has a row; and the `term` of every key in
          config.toml is a term in docs/glossary.md (.claude/rules/docs.md, Glossary;
          CLAUDE.md, invariant 6)
  change  over the range: a change to a public item (a line with `pub ` in a crate's
          src/), to spec/, counters.toml or config.toml needs a change under docs/ or to
          a README; a change to spec/ needs an ADR in the same change whose Status is
          Accepted; an ADR that was already Accepted may change only its Status line

The range: $LINT_DOCS_RANGE if set; in CI, origin/<base>...HEAD for a pull request and
<before>..<after> for a push (HEAD~1..HEAD when <before> is the null commit); on a
laptop, HEAD against the working tree when anything is modified or untracked, else
HEAD~1..HEAD, which checks the commit just made.

Standard library only, like scripts/lint-workspace.py (ADR 0003).
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"
SUMMARY = DOCS / "SUMMARY.md"
DECISIONS = DOCS / "decisions"
GLOSSARY = DOCS / "glossary.md"
COUNTERS = ROOT / "counters.toml"
CONFIG = ROOT / "config.toml"

STATUSES = re.compile(r"^- \*\*Status:\*\* (Proposed|Accepted|Superseded by \d{4}|Rejected)\s*$")
ADR_NAME = re.compile(r"^(\d{4})-[a-z0-9-]+\.md$")
LINK = re.compile(r"\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
REF_DEF = re.compile(r"^\s*\[[^\]]+\]:\s*(\S+)", re.M)
PUB_LINE = re.compile(r"^[+-]\s*pub\s")
SCHEME = re.compile(r"^[a-z][a-z0-9+.-]*:")

findings: list[str] = []


def find(msg: str) -> None:
    findings.append(msg)


def git(*args: str, check: bool = True) -> str:
    p = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True)
    if check and p.returncode != 0:
        sys.exit(f"lint-docs: git {' '.join(args)} failed: {p.stderr.strip()}")
    return p.stdout


def strip_code(text: str) -> str:
    """Drop fenced blocks and inline code: links and headings inside them are not links."""
    text = re.sub(r"^```.*?^```[ \t]*$", "", text, flags=re.S | re.M)
    text = re.sub(r"^~~~.*?^~~~[ \t]*$", "", text, flags=re.S | re.M)
    return re.sub(r"`[^`\n]*`", "", text)


def canon(s: str) -> str:
    """Anchor comparison that survives GitHub's and mdBook's slug differences."""
    return re.sub(r"[^a-z0-9]", "", s.lower())


def headings(path: Path) -> set[str]:
    out: set[str] = set()
    for line in strip_code(path.read_text(encoding="utf-8")).splitlines():
        m = re.match(r"^#{1,6}\s+(.*?)\s*#*\s*$", line)
        if m:
            title = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", m.group(1))
            out.add(canon(title))
    return out


def rel(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


# ---- pages ------------------------------------------------------------------------

def check_pages() -> int:
    if not SUMMARY.exists():
        find("pages: docs/SUMMARY.md is missing")
        return 0
    listed: set[Path] = set()
    for target in LINK.findall(strip_code(SUMMARY.read_text(encoding="utf-8"))):
        target = target.split("#", 1)[0]
        if not target or SCHEME.match(target):
            continue
        p = (SUMMARY.parent / target).resolve()
        listed.add(p)
        if not p.is_file():
            find(f"pages: docs/SUMMARY.md lists {target}, which does not exist")
    pages = sorted(p for p in DOCS.rglob("*.md") if p != SUMMARY)
    for p in pages:
        if p.resolve() not in listed:
            find(f"pages: {rel(p)} is not listed in docs/SUMMARY.md")
    return len(pages)


# ---- links ------------------------------------------------------------------------

def link_sources() -> list[Path]:
    srcs = sorted(DOCS.rglob("*.md")) + [ROOT / "README.md"]
    srcs += sorted((ROOT / "crates").glob("*/README.md"))
    return [p for p in srcs if p.is_file()]


def check_links() -> int:
    n = 0
    for src in link_sources():
        text = strip_code(src.read_text(encoding="utf-8"))
        targets = LINK.findall(text) + REF_DEF.findall(text)
        for target in targets:
            if SCHEME.match(target) or target.startswith("<"):
                continue
            n += 1
            path_part, _, anchor = target.partition("#")
            dest = src if not path_part else (src.parent / path_part).resolve()
            if not dest.exists():
                find(f"links: {rel(src)} links to {target}, which does not exist")
                continue
            if anchor:
                if dest.is_dir() or dest.suffix != ".md":
                    find(f"links: {rel(src)} links to {target}: an anchor on a non-page")
                elif canon(anchor) not in headings(dest):
                    find(f"links: {rel(src)} links to {target}: no heading matches #{anchor}")
    return n


# ---- adrs -------------------------------------------------------------------------

def adr_status(text: str) -> str | None:
    for line in text.splitlines():
        m = STATUSES.match(line)
        if m:
            return m.group(1)
    return None


def check_adrs() -> int:
    seen: dict[str, Path] = {}
    files = sorted(p for p in DECISIONS.glob("*.md") if p.name != "0000-template.md")
    for p in files:
        m = ADR_NAME.match(p.name)
        if not m:
            find(f"adrs: {rel(p)} is not named NNNN-short-title.md")
            continue
        num = m.group(1)
        if num in seen:
            find(f"adrs: {rel(p)} reuses number {num} of {rel(seen[num])}")
        seen[num] = p
        text = p.read_text(encoding="utf-8")
        first = text.splitlines()[0] if text else ""
        if not first.startswith(f"# {num}. "):
            find(f"adrs: {rel(p)} first line must start with '# {num}. '")
        if adr_status(text) is None:
            find(f"adrs: {rel(p)} has no '- **Status:**' line with a valid status")
    return len(files)


# ---- glossary ---------------------------------------------------------------------

def glossary_terms() -> set[str]:
    """The first column of the glossary table, lowercased, each alternative of a
    "claim / commit" row on its own."""
    terms: set[str] = set()
    if not GLOSSARY.exists():
        return terms
    for line in GLOSSARY.read_text(encoding="utf-8").splitlines():
        m = re.match(r"^\|\s*([^|]+?)\s*\|", line)
        if not m:
            continue
        cell = m.group(1).strip().strip("`").lower()
        if cell in ("cavalry term", "---") or not cell:
            continue
        for term in re.split(r"\s*/\s*", cell):
            terms.add(term.strip())
    return terms


def check_glossary() -> tuple[int, int]:
    terms = glossary_terms()
    first_words = {t.split()[0] for t in terms if t}
    names: list[str] = []
    if COUNTERS.exists():
        names = re.findall(r'^name\s*=\s*"([^"]+)"', COUNTERS.read_text(encoding="utf-8"), re.M)
    for name in names:
        segment = name.split(".", 1)[0].replace("_", " ")
        if segment not in terms and segment not in first_words:
            find(f"glossary: counter `{name}` begins with `{segment}`, which is not a term in "
                 "docs/glossary.md or the first word of one; add a row (.claude/rules/docs.md, "
                 "Glossary)")
    config_terms: list[str] = []
    if CONFIG.exists():
        text = CONFIG.read_text(encoding="utf-8")
        keys = re.findall(r'^name\s*=\s*"([^"]+)"', text, re.M)
        config_terms = re.findall(r'^term\s*=\s*"([^"]+)"', text, re.M)
        for key, term in zip(keys, config_terms):
            if term.lower() not in terms:
                find(f"glossary: config key `{key}` names the term `{term}`, which is not a row "
                     "in docs/glossary.md; add the row, or name the one that defines what the "
                     "key sets (.claude/rules/docs.md, Glossary)")
    return len(names), len(config_terms)


# ---- change -----------------------------------------------------------------------

def pick_range() -> tuple[str | None, str | None]:
    """Return (base, head); head None means the working tree."""
    env = os.environ.get("LINT_DOCS_RANGE")
    if env:
        return split_range(env)
    event = os.environ.get("GITHUB_EVENT_NAME")
    if event == "pull_request" or event == "pull_request_target":
        return f"origin/{os.environ['GITHUB_BASE_REF']}", "HEAD"
    if event == "push":
        before = None
        path = os.environ.get("GITHUB_EVENT_PATH")
        if path and Path(path).exists():
            before = json.loads(Path(path).read_text()).get("before")
        if not before or set(before) == {"0"}:
            return "HEAD~1", "HEAD"
        return before, "HEAD"
    if event:
        return "HEAD~1", "HEAD"
    dirty = git("status", "--porcelain", "--untracked-files=all").strip()
    if dirty:
        return "HEAD", None
    return "HEAD~1", "HEAD"


def split_range(spec: str) -> tuple[str, str]:
    if "..." in spec:
        a, b = spec.split("...", 1)
        return git("merge-base", a, b or "HEAD").strip(), b or "HEAD"
    if ".." in spec:
        a, b = spec.split("..", 1)
        return a, b or "HEAD"
    return spec, "HEAD"


def changed_files(base: str, head: str | None) -> list[str]:
    if head is None:
        files = set(git("diff", "--name-only", base).split())
        files |= set(git("ls-files", "--others", "--exclude-standard").split())
        return sorted(files)
    if base.startswith("origin/") or "..." not in base:
        pass
    return sorted(set(git("diff", "--name-only", base, head).split()))


def file_diff(base: str, head: str | None, path: str) -> str:
    if head is None:
        if git("ls-files", "--error-unmatch", path, check=False) == "" and not (ROOT / path).exists():
            return ""
        tracked = subprocess.run(["git", "ls-files", "--error-unmatch", path], cwd=ROOT,
                                 capture_output=True).returncode == 0
        if not tracked:
            p = ROOT / path
            return "".join("+" + line for line in p.read_text(encoding="utf-8", errors="replace").splitlines(True))
        return git("diff", base, "--", path)
    return git("diff", base, head, "--", path)


def content_at(rev: str | None, path: str) -> str | None:
    if rev is None:
        p = ROOT / path
        return p.read_text(encoding="utf-8") if p.exists() else None
    p = subprocess.run(["git", "show", f"{rev}:{path}"], cwd=ROOT, capture_output=True, text=True)
    return p.stdout if p.returncode == 0 else None


def check_change() -> tuple[str, int]:
    base, head = pick_range()
    label = f"{base}..{head or 'working tree'}"
    if git("rev-parse", "--verify", "--quiet", base, check=False).strip() == "":
        find(f"change: base {base} is not a commit; set LINT_DOCS_RANGE")
        return label, 0
    files = changed_files(base, head)
    code_triggers: list[str] = []
    for f in files:
        if f.endswith(".rs") and f.startswith("crates/") and "/src/" in f:
            if any(PUB_LINE.match(line) for line in file_diff(base, head, f).splitlines()):
                code_triggers.append(f"{f} (public item)")
        elif f.startswith("spec/") or f in ("counters.toml", "config.toml"):
            code_triggers.append(f)
    docs_changed = [f for f in files if f.startswith("docs/") or Path(f).name == "README.md"]
    if code_triggers and not docs_changed:
        find("change: " + ", ".join(code_triggers) + " changed with no change under docs/ or "
             "to a README (CLAUDE.md, invariant 6: docs ship in the same commit)")
    if any(f.startswith("spec/") for f in files):
        accepted = [f for f in files if f.startswith("docs/decisions/") and f.endswith(".md")
                    and (content_at(head, f) or "") and adr_status(content_at(head, f) or "") == "Accepted"]
        if not accepted:
            find("change: spec/ changed with no ADR in the same change whose Status is Accepted "
                 "(CLAUDE.md, Protocol: spec first, with an accepted ADR)")
    for f in files:
        if not (f.startswith("docs/decisions/") and ADR_NAME.match(Path(f).name)):
            continue
        before = content_at(base, f)
        after = content_at(head, f)
        if before is None or after is None or adr_status(before) != "Accepted":
            continue
        strip = lambda t: [l for l in t.splitlines() if not STATUSES.match(l)]
        if strip(before) != strip(after):
            find(f"change: {f} was Accepted at {base} and changed beyond its Status line; "
                 "accepted ADRs are immutable, write a new one (.claude/rules/docs.md)")
    return label, len(files)


# ---- main -------------------------------------------------------------------------

def main(argv: list[str]) -> int:
    mode = argv[1] if len(argv) > 1 else "all"
    if mode not in ("all", "links"):
        sys.exit("usage: lint-docs.py [all|links]")
    pages = check_pages()
    links = check_links()
    adrs = check_adrs()
    counters, keys = check_glossary()
    summary = f"{pages} pages, {links} links, {adrs} ADRs, {counters} counters, {keys} config keys"
    if mode == "all":
        label, n = check_change()
        summary += f", {n} files changed in {label}"
    if findings:
        for f in findings:
            print(f"lint-docs: {f}")
        print(f"lint-docs: {summary}, {len(findings)} finding(s)")
        return 1
    print(f"lint-docs: {summary}, ok")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
