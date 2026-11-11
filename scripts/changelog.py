#!/usr/bin/env python3
"""changelog: a change's changelog entry is a file of its own (ADR 0075; card 255).

    python3 scripts/changelog.py check     what `just lint-docs` runs
    python3 scripts/changelog.py fold      at a release, or to read the file whole

Every change used to append its entry at the end of CHANGELOG.md's Unreleased section,
so of two open pull requests the second to land conflicted with the first, whatever
else they changed, and went round again: merge, gates, review, bench. An entry is now
a file under `changelog.d/`, which no other change touches:

    changelog.d/<card or pull request>-<a few words>.md

The name begins with the card's number, or the pull request's when there is no card,
so that two changes do not choose one name. The file holds what would have been
appended, as it would have been written: one or more entries, each a line that begins
`- ` with its continuation lines indented two spaces, in the reader's words, with its
links written from the repository's root, as CHANGELOG.md's are. Nobody appends to
CHANGELOG.md between releases, and `scripts/lint-docs.py` fails a change that does.

`check` reads every file of `changelog.d/` but its README, and but a file whose name
begins with a dot or ends with `~`, which an editor or a file manager leaves, and
fails on a name or a shape that `fold` could not place, or a link that names no file:
see [`problems`].

`fold` puts the entries at the end of CHANGELOG.md's Unreleased section, which is the
end of the file until a release gives it a section below, in the order the files
landed, and removes the files. A file landed with the newest commit on the branch's
first-parent history that added it, so a branch's own commits do not count and a name
used again after an earlier fold counts from its second landing; a file no commit
holds follows the rest, by name. It changes the working tree and commits nothing: the
release's commit holds the fold. A second run finds no file and changes nothing.
CHANGELOG.md keeps every line it had. It refuses, and changes nothing, in a shallow
checkout, where the order cannot be read, when an entry is malformed, and when
CHANGELOG.md has no `## Unreleased` section.

Standard library only; scripts/changelog-test.py tests it, and `just test-scripts`
runs that.
"""
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIRECTORY = "changelog.d"
CHANGELOG = "CHANGELOG.md"
README = "README.md"
NAME = re.compile(r"[0-9]+-[0-9a-z][0-9a-z-]*\.md")
LINK = re.compile(r"\]\(([^)\s]+)\)")
UNRELEASED = "## Unreleased"
DOCS = "changelog.d/README.md"


def fragments(root):
    """The entry files of `root`, by name: everything in changelog.d/ but its README
    and what an editor or a file manager leaves, a name that begins with a dot or
    ends with `~`."""
    directory = root / DIRECTORY
    if not directory.is_dir():
        return []
    return sorted(path for path in directory.iterdir()
                  if path.name != README and not path.name.startswith(".") and not path.name.endswith("~"))


def problems(root, path):
    """What is wrong with one entry file, in words, or nothing."""
    if not path.is_file():
        return [f"{path.name} is not a file: an entry is one file, {DIRECTORY}/<card>-<a few words>.md"]
    found = []
    if not NAME.fullmatch(path.name):
        found.append(f"{path.name}: the name is the card's number, or the pull request's, a hyphen, then "
                     f"lower-case letters, digits and hyphens, then .md, as in 255-changelog-entries-as-files.md")
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as e:
        return found + [f"{path.name} could not be read as UTF-8: {e}"]
    lines = text.split("\n")
    if not text.endswith("\n"):
        found.append(f"{path.name}: the last line has no line break, and the next entry would join it")
    lines = lines[:-1] if text.endswith("\n") else lines
    if not lines or not lines[0].startswith("- "):
        found.append(f"{path.name}: the first line begins `- `, as an entry under Unreleased does")
    for number, line in enumerate(lines, start=1):
        if line != line.rstrip():
            found.append(f"{path.name}:{number}: the line ends in white space")
        elif not line:
            found.append(f"{path.name}:{number}: an empty line; entries follow one another without one")
        elif not line.startswith(("- ", "  ")):
            found.append(f"{path.name}:{number}: a line begins `- `, a new entry, or two spaces, the entry's "
                         f"next line")
    # A link is written as CHANGELOG.md will hold it, from the repository's root.
    for target in LINK.findall(text):
        if re.match(r"[a-z][a-z0-9+.-]*:", target) or target.startswith("#"):
            continue
        if not (root / target.split("#")[0]).exists():
            found.append(f"{path.name}: the link to {target} names no file from the repository's root, which "
                         f"is where {CHANGELOG} will read it from")
    return found


