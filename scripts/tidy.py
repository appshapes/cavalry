#!/usr/bin/env python3
"""tidy: retire branches whose work is on master, locally and on origin.

    tidy.py            retire every branch whose pull request merged
    tidy.py <branch>   retire that working branch, which never needed a pull request

GitHub deletes a pull request's branch on origin when it merges ("automatically delete
head branches" is on for this repository). What it cannot reach, this does: the local
branch, which `git branch -d` refuses after a squash merge because no commit says it
merged; the worktree that pins the branch; the stale remote-tracking ref; and a working
branch that never became a pull request. For each branch to retire, in this order: the
worktree that has it checked out is removed when it is clean and left, and named, when it
is not; the local branch is deleted; the branch is deleted on origin when it is still
there. The current branch and `master` are never touched. A named branch with an open
pull request is refused. Nothing here rebases, force-pushes or aborts a merge.

Needs `git` and, for the no-argument form, `gh` (the merged pull requests come from it).
Python 3.9 standard library only (ADR 0003).
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

PROTECTED = {"master", "HEAD"}


def run(*args: str, check: bool = True) -> str:
    result = subprocess.run(args, capture_output=True, text=True)
    if check and result.returncode != 0:
        sys.exit(f"tidy: {' '.join(args)} failed: {result.stderr.strip() or result.stdout.strip()}")
    return result.stdout


def local_branches() -> set[str]:
    return {line.strip().lstrip("+* ").strip() for line in run("git", "branch", "--format=%(refname:short)").splitlines()}


def origin_branches() -> set[str]:
    out = run("git", "ls-remote", "--heads", "origin")
    return {line.split("refs/heads/", 1)[1] for line in out.splitlines() if "refs/heads/" in line}


def worktrees() -> dict[str, Path]:
    """Branch name to worktree path, for every worktree on a branch."""
    found: dict[str, Path] = {}
    path: Path | None = None
    for line in run("git", "worktree", "list", "--porcelain").splitlines():
        if line.startswith("worktree "):
            path = Path(line[len("worktree "):])
        elif line.startswith("branch refs/heads/") and path is not None:
            found[line[len("branch refs/heads/"):]] = path
    return found


def worktree_is_clean(path: Path) -> bool:
    dirty = run("git", "-C", str(path), "status", "--porcelain").strip()
    git_dir = Path(run("git", "-C", str(path), "rev-parse", "--git-dir").strip())
    if not git_dir.is_absolute():
        git_dir = path / git_dir
    return not dirty and not (git_dir / "MERGE_HEAD").exists()


def merged_pull_request_heads() -> list[str]:
    try:
        out = run("gh", "pr", "list", "--state", "merged", "--limit", "200", "--json", "headRefName,isCrossRepository")
    except SystemExit:
        print("tidy: gh is not available or not signed in; retiring nothing from merged pull requests")
        return []
    return [p["headRefName"] for p in json.loads(out) if not p.get("isCrossRepository")]


def open_pull_request(branch: str) -> bool:
    out = run("gh", "pr", "list", "--state", "open", "--head", branch, "--json", "number", check=False)
    return bool(out.strip()) and out.strip() != "[]"


def retire(branch: str, main_worktree: Path, current: str) -> list[str]:
    """Retire one branch everywhere it exists; returns what was done."""
    done: list[str] = []
    if branch in PROTECTED or branch == current:
        return [f"{branch}: the current branch or master, left alone"]
    trees = worktrees()
    if branch in trees:
        path = trees[branch]
        if path.resolve() == main_worktree.resolve():
            return [f"{branch}: checked out in the main worktree, left alone"]
        if not worktree_is_clean(path):
            return [f"{branch}: its worktree {path} has uncommitted changes or a merge in progress, left for its owner"]
        run("git", "worktree", "remove", str(path))
        done.append(f"removed worktree {path}")
    if branch in local_branches():
        run("git", "branch", "-D", branch)
        done.append(f"deleted local branch {branch}")
    if branch in origin_branches():
        run("git", "push", "--quiet", "origin", "--delete", branch)
        done.append(f"deleted origin/{branch}")
    return done or [f"{branch}: nothing to retire"]


def main(argv: list[str]) -> int:
    if len(argv) > 2 or (len(argv) == 2 and argv[1].startswith("-")):
        sys.exit(__doc__)
    main_worktree = Path(run("git", "rev-parse", "--show-toplevel").strip())
    run("git", "fetch", "--prune", "--quiet", "origin")
    current = run("git", "rev-parse", "--abbrev-ref", "HEAD").strip()
    if len(argv) == 2:
        branch = argv[1]
        if branch in PROTECTED:
            sys.exit(f"tidy: {branch} is never retired")
        if open_pull_request(branch):
            sys.exit(f"tidy: {branch} has an open pull request; merge or close it first")
        if branch not in local_branches() and branch not in origin_branches():
            sys.exit(f"tidy: no branch {branch} here or on origin")
        targets = [branch]
    else:
        existing = local_branches() | origin_branches() | set(worktrees())
        targets = [b for b in merged_pull_request_heads() if b in existing]
    if not targets:
        print("tidy: nothing to retire")
        run("git", "worktree", "prune")
        return 0
    for branch in targets:
        for line in retire(branch, main_worktree, current):
            print(f"tidy: {line}")
    run("git", "worktree", "prune")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
