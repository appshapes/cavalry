#!/usr/bin/env python3
"""Tests of the range `scripts/lint-docs.py` reads in a run that is neither a pull
request's nor a push's: at a branch it is the branch's own change, and at the default
branch or a tag the last commit (work item 479). In a made-up repository with a
made-up origin.

Run by `just test` through `test-scripts`. Python 3.9 standard library and `git`
(ADR 0003).
"""

import importlib.util
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
KEYS = ("LINT_DOCS_RANGE", "GITHUB_EVENT_NAME", "GITHUB_REF", "GITHUB_EVENT_PATH", "GITHUB_BASE_REF")


def load():
    spec = importlib.util.spec_from_file_location("lint_docs", HERE / "lint-docs.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Range(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.kept = {key: os.environ.pop(key, None) for key in KEYS}
        self.git("init", "-q", "-b", "master")
        self.master = [self.commit("one"), self.commit("two")]
        self.git("update-ref", "refs/remotes/origin/master", self.master[-1])
        self.lint = load()
        self.lint.ROOT = self.root

    def tearDown(self):
        for key, value in self.kept.items():
            os.environ.pop(key, None)
            if value is not None:
                os.environ[key] = value
        self.directory.cleanup()

    def git(self, *args):
        return subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@example.org", *args], cwd=self.root,
                              check=True, capture_output=True, text=True).stdout.strip()

    def commit(self, name):
        (self.root / name).write_text(name + "\n", encoding="utf-8")
        self.git("add", name)
        self.git("commit", "-q", "-m", name)
        return self.git("rev-parse", "HEAD")

    def picked(self, event, ref, default=None):
        os.environ["GITHUB_EVENT_NAME"] = event
        os.environ["GITHUB_REF"] = ref
        if default:
            path = self.root / "event.json"
            path.write_text(json.dumps({"repository": {"default_branch": default}}), encoding="utf-8")
            os.environ["GITHUB_EVENT_PATH"] = str(path)
        return self.lint.pick_range()

    def test_a_run_by_hand_at_a_branch_reads_the_branchs_own_change(self):
        """Two commits on the branch, the second of which edits what the first added:
        the range begins where the branch left `master`, not at the commit before."""
        self.git("switch", "-q", "-c", "work")
        self.commit("three")
        self.commit("four")
        for event in ("workflow_dispatch", "schedule"):
            self.assertEqual(self.picked(event, "refs/heads/work"), (self.master[-1], "HEAD"), event)

    def test_a_branch_that_is_behind_master_still_reads_its_own_change_alone(self):
        self.git("switch", "-q", "-c", "work", self.master[0])
        self.commit("three")
        self.assertEqual(self.picked("workflow_dispatch", "refs/heads/work"), (self.master[0], "HEAD"))

    def test_at_the_default_branch_it_reads_the_last_commit(self):
        for event in ("workflow_dispatch", "schedule"):
            self.assertEqual(self.picked(event, "refs/heads/master"), ("HEAD~1", "HEAD"), event)

    def test_the_default_branch_is_the_one_the_event_names(self):
        self.git("update-ref", "refs/remotes/origin/main", self.master[0])
        self.assertEqual(self.picked("workflow_dispatch", "refs/heads/master", default="main"), (self.master[0], "HEAD"))
        self.assertEqual(self.picked("workflow_dispatch", "refs/heads/main", default="main"), ("HEAD~1", "HEAD"))

    def test_at_a_tag_it_reads_the_last_commit(self):
        self.git("switch", "-q", "-c", "release")
        self.commit("three")
        self.assertEqual(self.picked("workflow_dispatch", "refs/tags/v0.1.0"), ("HEAD~1", "HEAD"))

    def test_a_branch_with_nothing_of_its_own_reads_the_last_commit(self):
        self.git("switch", "-q", "-c", "work")
        self.assertEqual(self.picked("workflow_dispatch", "refs/heads/work"), ("HEAD~1", "HEAD"))

    def test_a_checkout_without_the_default_branch_reads_the_last_commit(self):
        self.git("update-ref", "-d", "refs/remotes/origin/master")
        self.git("switch", "-q", "-c", "work")
        self.commit("three")
        self.assertEqual(self.picked("workflow_dispatch", "refs/heads/work"), ("HEAD~1", "HEAD"))

    def test_a_pull_request_and_a_set_range_are_read_as_before(self):
        os.environ["GITHUB_BASE_REF"] = "master"
        self.assertEqual(self.picked("pull_request", "refs/pull/1/merge"), ("origin/master", "HEAD"))
        os.environ["LINT_DOCS_RANGE"] = "HEAD~1..HEAD"
        self.assertEqual(self.picked("workflow_dispatch", "refs/heads/work"), ("HEAD~1", "HEAD"))


if __name__ == "__main__":
    unittest.main()
