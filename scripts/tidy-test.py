#!/usr/bin/env python3
"""Tests of scripts/tidy.py (card 262), against a made-up origin, a clone of it with
worktrees, and a fake `gh` on PATH that lists the merged pull requests it is given: no
network and no token. Every git call, the script's own included, runs with an identity
and a configuration of the test's own, so that it reads the same on a laptop and on a
runner with none.

What is held: a merged head is matched by name, as before, and by the upstream a
local branch under another name tracks; a branch matched by its upstream alone is
retired only when it was made before its pull request was opened and holds no more
than the pull request did, the head's commit fetched from the pull request's ref when
a fixer put it past this checkout; a branch on origin goes only while it stands where
its pull request merged; what is left is named with the reason and with what retires
it; `master` and a branch that tracks it are never touched; with no `gh` nothing
is retired and nothing is said of pull requests that were not read; and a named branch
is refused, and left on origin, when `gh` cannot say whether it has an open pull
request: signed out, not installed, or printing what is no list.

Standard library only; `just test-scripts` runs it.
"""
import datetime
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

TIDY = Path(__file__).resolve().parent / "tidy.py"

# The fake `gh`: `gh pr list --state merged ... --json ...` prints the file FAKE_GH_MERGED
# names; `gh pr list --state open --head <branch> ...` prints a pull request for the head
# FAKE_GH_OPEN names and none for any other, or what FAKE_GH_OPEN_PRINTS holds; and
# either fails as a `gh` that is signed out does when FAKE_GH_SIGNED_OUT is set.
FAKE_GH = r'''#!/usr/bin/env python3
import os, sys
argv = sys.argv[1:]
if argv[:2] != ["pr", "list"] or "--state" not in argv:
    sys.exit("fake gh: only `gh pr list --state ...` is served, got " + " ".join(argv))
state = argv[argv.index("--state") + 1]
if os.environ.get("FAKE_GH_SIGNED_OUT"):
    sys.exit("gh: To get started with GitHub CLI, please run: gh auth login")
if state == "merged":
    sys.stdout.write(open(os.environ["FAKE_GH_MERGED"], encoding="utf-8").read())
elif state == "open":
    head = argv[argv.index("--head") + 1]
    if "FAKE_GH_OPEN_PRINTS" in os.environ:
        sys.stdout.write(os.environ["FAKE_GH_OPEN_PRINTS"])
    else:
        sys.stdout.write('[{"number": 77}]' if head == os.environ.get("FAKE_GH_OPEN") else "[]")
else:
    sys.exit("fake gh: an unexpected state " + state)
'''