def check(root, out=print):
    """Exit status of `check`: 0 when every entry file can be folded as it stands."""
    found = [problem for path in fragments(root) for problem in problems(root, path)]
    for problem in found:
        out(f"changelog: {problem} ({DOCS})")
    count = len(fragments(root))
    if not found:
        out(f"changelog: {count} entr{'y' if count == 1 else 'ies'} in {DIRECTORY}/, ok")
    return 1 if found else 0


def git(root, *args):
    done = subprocess.run(["git", *args], cwd=root, capture_output=True, text=True, check=False)
    return done.stdout if done.returncode == 0 else None


def landed(root, path):
    """When an entry file landed: the time of the newest commit on the branch's
    first-parent history that added it, or None for a file no such commit holds yet."""
    listed = git(root, "log", "--first-parent", "-m", "--diff-filter=A", "--format=%ct", "--",
                 str(path.relative_to(root)))
    times = [int(line) for line in (listed or "").split() if line.isdigit()]
    return max(times) if times else None


def fold(root, out=print):
    """Exit status of `fold`: the entries put at the end of CHANGELOG.md's Unreleased
    section in the order they landed and their files removed, or 1 with nothing
    changed when that cannot be done whole."""
    paths = fragments(root)
    found = [problem for path in paths for problem in problems(root, path)]
    changelog = root / CHANGELOG
    text = changelog.read_text(encoding="utf-8") if changelog.is_file() else None
    lines = text.split("\n") if text is not None else []
    if text is None:
        found.append(f"{CHANGELOG} is not there to fold into")
    elif UNRELEASED not in lines:
        found.append(f"{CHANGELOG} has no `{UNRELEASED}` section to fold into")
    if paths and (git(root, "rev-parse", "--is-shallow-repository") or "").strip() == "true":
        found.append("this checkout is shallow, so the order the entries landed in cannot be read: "
                     "`git fetch --unshallow`, then fold")
    if found:
        for problem in found:
            out(f"changelog: {problem} ({DOCS})")
        out("changelog: nothing was folded")
        return 1
    if not paths:
        out(f"changelog: no entry in {DIRECTORY}/, nothing to fold")
        return 0
    when = {path: landed(root, path) for path in paths}
    order = sorted(paths, key=lambda path: (when[path] is None, when[path] or 0, path.name))
    entries = [line for path in order for line in path.read_text(encoding="utf-8").split("\n")[:-1]]
    # The Unreleased section ends where the next section of its level begins, or with
    # the file; the entries go after its last line that is not empty.
    start = lines.index(UNRELEASED)
    end = next((n for n in range(start + 1, len(lines)) if lines[n].startswith("## ")), len(lines))
    last = end
    while last > start + 1 and not lines[last - 1].strip():
        last -= 1
    lines[last:last] = entries
    folded = "\n".join(lines)
    changelog.write_text(folded if folded.endswith("\n") else folded + "\n", encoding="utf-8")
    for path in order:
        path.unlink()
    out(f"changelog: folded {len(order)} entr{'y' if len(order) == 1 else 'ies'} into {CHANGELOG}'s Unreleased "
        f"section in the order they landed and removed the files; commit both with the release")
    return 0


def main(argv, root=ROOT, out=print):
    if argv == ["check"]:
        return check(root, out)
    if argv == ["fold"]:
        return fold(root, out)
    out("usage: changelog.py check | fold")
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
