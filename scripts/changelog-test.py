#!/usr/bin/env python3
"""Tests of changelog.py: what `check` takes for an entry file and what it refuses, and
`fold` in a repository of its own, where the order entries landed in is the order of
their commits. No network. Standard library only; `just test-scripts` runs it.

    python3 scripts/changelog-test.py
"""
import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
spec = importlib.util.spec_from_file_location("changelog", HERE / "changelog.py")
changelog = importlib.util.module_from_spec(spec)
spec.loader.exec_module(changelog)

HEAD = "# Changelog\n\n## Unreleased\n\n### Added\n\n- The first thing, which was here\n  before any file.\n"
RELEASED = "\n## 0.1.0, 2026-01-01\n\n### Added\n\n- What the first release had.\n"
ONE = "- `cavalry doctor` now reports the thing\n  ([a page](docs/how-to/a-page.md); card 1).\n"
TWO = "- A second change.\n- And a third, in the same file.\n"


class Repository(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.clock = 1_700_000_000
        self.git("init", "--quiet", "--initial-branch=master")
        (self.root / "CHANGELOG.md").write_text(HEAD, encoding="utf-8")
        (self.root / "changelog.d").mkdir()
        (self.root / "changelog.d" / "README.md").write_text("# Entries\n\nNot an entry.\n", encoding="utf-8")
        (self.root / "docs" / "how-to").mkdir(parents=True)
        (self.root / "docs" / "how-to" / "a-page.md").write_text("# A page\n\n## A heading\n", encoding="utf-8")
        self.commit("the changelog")

    def git(self, *args):
        self.clock += 60
        when = f"{self.clock} +0000"
        env = {**os.environ, "GIT_AUTHOR_NAME": "A Test", "GIT_AUTHOR_EMAIL": "test@example.invalid",
               "GIT_COMMITTER_NAME": "A Test", "GIT_COMMITTER_EMAIL": "test@example.invalid",
               "GIT_AUTHOR_DATE": when, "GIT_COMMITTER_DATE": when, "GIT_CONFIG_GLOBAL": os.devnull,
               "GIT_CONFIG_SYSTEM": os.devnull}
        done = subprocess.run(["git", *args], cwd=self.root, env=env, capture_output=True, text=True)
        self.assertEqual(done.returncode, 0, done.stderr)
        return done.stdout

    def commit(self, message):
        self.git("add", "--all")
        self.git("commit", "--quiet", "-m", message)

    def entry(self, name, text, landed=True):
        (self.root / "changelog.d" / name).write_text(text, encoding="utf-8")
        if landed:
            self.commit(f"an entry: {name}")

    def run_it(self, *argv):
        printed = []
        status = changelog.main(list(argv), self.root, printed.append)
        return status, "\n".join(printed)

    def names(self):
        return sorted(path.name for path in (self.root / "changelog.d").iterdir())


class Check(Repository):
    def test_no_entry_and_well_formed_entries_pass_and_the_readme_is_no_entry(self):
        status, text = self.run_it("check")
        self.assertEqual((status, text), (0, "changelog: 0 entries in changelog.d/, ok"))
        self.entry("1-doctor-reports-the-thing.md", ONE)
        self.entry("200-two-more.md", TWO)
        status, text = self.run_it("check")
        self.assertEqual((status, text), (0, "changelog: 2 entries in changelog.d/, ok"))
        self.git("rm", "--quiet", "changelog.d/200-two-more.md")
        self.assertEqual(self.run_it("check"), (0, "changelog: 1 entry in changelog.d/, ok"))

    def test_each_shape_fold_could_not_place_is_refused_and_named(self):
        cases = (
            ("1-no-dash.md", "A change with no dash.\n", "1-no-dash.md: the first line begins `- `"),
            ("2-empty.md", "", "2-empty.md: the first line begins `- `"),
            ("3-no-break.md", "- A change", "3-no-break.md: the last line has no line break"),
            ("4-blank.md", "- A change.\n\n- Another.\n", "4-blank.md:2: an empty line"),
            ("5-heading.md", "- A change.\n### Added\n", "5-heading.md:2: a line begins `- `, a new entry, or two spaces"),
            ("6-tab.md", "- A change\n\tcontinued.\n", "6-tab.md:2: a line begins"),
            ("7-space.md", "- A change. \n", "7-space.md:1: the line ends in white space"),
            ("8-indent.md", " - A change.\n", "8-indent.md: the first line begins `- `"),
            ("9-Nine.md", ONE, "9-Nine.md: the name is the card's number, or the pull request's, a hyphen"),
            ("10 spaces.md", ONE, "10 spaces.md: the name is"),
            ("11-entry.txt", ONE, "11-entry.txt: the name is"),
            ("fix.md", ONE, "fix.md: the name is"),
            ("pr-200-two-more.md", ONE, "pr-200-two-more.md: the name is"),
            ("12-link.md", "- A change ([a page](docs/how-to/no-such-page.md)).\n",
             "12-link.md: the link to docs/how-to/no-such-page.md names no file from the repository's root"),
            ("13-link.md", "- A change ([how](changelog.d/no-such.md), [it](../docs/how-to/a-page.md)).\n",
             "13-link.md: the link to changelog.d/no-such.md names no file"),
        )
        for name, text, want in cases:
            with self.subTest(name):
                path = self.root / "changelog.d" / name
                path.write_text(text, encoding="utf-8")
                status, printed = self.run_it("check")
                path.unlink()
                self.assertEqual(status, 1, printed)
                self.assertIn(f"changelog: {want}", printed)
                self.assertIn("(changelog.d/README.md)", printed)
                self.assertNotIn(", ok", printed)

    def test_a_link_from_the_root_an_anchor_and_an_address_pass(self):
        self.entry("14-links.md", "- A change ([a page](docs/how-to/a-page.md#a-heading), [here](#top),\n"
                                  "  [there](https://example.invalid/x), [how](changelog.d/README.md)).\n")
        self.assertEqual(self.run_it("check"), (0, "changelog: 1 entry in changelog.d/, ok"))

    def test_what_an_editor_or_a_file_manager_leaves_is_no_entry(self):
        for name in (".DS_Store", ".15-one.md.swp", "15-one.md~"):
            (self.root / "changelog.d" / name).write_bytes(b"\xff\xfe not text")
        self.entry("15-one.md", ONE)
        self.assertEqual(self.run_it("check"), (0, "changelog: 1 entry in changelog.d/, ok"))
        self.assertEqual(self.run_it("fold")[0], 0)
        self.assertEqual(self.names(), [".15-one.md.swp", ".DS_Store", "15-one.md~", "README.md"])

    def test_a_directory_and_a_file_that_is_not_text_are_refused(self):
        (self.root / "changelog.d" / "12-a-directory.md").mkdir()
        (self.root / "changelog.d" / "13-not-text.md").write_bytes(b"- \xff\xfe\n")
        status, printed = self.run_it("check")
        self.assertEqual(status, 1)
        self.assertIn("12-a-directory.md is not a file", printed)
        self.assertIn("13-not-text.md could not be read as UTF-8", printed)

    def test_a_repository_with_no_directory_has_no_entry(self):
        self.git("rm", "--quiet", "-r", "changelog.d")
        self.assertEqual(self.run_it("check"), (0, "changelog: 0 entries in changelog.d/, ok"))


class Fold(Repository):
    def test_entries_are_appended_in_the_order_they_landed_and_their_files_removed(self):
        # Landed in this order, which is not the order of their names.
        self.entry("9-landed-first.md", "- Landed first.\n")
        self.entry("1-landed-second.md", TWO)
        self.entry("5-landed-third.md", ONE)
        status, printed = self.run_it("fold")
        self.assertEqual(status, 0, printed)
        self.assertIn("folded 3 entries into CHANGELOG.md's Unreleased section in the order they landed", printed)
        self.assertEqual((self.root / "CHANGELOG.md").read_text(encoding="utf-8"),
                         HEAD + "- Landed first.\n" + TWO + ONE)
        self.assertEqual(self.names(), ["README.md"])

    def test_a_second_fold_changes_nothing(self):
        self.entry("1-one.md", ONE)
        self.assertEqual(self.run_it("fold")[0], 0)
        folded = (self.root / "CHANGELOG.md").read_text(encoding="utf-8")
        status, printed = self.run_it("fold")
        self.assertEqual((status, printed), (0, "changelog: no entry in changelog.d/, nothing to fold"))
        self.assertEqual((self.root / "CHANGELOG.md").read_text(encoding="utf-8"), folded)

    def test_an_entry_no_commit_holds_yet_follows_those_that_landed_and_two_at_one_time_go_by_name(self):
        self.entry("7-landed.md", "- Landed.\n")
        self.entry("8-b-not-committed.md", "- Not committed, b.\n", landed=False)
        self.entry("8-a-not-committed.md", "- Not committed, a.\n", landed=False)
        self.assertEqual(self.run_it("fold")[0], 0)
        self.assertTrue((self.root / "CHANGELOG.md").read_text(encoding="utf-8").endswith(
            "- Landed.\n- Not committed, a.\n- Not committed, b.\n"))

    def test_an_entry_moved_keeps_the_place_of_its_new_name(self):
        # A rename is a new file to `git log --diff-filter=A`: the entry lands again.
        self.entry("1-first.md", "- First.\n")
        self.entry("2-second.md", "- Second.\n")
        self.git("mv", "changelog.d/1-first.md", "changelog.d/3-renamed.md")
        self.commit("a rename")
        self.assertEqual(self.run_it("fold")[0], 0)
        self.assertTrue((self.root / "CHANGELOG.md").read_text(encoding="utf-8").endswith("- Second.\n- First.\n"))

    def test_entries_go_to_the_end_of_unreleased_and_not_below_a_release(self):
        (self.root / "CHANGELOG.md").write_text(HEAD + RELEASED, encoding="utf-8")
        self.commit("a release")
        self.entry("1-one.md", ONE)
        self.entry("2-two.md", TWO)
        self.assertEqual(self.run_it("fold")[0], 0)
        self.assertEqual((self.root / "CHANGELOG.md").read_text(encoding="utf-8"), HEAD + ONE + TWO + RELEASED)

    def test_a_branchs_entry_lands_when_the_branch_is_merged_not_when_it_was_written(self):
        self.git("checkout", "--quiet", "-b", "a-branch")
        self.entry("1-written-first.md", "- Written first, merged last.\n")
        self.git("checkout", "--quiet", "master")
        self.entry("2-landed-first.md", "- Landed first.\n")
        self.git("merge", "--quiet", "--no-ff", "-m", "Merge a-branch", "a-branch")
        self.assertEqual(self.run_it("fold")[0], 0)
        self.assertTrue((self.root / "CHANGELOG.md").read_text(encoding="utf-8").endswith(
            "- Landed first.\n- Written first, merged last.\n"))

    def test_a_name_used_again_after_a_fold_lands_with_its_second_commit(self):
        self.entry("1-again.md", "- The first use.\n")
        self.assertEqual(self.run_it("fold")[0], 0)
        self.commit("a fold")
        self.entry("2-between.md", "- Between.\n")
        self.entry("1-again.md", "- The second use.\n")
        self.assertEqual(self.run_it("fold")[0], 0)
        self.assertTrue((self.root / "CHANGELOG.md").read_text(encoding="utf-8").endswith(
            "- The first use.\n- Between.\n- The second use.\n"))

    def test_a_shallow_checkout_folds_nothing_and_says_why(self):
        self.entry("1-one.md", ONE)
        self.entry("2-two.md", TWO)
        shallow = Path(self.tmp.name + "-shallow")
        subprocess.run(["git", "clone", "--quiet", "--depth", "1", self.root.as_uri(), str(shallow)], check=True,
                       capture_output=True)
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(shallow)], check=False))
        printed = []
        self.assertEqual(changelog.main(["fold"], shallow, printed.append), 1)
        self.assertIn("this checkout is shallow", "\n".join(printed))
        self.assertTrue((shallow / "changelog.d" / "1-one.md").is_file())
        self.assertEqual((shallow / "CHANGELOG.md").read_text(encoding="utf-8"), HEAD)

    def test_a_changelog_with_no_last_line_break_gets_one_before_the_entries(self):
        (self.root / "CHANGELOG.md").write_text(HEAD.rstrip("\n"), encoding="utf-8")
        self.entry("1-one.md", ONE)
        self.assertEqual(self.run_it("fold")[0], 0)
        self.assertEqual((self.root / "CHANGELOG.md").read_text(encoding="utf-8"), HEAD + ONE)

    def test_a_malformed_entry_or_a_missing_changelog_folds_nothing(self):
        self.entry("1-one.md", ONE)
        self.entry("2-bad.md", "No dash.\n")
        before = (self.root / "CHANGELOG.md").read_text(encoding="utf-8")
        status, printed = self.run_it("fold")
        self.assertEqual(status, 1)
        self.assertIn("2-bad.md: the first line begins `- `", printed)
        self.assertIn("nothing was folded", printed)
        self.assertEqual((self.root / "CHANGELOG.md").read_text(encoding="utf-8"), before)
        self.assertEqual(self.names(), ["1-one.md", "2-bad.md", "README.md"])
        (self.root / "changelog.d" / "2-bad.md").unlink()
        (self.root / "CHANGELOG.md").write_text("# Changelog\n\n## 0.1.0\n\n- Released.\n", encoding="utf-8")
        status, printed = self.run_it("fold")
        self.assertEqual(status, 1)
        self.assertIn("CHANGELOG.md has no `## Unreleased` section to fold into", printed)
        (self.root / "CHANGELOG.md").unlink()
        status, printed = self.run_it("fold")
        self.assertEqual(status, 1)
        self.assertIn("CHANGELOG.md is not there to fold into", printed)
        self.assertEqual(self.names(), ["1-one.md", "README.md"])


class ThisRepository(unittest.TestCase):
    def test_the_entries_of_this_repository_are_well_formed(self):
        printed = []
        self.assertEqual(changelog.check(ROOT, printed.append), 0, "\n".join(printed))

    def test_the_readme_the_messages_name_is_there(self):
        self.assertTrue((ROOT / changelog.DOCS).is_file())

    def test_anything_but_check_or_fold_prints_the_usage(self):
        printed = []
        self.assertEqual(changelog.main([], ROOT, printed.append), 2)
        self.assertEqual(changelog.main(["check", "fold"], ROOT, printed.append), 2)
        self.assertEqual(printed, ["usage: changelog.py check | fold"] * 2)


if __name__ == "__main__":
    unittest.main()
