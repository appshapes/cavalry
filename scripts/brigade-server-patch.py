#!/usr/bin/env python3
"""brigade-server-patch: point a checkout of Brigade's server at this tree of Cavalry
(ADR 0138, condition 9 and feature 2; ADR 0142; work item 362).

Brigade is the tool the team's people and their Claude Code sessions message each other
with, in the repository `appshapes/brigade`. Its server, the Rust workspace under
`server/` there, embeds Cavalry's driver: it takes the crates `cavalry` and
`cavalry-log` from this repository by git, pinned to one commit, and two teams' servers
run that build. A change on `master` here that breaks what the server calls would
otherwise be seen on the day Brigade moves its pin. The nightly job
`.github/workflows/brigade-server.yml` builds the server against `master` instead and
runs Brigade's own tests of it. This script is the step of that job that makes the
server take this tree, and a person runs the same script on their own machine
(docs/how-to/read-the-nightly-run-of-brigades-server.md).

Given the path of a checkout of Brigade, it:

1. prints the commit of each of the two checkouts, before anything can fail;
2. reads which crates of this tree the server's own packages name, and from which git
   URL (`cargo metadata --no-deps` on both workspaces, which resolves nothing), and
   prints what the server pins;
3. writes `server/.cargo/config.toml` in the checkout: a `[patch]` table that replaces
   each of them with this tree's copy, by path. Cargo reads that file for every command
   run under `server/`, so Brigade's own make targets need no flag;
4. resolves again with `cargo fetch`, which rewrites `server/Cargo.lock`: the make
   targets pass `--locked`, and cargo refuses a lock file that still names the pin;
5. checks, in the graph cargo then resolves with `--locked`, that every crate of this
   tree's is taken from this tree, and that nothing at all is taken from the git URL
   that was patched, so that a green run after it is a run against this tree;
6. prints the crates taken from this tree, and the crates the lock file gained and lost.

It changes the checkout: the lock file is rewritten and the config file is new. Give it
a clone to delete afterwards, never a checkout with work in it. A config file that this
script did not write is left as it is, and the run fails.

What it refuses, each with a message that says what to do next: a dependency on a crate
of this tree's that is not taken by git (from a registry or by a path: a git source is
the one this script patches so far); a crate that the server takes from the same git
URL and this tree does not have, renamed or removed here since the pin; a server that
names no crate of this tree's; anything still taken from that URL after the patch.

    python3 scripts/brigade-server-patch.py <checkout of appshapes/brigade>

Every cargo command runs under the checkout's `server/`, so the cargo that runs is the
one `server/rust-toolchain.toml` there names. The summary is on stdout, as the lines of
a Markdown list, so that the workflow can put it in the run's summary; cargo's own
output and every failure are on stderr.

Brigade's files are another repository's words, and what this script prints goes to a
workflow's log and to its run's summary. So a name, a version, a URL and a pin are
printed on stdout only when each is made of plain characters, and what a refusal quotes
from the checkout is quoted with its control characters escaped.

Python 3.9 standard library only (ADR 0003).
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path

TREE = Path(__file__).resolve().parent.parent
PAGE = "docs/how-to/read-the-nightly-run-of-brigades-server.md"
# The first line of the config file this script writes, by which a later run knows its own.
MARK = "# Written by Cavalry's scripts/brigade-server-patch.py. Not for a commit."
# A crate's name and its version, as crates.io allows them: no dot in a name, so that
# nothing in a lock file is printed that a Markdown page would turn into a link.
LOCKED = re.compile(r'^\[\[package\]\]\nname = "([A-Za-z0-9_-]+)"\nversion = "([0-9][0-9A-Za-z.+-]*)"$', re.M)
# A git URL, and a pin such as `rev=<commit>`.
PLAIN = re.compile(r"[A-Za-z0-9:/._@=+~-]+")


class Refused(Exception):
    """What the script cannot do, in words that say what to do next."""


def quoted(text: object) -> str:
    """A TOML basic string, and how a refusal quotes a word of the checkout's: JSON's
    escapes are among TOML's, and no control character is left as it is."""
    return json.dumps(str(text), ensure_ascii=False)


def cargo(server: Path, *args: str) -> str:
    """Run cargo under `server/`, where rustup reads the checkout's toolchain file. What
    it prints on stdout is returned; its stderr is ours."""
    try:
        done = subprocess.run(("cargo",) + args, cwd=server, stdout=subprocess.PIPE, text=True)
    except OSError as error:
        raise Refused(f"cargo did not start ({error}): install Rust with rustup, https://rustup.rs") from error
    if done.returncode != 0:
        raise Refused(f"`cargo {' '.join(args)}` failed under {server}; cargo's own message is above this line")
    return done.stdout


