#!/usr/bin/env python3
"""fixer-spec-pages: which files under spec/ the loop's agent may write in this run
(ADR 0140; work item 340).

The agent's job in .github/workflows/claude.yml denied every write under spec/. A
blocker of a review that was under spec/ could then never be fixed by the fixer: on
pull request 300 three fix cycles went to three sentences of spec/cluster.md, the cap
was reached, and a person's session applied them. The owner ruled on 2026-11-21 that
the agent may write a spec/ page when the pull request's branch holds an Accepted ADR
that names that page.

This script reads the checkout and prints the deny entries for spec/, as the text the
workflow puts into the agent's settings:

- No ADR that the branch adds is Accepted and names a page: every write under spec/
  is denied, as before: `"Write(spec/**)", "Edit(spec/**)",`.
- Such an ADR names `spec/<page>.md`: that page is writable, and so are its vectors,
  `spec/vectors/<page>.py` and `spec/vectors/<page>.json`, and `spec/README.md`, whose
  table holds the page's version and its ADRs. Every other file under spec/ that the
  head or the base holds is denied by name, since a permission rule has no "all but".

A branch is the commits of HEAD that the base does not have. The base is
`refs/remotes/origin/master`, by its full name: a tag that somebody named
`origin/master` would be read before the branch by the short name. An ADR counts when
it is a file under docs/decisions/ that the branch adds, its first Status line reads
exactly Accepted, and its text names the page as `spec/<page>.md`, anywhere in it. An
ADR that `master` already has gives nothing, changed or not: the page it names was
decided for another change, and an accepted ADR is not edited.

Closed when in doubt. What cannot be read gives no page, and neither does a file under
spec/ whose name holds anything but letters, digits, `.`, `_`, `/` and `-`: such a
name could not be written into the settings as it is.

What this does not close. A file under spec/ that neither the head nor the base holds
is denied only while nothing is writable: once one page is, a new file beside it is on
no list. `just lint-docs` refuses a change under spec/ with no Accepted ADR in the
same change, and the reviewer reads every change. A permission rule is a guard rail,
not a boundary (claude.yml, the comment under `settings`).

The workflow runs master's copy of this script, `git show
refs/remotes/origin/master:scripts/fixer-spec-pages.py`, so that a branch cannot widen
its own permissions by editing it, and takes the answer only when it is deny entries
for spec/ and nothing else. The answer is one line on stdout; what it rests on is on
stderr.

    python3 -I scripts/fixer-spec-pages.py            in the checkout, HEAD against the base
    FIXER_SPEC_BASE=<ref> python3 -I scripts/fixer-spec-pages.py     another base, for the tests

Python 3.9 standard library only (ADR 0003).
"""

from __future__ import annotations

import os
import re
import subprocess
import sys

DENY_ALL = '"Write(spec/**)", "Edit(spec/**)",'
BASE = "refs/remotes/origin/master"
# When every file under spec/ is writable, which no real branch reaches, the list still
# needs an entry: an empty answer would read as no answer, and the workflow denies all.
NOTHING_ELSE = '"Write(spec/.nothing-else)", "Edit(spec/.nothing-else)",'
STATUS = re.compile(r"^- \*\*Status:\*\*[ \t]*(.*?)[ \t]*$", re.M)
PAGE = re.compile(r"\bspec/([A-Za-z0-9][A-Za-z0-9_-]*)\.md\b")
NAME = re.compile(r"^[A-Za-z0-9._/-]+$")


def git(*args: str) -> str:
    done = subprocess.run(("git",) + args, capture_output=True, text=True)
    if done.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {done.stderr.strip()}")
    return done.stdout


def listed(*args: str) -> list[str]:
    """The names a git command prints with `-z`, as they are, never quoted."""
    return [name for name in git(*args).split("\0") if name]


def added_adrs(base: str) -> list[str]:
    """The files under docs/decisions/ that the branch adds."""
    out = listed("diff", "--name-only", "-z", "--diff-filter=A", f"{base}...HEAD", "--", "docs/decisions/")
    return sorted(name for name in out if name.endswith(".md"))


def accepted(text: str) -> bool:
    """Whether the ADR's first Status line reads exactly Accepted, as `just lint-docs` reads it."""
    first = STATUS.search(text)
    return bool(first) and first.group(1) == "Accepted"


def named_pages(adr: str, names: set[str]) -> set[str]:
    """The pages under spec/ that an Accepted ADR at HEAD names, by their stems."""
    text = git("show", f"HEAD:{adr}")
    if not accepted(text):
        return set()
    return {stem for stem in PAGE.findall(text) if f"spec/{stem}.md" in names}


def spec_files(base: str) -> set[str]:
    """Every file under spec/ that the index, the head or the base holds: a page the branch
    deleted is still one to deny."""
    names = set(listed("ls-files", "-z", "--", "spec/"))
    for tree in ("HEAD", base):
        names |= set(listed("ls-tree", "-r", "-z", "--name-only", tree, "--", "spec/"))
    for name in sorted(names):
        if not NAME.match(name):
            raise RuntimeError(f"a file under spec/ has a name the settings cannot hold: {name!r}")
    return names


def writable(stems: set[str]) -> set[str]:
    # The table of pages, versions and ADRs: a page's change moves its row.
    files = {"spec/README.md"} if stems else set()
    for stem in stems:
        files |= {f"spec/{stem}.md", f"spec/vectors/{stem}.py", f"spec/vectors/{stem}.json"}
    return files


def entries(base: str) -> tuple[str, list[str]]:
    """The deny entries for spec/, and the lines that say what they rest on."""
    names = spec_files(base)
    said, stems = [], set()
    for adr in added_adrs(base):
        pages = named_pages(adr, names)
        if pages:
            said.append(f"{adr} is Accepted and names " + ", ".join(f"spec/{s}.md" for s in sorted(pages)))
            stems |= pages
    if not stems:
        said.append(f"no ADR that this branch adds over {base} is Accepted and names a page of spec/: "
                    "every write under spec/ is denied")
        return DENY_ALL, said
    allowed = writable(stems) & names
    denied = sorted(names - allowed)
    said.append("writable: " + ", ".join(sorted(allowed)))
    said.append(f"denied by name: the other {len(denied)} files under spec/")
    if not denied:
        return NOTHING_ELSE, said
    return "".join(f'"Write({f})", "Edit({f})", ' for f in denied).rstrip(), said


def main() -> int:
    base = os.environ.get("FIXER_SPEC_BASE", BASE)
    try:
        answer, said = entries(base)
    except (RuntimeError, UnicodeError) as error:
        # Closed, not open: what cannot be read gives no page.
        answer, said = DENY_ALL, [f"could not read the branch ({error}): every write under spec/ is denied"]
    for line in said:
        print(f"fixer-spec-pages: {line}", file=sys.stderr)
    print(answer)
    return 0


if __name__ == "__main__":
    sys.exit(main())
