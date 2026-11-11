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

A merged pull request's head is matched to what is here in two ways. By name: a local
branch, a worktree's branch or a branch on origin that is called what the head was
called. And by upstream: a local branch under another name whose upstream is that head
on origin, as a branch has when it was pushed with `git push -u origin <local>:<head>`.

A branch started from that head, `git worktree add -b <new> <path> origin/<head>`, has
the same upstream and is somebody's work in progress. So a branch matched by its
upstream alone is retired only when both of these hold, and is otherwise left, with
the reason:

- it was made before its pull request was opened, by the oldest entry of its reflog,
  so it is the branch the pull request came from and not one started from it;
- its tip is the merged head's commit or one before it, so it holds no work the pull
  request did not. The head's commit is fetched from the pull request's own ref when a
  fixer's push put it past what this checkout has.

A branch on origin is deleted only while it still stands at the commit its pull request
merged at: one that was pushed to again since is somebody's and is left. A local
branch whose upstream is gone from origin and that no merged pull request accounts
for is left too, and named, with what retires it.

Needs `git` and, for the no-argument form, `gh` (the merged pull requests come from it).
Python 3.9 standard library only (ADR 0003).
"""

from __future__ import annotations

import datetime
import json
import subprocess
import sys
from pathlib import Path

PROTECTED = {"master", "HEAD"}


def run(*args: str, check: bool = True) -> str:
    try:
        result = subprocess.run(args, capture_output=True, text=True)
    except FileNotFoundError:
        if check:
            sys.exit(f"tidy: {args[0]} is not installed")
        return ""
    if check and result.returncode != 0:
        sys.exit(f"tidy: {' '.join(args)} failed: {result.stderr.strip() or result.stdout.strip()}")
    return result.stdout


def ok(*args: str) -> bool:
    """Whether a git command succeeds; what it prints is not wanted."""
    return subprocess.run(args, capture_output=True, text=True).returncode == 0


def local_branches() -> set[str]:
    """Every local branch, by its name under refs/heads/: the short form git prints
    gains a `heads/` when a tag has the same name, and is then no branch's name."""
    out = run("git", "for-each-ref", "--format=%(refname:lstrip=2)", "refs/heads")
    return {line.strip() for line in out.splitlines() if line.strip()}


def origin_branches() -> dict[str, str]:
    """Branch name to the commit it stands at, for every branch on origin."""
    out = run("git", "ls-remote", "--heads", "origin")
    return {line.split("refs/heads/", 1)[1]: line.split()[0] for line in out.splitlines() if "refs/heads/" in line}


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


def merged_pull_requests() -> dict[str, dict] | None:
    """Head branch name to its merged pull request (`number`, `headRefOid`,
    `createdAt`), for this repository's own branches, the newest pull request winning
    a name used twice. None when `gh` cannot say."""
    try:
        out = run("gh", "pr", "list", "--state", "merged", "--limit", "200", "--json",
                  "number,headRefName,headRefOid,createdAt,isCrossRepository")
    except SystemExit:
        print("tidy: gh is not available or not signed in; retiring nothing from merged pull requests")
        return None
    heads: dict[str, dict] = {}
    for p in json.loads(out):                      # gh lists the newest first
        if not p.get("isCrossRepository"):
            heads.setdefault(p["headRefName"], p)
    return heads


def upstreams() -> dict[str, tuple[str, bool]]:
    """Local branch to (the branch on origin it tracks, whether that is gone), for
    every local branch with an upstream on origin."""
    found: dict[str, tuple[str, bool]] = {}
    out = run("git", "for-each-ref", "--format=%(refname:lstrip=2)%09%(upstream:lstrip=2)%09%(upstream:track)",
              "refs/heads")
    for line in out.splitlines():
        branch, _, rest = line.partition("\t")
        upstream, _, track = rest.partition("\t")
        if upstream.startswith("origin/"):
            found[branch] = (upstream[len("origin/"):], "gone" in track)
    return found


def made_at(branch: str) -> int | None:
    """When the local branch was made, in seconds since the Unix epoch: the oldest
    entry of its reflog. None when the reflog has none."""
    out = run("git", "reflog", "show", "--date=unix", "--format=%gd", f"refs/heads/{branch}", check=False)
    oldest = out.strip().splitlines()[-1:] or [""]
    stamp = oldest[0].rpartition("@{")[2].rstrip("}")
    return int(stamp) if stamp.isdigit() else None


def opened_at(pull_request: dict) -> int | None:
    """When the pull request was opened, in seconds since the Unix epoch."""
    try:
        return int(datetime.datetime.fromisoformat(str(pull_request.get("createdAt", "")).replace("Z", "+00:00"))
                   .timestamp())
    except ValueError:
        return None