def members(metadata: dict) -> dict[str, Path]:
    """The packages of a workspace, each by its name with its manifest."""
    ids = set(metadata["workspace_members"])
    return {p["name"]: Path(p["manifest_path"]) for p in metadata["packages"] if p["id"] in ids}


def git_url(source: object) -> str | None:
    """The URL of a git source as cargo's metadata writes one, `git+<url>?<pin>#<commit>`,
    or nothing for any other source."""
    if not isinstance(source, str) or not source.startswith("git+"):
        return None
    return source[len("git+"):].partition("#")[0].partition("?")[0]


def same(url: str) -> str:
    """A git URL as cargo compares two: `.../cavalry`, `.../cavalry/` and `.../cavalry.git`
    are one repository. Lower case as well, which errs toward calling two URLs one."""
    url = url.rstrip("/")
    return (url[:-len(".git")] if url.endswith(".git") else url).lower()


def taken(server: dict, ours: dict[str, Path]) -> dict[str, tuple[str, str]]:
    """The crates of this tree's that the server's own packages name: each with the git
    URL it is taken from and what it is pinned to there (`rev=<commit>`, or nothing)."""
    found: dict[str, tuple[str, str]] = {}
    others: list[tuple[str, str, str]] = []
    ids = set(server["workspace_members"])
    for package in server["packages"]:
        if package["id"] not in ids:
            continue
        for dependency in package["dependencies"]:
            name, source = dependency["name"], dependency.get("source")
            url = git_url(source)
            if name not in ours:
                if url is not None:
                    others.append((package["name"], name, url))
                continue
            if url is None:
                where = quoted(source) if source else f"the path {quoted(dependency.get('path'))}"
                raise Refused(
                    f"{quoted(package['name'])} takes {name} from {where}, not by git. This script patches a "
                    "git source and nothing else: teach `taken` and `patch` in scripts/brigade-server-patch.py "
                    "that source, with a test, before the nightly job can build against this tree again")
            pin = source.partition("#")[0].partition("?")[2]
            if not PLAIN.fullmatch(url) or not PLAIN.fullmatch(pin or "none"):
                raise Refused(
                    f"{quoted(package['name'])} takes {name} from a source this script does not read, "
                    f"{quoted(source)}: a git URL and a pin are letters, digits and `:/._@=+~-` here. Read "
                    "that crate's Cargo.toml in the checkout")
            if found.setdefault(name, (url, pin)) != (url, pin):
                raise Refused(f"the server takes {name} from two places, {found[name]} and {(url, pin)}: "
                              "Brigade's manifests are to name one")
    if not found:
        raise Refused(
            "no package of the server names a crate of this tree's, so there is nothing to build against it. "
            "Read `server/crates/*/Cargo.toml` in the checkout for how the server takes Cavalry now, and "
            "change scripts/brigade-server-patch.py to follow it")
    urls = {same(url) for url, _ in found.values()}
    for owner, name, url in others:
        if same(url) in urls:
            raise Refused(
                f"{quoted(owner)} takes {quoted(name)} from Cavalry's repository, and this tree has no crate "
                "of that name: it was renamed or removed here after the commit the server pins. No patch can "
                f"stand in for it, so the server cannot be built against this tree. {PAGE}, step 6, says "
                "whether that is a change made on purpose or a regression")
    return found


def patch(found: dict[str, tuple[str, str]], ours: dict[str, Path]) -> str:
    """The config file's text: one `[patch]` table for each git URL, and in it each crate
    by its directory in this tree."""
    by_url: dict[str, list[str]] = {}
    for name in sorted(found):
        by_url.setdefault(found[name][0], []).append(name)
    lines = [MARK, "# It replaces the crates this server takes from Cavalry's repository with one tree's copies."]
    for url in sorted(by_url):
        lines += ["", f"[patch.{quoted(url)}]"]
        lines += [f"{name} = {{ path = {quoted(ours[name].parent)} }}" for name in by_url[url]]
    return "\n".join(lines) + "\n"


def wrong(graph: dict, found: dict[str, tuple[str, str]], ours: dict[str, Path]) -> list[str]:
    """What is wrong with the graph that cargo resolved after the patch. Nothing, when
    every crate of this tree's in it is taken from this tree and no crate at all is
    taken from a git URL that was patched."""
    urls = {same(url) for url, _ in found.values()}
    said, seen = [], set()
    for package in graph["packages"]:
        name, source, manifest = package["name"], package.get("source"), package["manifest_path"]
        url = git_url(source)
        if name in ours:
            seen.add(name)
            if source is not None or os.path.realpath(manifest) != os.path.realpath(ours[name]):
                said.append(f"{name} is taken from {quoted(source or manifest)}")
        elif url is not None and same(url) in urls:
            said.append(f"{quoted(name)} is taken from Cavalry's repository at {quoted(source)}, and this tree "
                        "has no crate of that name")
    said += [f"{name} is not in the graph" for name in sorted(set(found) - seen)]
    return said


