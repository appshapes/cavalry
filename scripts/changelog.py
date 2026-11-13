#!/usr/bin/env python3
"""changelog: a change's changelog entry is a file of its own (ADR 0075; card 255).

    python3 scripts/changelog.py check     what `just lint-docs` runs
    python3 scripts/changelog.py fold      at a release

Every change used to append its entry at the end of CHANGELOG.md's Unreleased section,
so of two open pull requests the second to land conflicted with the first, whatever
else they changed, and went round again: merge, gates, review, bench. An entry is now
a file under `changelog.d/`, which no other change touches:

    changelog.d/<work item or pull request>-<a few words>.md

The name begins with the work item's number, or the pull request's when there is no
work item, so that two changes do not choose one name. An entry added before commit
5bbd2ad8, where Cavalry's work moved to the Brigade Board, begins with its Trello
card's number (ADR 0113). The file holds what would have been appended, as it would
have been written: one or more entries, each a line that begins `- ` with its
continuation lines indented two spaces, in the reader's words, with its
links written from the repository's root, as CHANGELOG.md's are.

`check` reads every file of `changelog.d/` but its README, and but a file whose name
begins with a dot or ends with `~`, which an editor or a file manager leaves, and
fails on a name or a shape that `fold` could not place, or a link that names no file:
see [`problems`].

`fold` puts the entries at the end of CHANGELOG.md's Unreleased section, which is the
end of the file until a release gives it a section below, in the order the files
landed, and removes the files. A file landed with the newest commit on the branch's
first-parent history that added it, by that commit's place there and not by its
date, so a branch's own commits do not count and a commit whose clock was wrong
lands where it was pushed. It changes the working tree and commits nothing: the
release's commit holds the fold. A second run finds no file and changes nothing.
CHANGELOG.md keeps every line it had. It refuses, and changes nothing, in a shallow
checkout, where the order cannot be read; when an entry is malformed; when an entry
file is not as HEAD holds it, untracked, changed or staged, since [`grown`] could not
tell its lines from appended ones; and when CHANGELOG.md has no `## Unreleased`
section.

Nobody appends to CHANGELOG.md between releases, and [`grown`] is the gate for it,
which `scripts/lint-docs.py` runs over the range of a change: the file may gain an
entry line as a fold moves it out of the entry files, or as a correction replaces a
line where it stands, and in no other way (ADR 0076).

Standard library only; scripts/changelog-test.py tests it, and `just test-scripts`
runs that.
"""
import collections
import difflib
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


def is_entry_name(name):
    """Whether a path from the root is an entry file's: directly under changelog.d/,
    not its README, and not what an editor or a file manager leaves, a name that
    begins with a dot or ends with `~`."""
    directory, _, base = name.rpartition("/")
    return directory == DIRECTORY and base != README and not base.startswith(".") and not base.endswith("~")


def is_entry_line(line):
    """Whether a line of CHANGELOG.md or of an entry file is an entry's: it begins
    `- ` or two spaces."""
    return line.startswith(("- ", "  "))


def fragments(root):
    """The entry files of `root`, by name: everything in changelog.d/ that
    [`is_entry_name`] passes."""
    directory = root / DIRECTORY
    if not directory.is_dir():
        return []
    return sorted(path for path in directory.iterdir() if is_entry_name(f"{DIRECTORY}/{path.name}"))