def holds_no_more(branch: str, pull_request: dict) -> bool:
    """Whether the local branch's tip is the merged head's commit or an ancestor of
    it: nothing was added to the branch that the pull request did not have. The
    head's commit is fetched from the pull request's ref when it is not here, as
    after a fixer's push. False when it still is not here, since then it cannot be
    told."""
    head = pull_request.get("headRefOid") or ""
    if not head:
        return False
    if not ok("git", "cat-file", "-e", f"{head}^{{commit}}"):
        ok("git", "fetch", "--quiet", "origin", f"refs/pull/{pull_request.get('number')}/head")
    if not ok("git", "cat-file", "-e", f"{head}^{{commit}}"):
        return False
    return ok("git", "merge-base", "--is-ancestor", f"refs/heads/{branch}", head)


def open_pull_request(branch: str) -> bool:
    out = run("gh", "pr", "list", "--state", "open", "--head", branch, "--json", "number", check=False)
    return bool(out.strip()) and out.strip() != "[]"


def retire(branch: str, main_worktree: Path, current: str, origin: str | None = None,
           merged_at: str | None = None) -> list[str]:
    """Retire one branch everywhere it exists; returns what was done. `origin` is the
    branch's name on origin, the local name when None, or "" to leave origin alone.
    `merged_at` is the commit its pull request merged at: the branch on origin is
    deleted only while it stands there."""
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
    remote = branch if origin is None else origin
    on_origin = origin_branches() if remote and remote not in PROTECTED else {}
    if remote in on_origin:
        if merged_at and on_origin[remote] != merged_at:
            done.append(f"left origin/{remote}: it stands at {on_origin[remote][:7]}, not at {merged_at[:7]} where "
                        f"its pull request merged, so it was pushed to since")
        else:
            run("git", "push", "--quiet", "origin", "--delete", remote)
            done.append(f"deleted origin/{remote}")
    return done or [f"{branch}: nothing to retire"]


def by_upstream(branch: str, upstream: str, pull_request: dict, main_worktree: Path, current: str) -> list[str]:
    """What to print for a local branch under another name that tracks a merged
    head: retired, or left with the reason."""
    head = pull_request.get("headRefOid") or ""
    lead = f"{branch}: it tracks origin/{upstream}, whose pull request {pull_request.get('number')} merged"
    if branch == current:
        return [f"{lead}; it is the current branch, left alone: switch to master and run `just tidy` again"]
    made, opened = made_at(branch), opened_at(pull_request)
    if made is None or opened is None or made > opened:
        return [f"{lead}, and it was made after that pull request was opened, or when cannot be told, so it may "
                f"be a branch started from that head and not the one pushed to it: left; `just tidy {branch}` "
                f"retires it"]
    if not holds_no_more(branch, pull_request):
        return [f"{lead}, and its tip is not that pull request's head {head[:7] or 'commit'} or a commit before "
                f"it, or that commit could not be fetched, so it may hold work the pull request did not: left; "
                f"`just tidy {branch}` retires it"]
    return [lead] + retire(branch, main_worktree, current, origin=upstream, merged_at=head)


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
        # The branch's pull request is under the name it was pushed to.
        upstream = upstreams().get(branch, (branch, False))[0]
        for name in sorted({branch, upstream}):
            if open_pull_request(name):
                sys.exit(f"tidy: {name} has an open pull request; merge or close it first")
        if branch not in local_branches() and branch not in origin_branches():
            sys.exit(f"tidy: no branch {branch} here or on origin")
        # A branch on origin under another name may be more than this branch's: left.
        for line in retire(branch, main_worktree, current, origin=None if upstream == branch else ""):
            print(f"tidy: {line}")
        if upstream != branch and upstream not in PROTECTED and upstream in origin_branches():
            print(f"tidy: left origin/{upstream}, which {branch} tracked under another name: "
                  f"`git push origin --delete {upstream}` removes it if it was this branch's alone")
        run("git", "worktree", "prune")
        return 0
    merged = merged_pull_requests()
    printed = 0
    if merged is not None:
        existing = local_branches() | set(origin_branches()) | set(worktrees())
        # By name: what is called as a merged head was called.
        for head, pull_request in merged.items():
            if head in existing and head not in PROTECTED:
                for line in retire(head, main_worktree, current, merged_at=pull_request.get("headRefOid") or None):
                    print(f"tidy: {line}")
                    printed += 1
        # By upstream: a local branch under another name that tracks a merged head; and
        # any local branch whose upstream is gone with no merged pull request for it.
        here = local_branches()
        for branch, (upstream, gone) in sorted(upstreams().items()):
            if upstream in PROTECTED or branch in PROTECTED or branch not in here:
                continue
            if upstream in merged and branch != upstream:
                for line in by_upstream(branch, upstream, merged[upstream], main_worktree, current):
                    print(f"tidy: {line}")
                    printed += 1
            elif gone and upstream not in merged:
                how = (f"`just tidy {branch}` retires it" if branch != current
                       else f"it is the current branch: switch to master, then `just tidy {branch}` retires it")
                print(f"tidy: {branch}: it tracks origin/{upstream}, which is gone from origin, and none of the "
                      f"last 200 merged pull requests had that head: left; {how}")
                printed += 1
    if not printed:
        print("tidy: nothing to retire")
    run("git", "worktree", "prune")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