def locked(text: str) -> set[tuple[str, str]]:
    """The crates a lock file holds, each as its name and its version. An entry whose name
    or version is made of anything else than a crate's characters is not read."""
    return set(LOCKED.findall(text))


def moved(before: set, after: set, ours: dict[str, Path]) -> tuple[list[str], list[str]]:
    """The crates the lock file gained and the ones it lost, this tree's own left out."""
    def named(crates: set) -> list[str]:
        return [f"{name} {version}" for name, version in sorted(crates) if name not in ours]
    return named(after - before), named(before - after)


def commit(path: Path, changes: bool) -> str:
    """The commit a checkout is at, and for this tree whether it holds changes that no
    commit does. Git looks no higher than `path`: a directory that is no checkout itself
    is not read as the repository above it."""
    def git(*args: str) -> str | None:
        try:
            done = subprocess.run(("git", "-C", str(path)) + args, capture_output=True, text=True,
                                  env={**os.environ, "GIT_CEILING_DIRECTORIES": str(path.parent)})
        except OSError:
            return None
        return done.stdout.strip() if done.returncode == 0 else None

    sha = git("rev-parse", "--verify", "HEAD")
    if sha is None or not re.fullmatch(r"[0-9a-f]{40,64}", sha):
        return "no commit, since git does not read it as a checkout"
    return sha + (", with changes that no commit holds" if changes and git("status", "--porcelain") else "")


def say(line: str) -> None:
    """One line of the summary, at once: a run that fails later has still said it."""
    print(line, flush=True)


def point(checkout: Path) -> None:
    """Point the server of `checkout` at this tree, and say in a few lines what was done."""
    server = checkout / "server"
    if not (server / "Cargo.toml").is_file():
        raise Refused(f"{server} holds no Cargo.toml: give the path of a checkout of appshapes/brigade, "
                      "whose server is the workspace under server/")
    config, lock = server / ".cargo" / "config.toml", server / "Cargo.lock"
    for held in (config, config.with_suffix("")):
        if held.exists() and not held.read_text(encoding="utf-8", errors="replace").startswith(MARK):
            raise Refused(f"{held} is there and this script did not write it. It is left as it is. Use a new "
                          "clone of appshapes/brigade; if Brigade's server has a cargo config of its own now, "
                          "scripts/brigade-server-patch.py has to keep its lines")
    say(f"- Brigade's server: {commit(checkout, changes=False)}")
    say(f"- Cavalry: {commit(TREE, changes=True)}")

    ours = members(json.loads(cargo(server, "metadata", "--no-deps", "--format-version", "1",
                                    "--manifest-path", str(TREE / "Cargo.toml"))))
    found = taken(json.loads(cargo(server, "metadata", "--no-deps", "--format-version", "1")), ours)
    say("- In place of what the server pins: " + " and ".join(sorted({pin or "no pin" for _, pin in found.values()})))
    before = locked(lock.read_text(encoding="utf-8")) if lock.is_file() else set()

    config.parent.mkdir(exist_ok=True)
    config.write_text(patch(found, ours), encoding="utf-8")
    cargo(server, "fetch")
    graph = json.loads(cargo(server, "metadata", "--locked", "--format-version", "1"))
    said = wrong(graph, found, ours)
    if said:
        raise Refused("after the patch the server still does not take every crate of Cavalry's from this tree: "
                      + "; ".join(said) + ". A crate the server names that the patch leaves out is the usual "
                      f"cause: compare {config} with `server/crates/*/Cargo.toml`")

    say("- Taken from this tree: " + ", ".join(sorted(p["name"] for p in graph["packages"] if p["name"] in ours)))
    gained, lost = moved(before, locked(lock.read_text(encoding="utf-8")), ours)
    if gained:
        say(f"- Crates the lock file gained: {', '.join(gained)}")
    if lost:
        say(f"- Crates the lock file lost: {', '.join(lost)}")


def main(argv: list[str]) -> int:
    if len(argv) != 2 or argv[1].startswith("-"):
        print("usage: python3 scripts/brigade-server-patch.py <checkout of appshapes/brigade>\n"
              "It changes that checkout: give it a clone to delete afterwards.", file=sys.stderr)
        return 2
    try:
        point(Path(argv[1]).resolve())
    except Refused as error:
        print(f"brigade-server-patch: {error}", file=sys.stderr)
        return 1
    except (OSError, ValueError) as error:
        # A file that cannot be read or written, or an answer of cargo's that is not JSON.
        print(f"brigade-server-patch: {quoted(error)}. Nothing was checked: use a new clone of "
              "appshapes/brigade and run the script again", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