def problems(root, path):
    """What is wrong with one entry file, in words, or nothing."""
    if not path.is_file():
        return [f"{path.name} is not a file: an entry is one file, {DIRECTORY}/<work item>-<a few words>.md"]
    found = []
    if not NAME.fullmatch(path.name):
        found.append(f"{path.name}: the name is the work item's number, or the pull request's, a hyphen, then "
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
        elif not is_entry_line(line):
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
    """What a git command printed, or None when it failed. Text is UTF-8 whatever the
    locale, as the working tree's files are read, and a path is printed as it is."""
    done = subprocess.run(["git", "-c", "core.quotePath=false", *args], cwd=root, capture_output=True,
                          encoding="utf-8", errors="replace", check=False)
    return done.stdout if done.returncode == 0 else None


def places(root):
    """{path, from the root: place}: where each file under changelog.d/ landed, the
    newest commit on the branch's first-parent history that added it, numbered from
    the oldest such commit. A place and not a time: a commit made on a laptop whose
    clock is behind lands where it is pushed, not where its date would put it. None
    when the history cannot be read."""
    # --no-renames: a file moved to a new name is an entry added under that name.
    listed = git(root, "log", "--first-parent", "-m", "--no-renames", "--diff-filter=A", "--name-only",
                 "--format=%x00%H", "--", DIRECTORY + "/")
    if listed is None:
        return None
    commits = [block.split("\n") for block in listed.split("\x00") if block.strip()]
    found = {}
    for place, block in enumerate(reversed(commits)):
        for name in block[1:]:
            if name:
                found[name] = place          # a later commit that adds the name again wins
    return found


def uncommitted(root):
    """The paths under changelog.d/ that are not as HEAD holds them: untracked, changed
    or staged. None when git cannot say."""
    listed = git(root, "status", "--porcelain", "--untracked-files=all", "--no-renames", "--", DIRECTORY + "/")
    if listed is None:
        return None
    return {line[3:].strip('"') for line in listed.split("\n") if line}


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
    where = {}
    if paths and (git(root, "rev-parse", "--is-shallow-repository") or "").strip() == "true":
        found.append("this checkout is shallow, so the order the entries landed in cannot be read: "
                     "`git fetch --unshallow`, then fold")
    elif paths:
        landed, loose = places(root), uncommitted(root)
        if landed is None or loose is None:
            found.append("git could not read this checkout's history or its status, so the order the entries "
                         "landed in is not known: run `git status` here, mend what it reports, then fold")
        else:
            where = {path: landed.get(f"{DIRECTORY}/{path.name}") for path in paths}
            for path in paths:
                # The gate reads a fold as the lines that left committed entry files, so
                # the file folded is the file HEAD holds, to the byte.
                if f"{DIRECTORY}/{path.name}" in loose:
                    found.append(f"{path.name} is not as HEAD holds it (`git status` shows it): commit it, then "
                                 f"fold, since the gate reads a fold as the lines that left committed entry files")
                elif where[path] is None:
                    found.append(f"{path.name} was added by no commit on this branch's first-parent history, so "
                                 f"where it landed cannot be read: fold on `master`, where the entry landed")
    if found:
        for problem in found:
            out(f"changelog: {problem} ({DOCS})")
        out("changelog: nothing was folded")
        return 1
    if not paths:
        out(f"changelog: no entry in {DIRECTORY}/, nothing to fold")
        return 0
    order = sorted(paths, key=lambda path: (where[path], path.name))
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


def entry_lines(text):
    """The entry lines of a text, counted."""
    return collections.Counter(line for line in (text or "").split("\n") if is_entry_line(line))


class Unreadable(Exception):
    """A change whose ends git cannot read: [`grown`] has no answer for it."""


def commit_of(root, name):
    """The commit a name stands for, or [`Unreadable`]."""
    found = git(root, "rev-parse", "--verify", "--quiet", f"{name}^{{commit}}")
    if not found or not found.strip():
        raise Unreadable(f"{name} names no commit in this checkout")
    return found.strip()


def files_at(root, commit, path):
    """The files a commit holds directly at `path`, a file or a directory with its
    `/`, by path from the root; [`Unreadable`] when git cannot list them."""
    listed = git(root, "ls-tree", commit, "--", path)
    if listed is None:
        raise Unreadable(f"git could not list {path} at {commit}")
    found = set()
    for line in listed.split("\n"):
        kind, _, name = line.partition("\t")
        if name and kind.split()[1:2] == ["blob"]:
            found.add(name)
    return found


def at(root, commit, path):
    """A file's text at a commit, or in the working tree when `commit` is None; None
    when the file is not there, and [`Unreadable`] when it is and cannot be read."""
    if commit is None:
        target = root / path
        if not target.is_file():
            return None
        try:
            return target.read_text(encoding="utf-8", errors="replace")
        except OSError as e:
            raise Unreadable(f"{path} could not be read: {e}") from e
    if path not in files_at(root, commit, path):
        return None
    text = git(root, "show", f"{commit}:{path}")
    if text is None:
        raise Unreadable(f"git could not read {path} at {commit}")
    return text


def entries_at(root, commit):
    """The entry files a commit holds, or the working tree when `commit` is None: the
    files directly under changelog.d/, by path from the root."""
    if commit is None:
        return {f"{DIRECTORY}/{path.name}" for path in fragments(root) if path.is_file()}
    return {name for name in files_at(root, commit, DIRECTORY + "/") if is_entry_name(name)}


def held(root, commit):
    """Every entry line the entry files hold at a commit, or in the working tree when
    `commit` is None, counted."""
    lines = collections.Counter()
    for name in sorted(entries_at(root, commit)):
        lines += entry_lines(at(root, commit, name))
    return lines


def grown(root, base, head=None):
    """By how many entry lines a change makes CHANGELOG.md grow beyond what a fold and
    a correction account for: 0 when the change keeps the rule that an entry is a file
    of its own. [`Unreadable`] when git cannot read the change.

    The change is read from its two ends and from nothing between them, so a pull
    request reads the same commit by commit, as its merge with `master`, and as the
    squash that lands. Its end is the commit `head`, or the working tree when `head`
    is None. Its beginning is the commit where that end left `base`'s history, their
    merge base, which is `base` itself for a pull request that has merged it and for
    a push: read from `base` as it stands, a branch behind it would be charged, or
    credited, with what `base` gained since.

    CHANGELOG.md may gain an entry line in two ways only. A fold moves it there out
    of the entry files: the files held that very line where the change began, and at
    its end they do not ([`held`]). Or a correction replaces a line where it stands:
    in each block of lines the change rewrites, it may put in one entry line for each
    entry line it takes out. Every other added entry line is one a change appended,
    which is what two pull requests conflict in (ADR 0075, ADR 0076). An entry line
    taken out in one place excuses none added in another."""
    tip = commit_of(root, head or "HEAD")
    fork = git(root, "merge-base", commit_of(root, base), tip)
    if not fork or not fork.strip():
        raise Unreadable(f"{base} and {head or 'HEAD'} share no commit in this checkout")
    fork = fork.strip()
    head = None if head is None else tip
    before = (at(root, fork, CHANGELOG) or "").split("\n")
    after = (at(root, head, CHANGELOG) or "").split("\n")
    moved = held(root, fork) - held(root, head)
    count = 0
    blocks = difflib.SequenceMatcher(None, before, after, autojunk=False).get_opcodes()
    for kind, old_from, old_to, new_from, new_to in blocks:
        if kind not in ("replace", "insert"):
            continue
        added = collections.Counter(line for line in after[new_from:new_to] if is_entry_line(line))
        a_folds = added & moved
        moved -= a_folds
        replaced = sum(1 for line in before[old_from:old_to] if is_entry_line(line))
        count += max(0, sum((added - a_folds).values()) - replaced)
    return count


def main(argv, root=ROOT, out=print):
    if argv == ["check"]:
        return check(root, out)
    if argv == ["fold"]:
        return fold(root, out)
    out("usage: changelog.py check | fold")
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