class Checkout(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        tmp = Path(self.tmp.name).resolve()
        self.origin, self.work, self.trees = tmp / "origin.git", tmp / "work", tmp / "trees"
        gh = tmp / "bin" / "gh"
        gh.parent.mkdir()
        gh.write_text(FAKE_GH, encoding="utf-8")
        gh.chmod(gh.stat().st_mode | stat.S_IXUSR)
        self.merged_file = tmp / "merged.json"
        self.merged = []
        self.env = {**os.environ, "PATH": f"{gh.parent}{os.pathsep}{os.environ['PATH']}",
                    "FAKE_GH_MERGED": str(self.merged_file),
                    "GIT_AUTHOR_NAME": "A Test", "GIT_AUTHOR_EMAIL": "test@example.invalid",
                    "GIT_COMMITTER_NAME": "A Test", "GIT_COMMITTER_EMAIL": "test@example.invalid",
                    "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_SYSTEM": os.devnull}
        self.git("init", "--quiet", "--bare", "--initial-branch=master", str(self.origin), cwd=tmp)
        self.git("clone", "--quiet", str(self.origin), str(self.work), cwd=tmp)
        (self.work / "README.md").write_text("a checkout\n", encoding="utf-8")
        self.git("add", "--all")
        self.git("commit", "--quiet", "-m", "the first commit")
        self.git("push", "--quiet", "-u", "origin", "master")

    def git(self, *args, cwd=None):
        done = subprocess.run(["git", *args], cwd=cwd or self.work, env=self.env, capture_output=True, text=True)
        self.assertEqual(done.returncode, 0, f"git {' '.join(args)}: {done.stderr}")
        return done.stdout

    def worktree(self, local, head, push=True):
        """A worktree on a new branch `local` with one commit, pushed to origin as
        `head` and tracking it; returns the worktree's path and the commit."""
        path = self.trees / local
        self.git("worktree", "add", "--quiet", "-b", local, str(path), "master")
        (path / f"{local}.txt").write_text("work\n", encoding="utf-8")
        self.git("add", "--all", cwd=path)
        self.git("commit", "--quiet", "-m", f"work on {local}", cwd=path)
        if push:
            self.git("push", "--quiet", "-u", "origin", f"{local}:{head}", cwd=path)
        return path, self.git("rev-parse", "HEAD", cwd=path).strip()

    def merge(self, head, commit, delete=True, opened=3600):
        """The pull request from `head` at `commit` merged, and GitHub deleted the
        branch on origin, unless `delete` is false. It was opened `opened` seconds
        from now: after the branches of this test were made, unless negative. GitHub
        keeps the pull request's own ref, `refs/pull/<number>/head`, when the commit
        is on origin."""
        number = 100 + len(self.merged)
        when = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=opened)
        self.merged.insert(0, {"number": number, "headRefName": head, "headRefOid": commit,
                               "createdAt": when.strftime("%Y-%m-%dT%H:%M:%SZ"), "isCrossRepository": False})
        if subprocess.run(["git", "cat-file", "-e", commit], cwd=self.origin, env=self.env,
                          capture_output=True).returncode == 0:
            self.git("update-ref", f"refs/pull/{number}/head", commit, cwd=self.origin)
        if delete:
            self.git("push", "--quiet", "origin", "--delete", head)
        return number

    def tidy(self, *args, cwd=None, **env):
        self.merged_file.write_text(json.dumps(self.merged), encoding="utf-8")
        done = subprocess.run([sys.executable, str(TIDY), *args], cwd=cwd or self.work, env={**self.env, **env},
                              capture_output=True, text=True)
        return done.returncode, done.stdout + done.stderr

    def branches(self):
        return set(self.git("for-each-ref", "--format=%(refname:lstrip=2)", "refs/heads").split())

    def on_origin(self):
        return {line.split("refs/heads/", 1)[1] for line in self.git("ls-remote", "--heads", "origin").splitlines()}


class Tidy(Checkout):
    def test_a_merged_head_is_retired_by_its_name_with_its_worktree_and_on_origin(self):
        path, commit = self.worktree("5-by-name", "5-by-name")
        self.merge("5-by-name", commit, delete=False)
        status, out = self.tidy()
        self.assertEqual(status, 0, out)
        self.assertFalse(path.exists(), out)
        self.assertNotIn("5-by-name", self.branches())
        self.assertNotIn("5-by-name", self.on_origin())
        for line in (f"removed worktree {path}", "deleted local branch 5-by-name", "deleted origin/5-by-name"):
            self.assertIn(f"tidy: {line}", out)

    def test_a_branch_under_another_name_is_retired_by_the_merged_head_it_tracks(self):
        # Card 262: `177-recorded-purging` was pushed as another head, and stayed.
        path, commit = self.worktree("177-recorded-purging", "177-bench-the-recorded-rows")
        self.merge("177-bench-the-recorded-rows", commit)
        status, out = self.tidy()
        self.assertEqual(status, 0, out)
        self.assertFalse(path.exists(), out)
        self.assertNotIn("177-recorded-purging", self.branches())
        self.assertIn("tidy: 177-recorded-purging: it tracks origin/177-bench-the-recorded-rows, whose pull request "
                      "100 merged", out)
        self.assertIn(f"tidy: removed worktree {path}", out)
        self.assertIn("tidy: deleted local branch 177-recorded-purging", out)

    def test_a_head_still_on_origin_goes_with_the_branch_that_tracked_it(self):
        path, commit = self.worktree("8-local", "8-head")
        self.merge("8-head", commit, delete=False)
        status, out = self.tidy()
        self.assertEqual(status, 0, out)
        self.assertNotIn("8-local", self.branches())
        self.assertNotIn("8-head", self.on_origin())
        self.assertIn("tidy: deleted origin/8-head", out)

    def test_a_fixer_commit_this_checkout_never_had_does_not_keep_the_branch(self):
        # The fixer pushed to the head from a checkout of its own, and GitHub deleted
        # the branch at the merge: the head's commit is on origin under the pull
        # request's ref alone, and is fetched from there.
        path, commit = self.worktree("9-local", "9-head")
        fixer = Path(self.tmp.name).resolve() / "fixer"
        self.git("clone", "--quiet", "--branch", "9-head", str(self.origin), str(fixer), cwd=fixer.parent)
        (fixer / "fix.txt").write_text("a fix\n", encoding="utf-8")
        self.git("add", "--all", cwd=fixer)
        self.git("commit", "--quiet", "-m", "the fixer's commit", cwd=fixer)
        self.git("push", "--quiet", "origin", "9-head", cwd=fixer)
        head = self.git("rev-parse", "HEAD", cwd=fixer).strip()
        self.merge("9-head", head)
        self.assertNotEqual(subprocess.run(["git", "cat-file", "-e", head], cwd=self.work, env=self.env,
                                           capture_output=True).returncode, 0, "the checkout has no such commit")
        status, out = self.tidy()
        self.assertEqual(status, 0, out)
        self.assertNotIn("9-local", self.branches(), out)
        self.assertFalse(path.exists())

    def test_a_branch_started_from_a_merged_head_is_somebodys_and_is_left(self):
        # `git worktree add -b <new> <path> origin/<head>` gives the new branch that
        # upstream too. It was made after the pull request was opened: not its source.
        _, commit = self.worktree("10-source", "10-head")
        started = self.trees / "10-started-from-it"
        self.git("worktree", "add", "--quiet", "-b", "10-started-from-it", str(started), "origin/10-head")
        self.assertEqual(self.git("rev-parse", "--abbrev-ref", "10-started-from-it@{upstream}").strip(),
                         "origin/10-head")
        self.merge("10-head", commit, opened=-3600)
        status, out = self.tidy()
        self.assertEqual(status, 0, out)
        self.assertTrue(started.exists(), out)
        self.assertIn("10-started-from-it", self.branches())
        self.assertIn("tidy: 10-started-from-it: it tracks origin/10-head, whose pull request 100 merged, and it was "
                      "made after that pull request was opened", out)
        self.assertIn("left; `just tidy 10-started-from-it` retires it", out)

    def test_a_head_pushed_to_again_since_its_merge_stays_on_origin(self):
        # By name and by upstream alike: origin's branch is deleted only where it merged.
        path, commit = self.worktree("11-local", "11-head")
        self.merge("11-head", commit, delete=False)
        other = Path(self.tmp.name).resolve() / "other"
        self.git("clone", "--quiet", "--branch", "11-head", str(self.origin), str(other), cwd=other.parent)
        (other / "more.txt").write_text("a follow-up\n", encoding="utf-8")
        self.git("add", "--all", cwd=other)
        self.git("commit", "--quiet", "-m", "pushed after the merge", cwd=other)
        self.git("push", "--quiet", "origin", "11-head", cwd=other)
        status, out = self.tidy()
        self.assertEqual(status, 0, out)
        self.assertNotIn("11-local", self.branches(), out)
        self.assertIn("11-head", self.on_origin())
        self.assertIn("tidy: left origin/11-head: it stands at", out)
        self.assertIn("so it was pushed to since", out)

    def test_a_branch_with_work_its_pull_request_did_not_have_is_left_and_named(self):
        path, commit = self.worktree("6-local", "6-head")
        self.merge("6-head", commit)
        (path / "later.txt").write_text("work after the merge\n", encoding="utf-8")
        self.git("add", "--all", cwd=path)
        self.git("commit", "--quiet", "-m", "work the pull request did not have", cwd=path)
        # A tag of the branch's name at the merged commit: the branch is what is asked
        # about, not whatever its short name resolves to.
        self.git("tag", "6-local", commit)
        status, out = self.tidy()
        self.assertEqual(status, 0, out)
        self.assertTrue(path.exists())
        self.assertIn("6-local", self.branches())
        self.assertIn("tidy: 6-local: it tracks origin/6-head, whose pull request 100 merged, and its tip is not", out)
        self.assertIn("so it may hold work the pull request did not: left; `just tidy 6-local` retires it", out)

    def test_a_merged_head_whose_commit_is_not_here_leaves_the_branch(self):
        path, commit = self.worktree("7-local", "7-head")
        self.merge("7-head", "0" * 40)
        status, out = self.tidy()
        self.assertEqual(status, 0, out)
        self.assertIn("7-local", self.branches())
        self.assertIn("or that commit could not be fetched", out)

    def test_a_branch_whose_upstream_is_gone_and_no_pull_request_merged_is_left_and_named(self):
        path, _ = self.worktree("4-local", "4-closed-unmerged")
        self.git("push", "--quiet", "origin", "--delete", "4-closed-unmerged")
        status, out = self.tidy()
        self.assertEqual(status, 0, out)
        self.assertTrue(path.exists())
        self.assertIn("tidy: 4-local: it tracks origin/4-closed-unmerged, which is gone from origin, and none of "
                      "the last 200 merged pull requests had that head: left; `just tidy 4-local` retires it", out)
        self.assertNotIn("nothing to retire", out)

    def test_a_branch_of_its_own_name_whose_upstream_is_gone_is_named_too(self):
        path, _ = self.worktree("12-closed-unmerged", "12-closed-unmerged")
        self.git("push", "--quiet", "origin", "--delete", "12-closed-unmerged")
        status, out = self.tidy()
        self.assertEqual(status, 0, out)
        self.assertTrue(path.exists())
        self.assertIn("tidy: 12-closed-unmerged: it tracks origin/12-closed-unmerged, which is gone from origin", out)

    def test_with_no_gh_nothing_is_retired_and_nothing_is_said_of_pull_requests(self):
        path, commit = self.worktree("13-local", "13-head")
        self.merge("13-head", commit)
        status, out = self.tidy(FAKE_GH_SIGNED_OUT="1")
        self.assertEqual(status, 0, out)
        self.assertTrue(path.exists())
        self.assertIn("tidy: gh is not available or not signed in; retiring nothing from merged pull requests", out)
        self.assertNotIn("merged pull requests had that head", out)
        self.assertIn("tidy: nothing to retire", out)

    def test_the_current_branch_is_left_and_told_to_switch(self):
        path, commit = self.worktree("14-local", "14-head")
        self.merge("14-head", commit)
        status, out = self.tidy(cwd=path)
        self.assertEqual(status, 0, out)
        self.assertTrue(path.exists())
        self.assertIn("14-local", self.branches())
        self.assertIn("it is the current branch, left alone: switch to master and run `just tidy` again", out)

    def test_a_named_branch_is_refused_while_the_head_it_tracks_has_an_open_pull_request(self):
        path, _ = self.worktree("15-local", "15-head")
        status, out = self.tidy("15-local", FAKE_GH_OPEN="15-head")
        self.assertNotEqual(status, 0)
        self.assertIn("tidy: 15-head has an open pull request; merge or close it first", out)
        self.assertTrue(path.exists())
        self.assertIn("15-local", self.branches())

    def left_as_it_was(self, branch, path, out):
        """A named branch that tidy refused: here, on origin and in its worktree."""
        self.assertTrue(path.exists(), out)
        self.assertIn(branch, self.branches(), out)
        self.assertIn(branch, self.on_origin(), out)

    def test_a_named_branch_is_refused_when_gh_is_signed_out_and_stays_on_origin(self):
        path, _ = self.worktree("17-own-name", "17-own-name")
        status, out = self.tidy("17-own-name", FAKE_GH_SIGNED_OUT="1")
        self.assertNotEqual(status, 0, out)
        self.assertIn("tidy: gh could not say whether 17-own-name has an open pull request, so 17-own-name is "
                      "left as it is: install gh, or sign in with `gh auth login`, then run "
                      "`just tidy 17-own-name` again", out)
        self.left_as_it_was("17-own-name", path, out)

    def test_a_named_branch_is_refused_when_gh_is_not_installed(self):
        path, _ = self.worktree("18-own-name", "18-own-name")
        # A PATH with git and nothing else: no `gh` of the test's, none of the host's.
        only_git = Path(self.tmp.name).resolve() / "only-git"
        only_git.mkdir()
        (only_git / "git").symlink_to(shutil.which("git", path=self.env["PATH"]))
        status, out = self.tidy("18-own-name", PATH=str(only_git))
        self.assertNotEqual(status, 0, out)
        self.assertIn("tidy: gh could not say whether 18-own-name has an open pull request", out)
        self.assertNotIn("Traceback", out)
        self.left_as_it_was("18-own-name", path, out)

    def test_a_named_branch_is_refused_when_gh_prints_what_is_no_list(self):
        path, _ = self.worktree("19-own-name", "19-own-name")
        for printed in ("", "not json", '{"message": "rate limited"}'):
            status, out = self.tidy("19-own-name", FAKE_GH_OPEN_PRINTS=printed)
            self.assertNotEqual(status, 0, (printed, out))
            self.assertIn("tidy: gh could not say whether 19-own-name has an open pull request", out)
            self.left_as_it_was("19-own-name", path, out)

    def test_a_named_branch_that_tracks_master_is_not_asked_about_master(self):
        path = self.trees / "20-from-master"
        self.git("worktree", "add", "--quiet", "--track", "-b", "20-from-master", str(path), "origin/master")
        # Some other pull request's head is called master: not this branch's to wait for.
        status, out = self.tidy("20-from-master", FAKE_GH_OPEN="master")
        self.assertEqual(status, 0, out)
        self.assertFalse(path.exists())
        self.assertNotIn("20-from-master", self.branches())
        self.assertIn("master", self.on_origin())
        self.assertNotIn("open pull request", out)

    def test_a_named_branch_leaves_a_head_of_another_name_on_origin_and_says_so(self):
        path, _ = self.worktree("16-local", "16-head")
        status, out = self.tidy("16-local")
        self.assertEqual(status, 0, out)
        self.assertFalse(path.exists())
        self.assertNotIn("16-local", self.branches())
        self.assertIn("16-head", self.on_origin())
        self.assertIn("tidy: left origin/16-head, which 16-local tracked under another name", out)

    def test_a_worktree_with_uncommitted_changes_is_left_for_its_owner(self):
        path, commit = self.worktree("3-local", "3-head")
        self.merge("3-head", commit)
        (path / "unsaved.txt").write_text("not committed\n", encoding="utf-8")
        status, out = self.tidy()
        self.assertEqual(status, 0, out)
        self.assertTrue(path.exists())
        self.assertIn("3-local", self.branches())
        self.assertIn(f"its worktree {path} has uncommitted changes or a merge in progress, left for its owner", out)

    def test_master_and_a_branch_that_tracks_it_are_never_touched(self):
        path = self.trees / "follows-master"
        self.git("worktree", "add", "--quiet", "--track", "-b", "follows-master", str(path), "origin/master")
        self.merged.insert(0, {"headRefName": "master", "headRefOid": self.git("rev-parse", "master").strip(),
                               "isCrossRepository": False})
        status, out = self.tidy()
        self.assertEqual(status, 0, out)
        self.assertIn("master", self.on_origin())
        self.assertIn("master", self.branches())
        self.assertIn("follows-master", self.branches(), out)
        self.assertTrue(path.exists())
        self.assertEqual(out.strip(), "tidy: nothing to retire")

    def test_a_checkout_with_nothing_merged_says_so(self):
        self.worktree("2-open", "2-open")
        status, out = self.tidy()
        self.assertEqual((status, out.strip()), (0, "tidy: nothing to retire"))
        self.assertIn("2-open", self.branches())

    def test_a_named_branch_is_retired_as_before(self):
        path, _ = self.worktree("1-no-pull-request", "1-no-pull-request", push=False)
        status, out = self.tidy("1-no-pull-request")
        self.assertEqual(status, 0, out)
        self.assertFalse(path.exists())
        self.assertNotIn("1-no-pull-request", self.branches())


if __name__ == "__main__":
    unittest.main()
