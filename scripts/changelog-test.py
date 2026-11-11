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
import unittest.mock
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

    def git(self, *args, at=None):
        self.clock += 60
        when = f"{at if at is not None else self.clock} +0000"
        env = {**os.environ, "GIT_AUTHOR_NAME": "A Test", "GIT_AUTHOR_EMAIL": "test@example.invalid",
               "GIT_COMMITTER_NAME": "A Test", "GIT_COMMITTER_EMAIL": "test@example.invalid",
               "GIT_AUTHOR_DATE": when, "GIT_COMMITTER_DATE": when, "GIT_CONFIG_GLOBAL": os.devnull,
               "GIT_CONFIG_SYSTEM": os.devnull}
        done = subprocess.run(["git", *args], cwd=self.root, env=env, capture_output=True, text=True)
        self.assertEqual(done.returncode, 0, done.stderr)
        return done.stdout

    def commit(self, message, at=None):
        self.git("add", "--all")
        self.git("commit", "--quiet", "-m", message, at=at)

    def entry(self, name, text, landed=True, at=None):
        (self.root / "changelog.d" / name).write_text(text, encoding="utf-8")
        if landed:
            self.commit(f"an entry: {name}", at=at)

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
        self.entry("15-one.md", ONE)
        for name in (".DS_Store", ".15-one.md.swp", "15-one.md~"):
            (self.root / "changelog.d" / name).write_bytes(b"\xff\xfe not text")
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

    def test_an_entry_no_commit_holds_is_not_folded_and_two_of_one_commit_go_by_name(self):
        # The gate reads a fold as the lines that left committed entry files, so an entry
        # no commit holds would fold into lines it takes for appended ones.
        self.entry("7-landed.md", "- Landed.\n")
        self.entry("8-not-committed.md", "- Not committed.\n", landed=False)
        before = (self.root / "CHANGELOG.md").read_text(encoding="utf-8")
        status, printed = self.run_it("fold")
        self.assertEqual(status, 1)
        self.assertIn("8-not-committed.md is not as HEAD holds it", printed)
        self.assertNotIn("7-landed.md is not", printed)
        self.assertIn("nothing was folded", printed)
        self.assertEqual((self.root / "CHANGELOG.md").read_text(encoding="utf-8"), before)
        self.assertEqual(self.names(), ["7-landed.md", "8-not-committed.md", "README.md"])
        # Committed, with a second entry in the same commit: the two go by name.
        (self.root / "changelog.d" / "6-same-commit.md").write_text("- Same commit, 6.\n", encoding="utf-8")
        self.commit("two entries in one commit")
        self.assertEqual(self.run_it("fold")[0], 0)
        self.assertTrue((self.root / "CHANGELOG.md").read_text(encoding="utf-8").endswith(
            "- Landed.\n- Same commit, 6.\n- Not committed.\n"))

    def test_an_entry_changed_or_staged_since_its_commit_is_not_folded(self):
        # The file folded is the file HEAD holds: lines the gate has at the change's
        # base. An edit no commit holds would fold into lines it takes for appended.
        self.entry("7-landed.md", "- Landed.\n")
        before = (self.root / "CHANGELOG.md").read_text(encoding="utf-8")
        (self.root / "changelog.d" / "7-landed.md").write_text("- Landed, and edited since.\n", encoding="utf-8")
        for staged in (False, True):
            with self.subTest(staged=staged):
                if staged:
                    self.git("add", "--all")
                status, printed = self.run_it("fold")
                self.assertEqual(status, 1)
                self.assertIn("7-landed.md is not as HEAD holds it", printed)
                self.assertEqual((self.root / "CHANGELOG.md").read_text(encoding="utf-8"), before)
                self.assertEqual(self.names(), ["7-landed.md", "README.md"])

    def test_a_name_used_again_and_not_committed_yet_is_not_folded(self):
        # An earlier commit added the name, and a fold removed it. The file there now
        # is a new one that no commit holds, whatever the history says of its name.
        self.entry("1-again.md", "- The first use.\n")
        self.assertEqual(self.run_it("fold")[0], 0)
        self.commit("a fold")
        self.entry("1-again.md", "- The second use.\n", landed=False)
        status, printed = self.run_it("fold")
        self.assertEqual(status, 1)
        self.assertIn("1-again.md is not as HEAD holds it", printed)
        self.assertEqual(self.names(), ["1-again.md", "README.md"])

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

    def test_an_entry_lands_where_its_commit_is_and_not_when_its_clock_says(self):
        # The second commit carries a date a year before the first's, as a laptop whose
        # clock is behind would give it. It still landed second.
        self.entry("2-landed-first.md", "- Landed first.\n")
        self.entry("1-landed-second-dated-earlier.md", "- Landed second, dated a year earlier.\n",
                   at=self.clock - 365 * 24 * 3600)
        self.entry("3-landed-third.md", "- Landed third.\n")
        self.assertEqual(self.run_it("fold")[0], 0)
        self.assertTrue((self.root / "CHANGELOG.md").read_text(encoding="utf-8").endswith(
            "- Landed first.\n- Landed second, dated a year earlier.\n- Landed third.\n"))

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
        # One reason, the true one: nothing about commits a shallow checkout cannot see.
        self.assertEqual(len(printed), 2, printed)
        self.assertEqual(printed[1], "changelog: nothing was folded")
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


class Grown(Repository):
    """The gate `scripts/lint-docs.py` runs over a change: CHANGELOG.md gains entry lines
    as a fold moves them out of entry files, or as a correction replaces a line where
    it stands, and in no other way (ADR 0075, ADR 0076)."""

    def setUp(self):
        super().setUp()
        self.entry("1-one.md", ONE)
        self.entry("2-two.md", TWO)
        self.base = self.git("rev-parse", "HEAD").strip()

    def append(self, text):
        with open(self.root / "CHANGELOG.md", "a", encoding="utf-8") as f:
            f.write(text)

    def both(self, want):
        """The change read in the working tree, staged and not, and then as a commit."""
        self.assertEqual(changelog.grown(self.root, self.base), want, "in the working tree")
        self.git("add", "--all")
        self.assertEqual(changelog.grown(self.root, self.base), want, "staged")
        self.git("commit", "--quiet", "--allow-empty", "-m", "the change")
        head = self.git("rev-parse", "HEAD").strip()
        self.assertEqual(changelog.grown(self.root, self.base, head), want, "as a commit")
        self.assertEqual(changelog.grown(self.root, self.base), want, "against a clean tree")

    def test_a_change_that_leaves_the_changelog_alone_grows_it_by_nothing(self):
        self.entry("3-three.md", "- A third.\n", landed=False)
        (self.root / "changelog.d" / "README.md").write_text("# Entries\n\n- A list item of the page.\n", encoding="utf-8")
        self.both(0)

    def test_an_appended_entry_is_counted_line_by_line(self):
        self.append("- An entry appended as before,\n  on two lines.\n")
        self.both(2)

    def test_a_fold_grows_it_by_nothing_staged_or_not(self):
        self.assertEqual(self.run_it("fold")[0], 0)
        self.assertEqual(self.names(), ["README.md"])
        self.both(0)

    def test_a_line_appended_beside_a_fold_is_counted(self):
        self.assertEqual(self.run_it("fold")[0], 0)
        self.append("- An entry appended under cover of the fold.\n")
        self.both(1)

    def test_an_entry_file_removed_that_does_not_hold_the_line_excuses_nothing(self):
        (self.root / "changelog.d" / "1-one.md").unlink()
        self.append("- An entry appended as before.\n")
        self.both(1)

    def test_a_line_moved_out_of_an_entry_file_that_stays_is_a_fold_of_that_line(self):
        # A fold is the lines that left the entry files, a whole file or not.
        (self.root / "changelog.d" / "2-two.md").write_text("- A second change.\n", encoding="utf-8")
        self.append("- And a third, in the same file.\n")
        self.both(0)

    def test_an_entry_renamed_holds_its_lines_still_and_excuses_none(self):
        self.git("mv", "changelog.d/2-two.md", "changelog.d/9-renamed.md")
        self.append("- A second change.\n")
        self.both(1)

    def test_a_correction_replaces_a_line_and_grows_it_by_nothing(self):
        path = self.root / "CHANGELOG.md"
        path.write_text(path.read_text(encoding="utf-8").replace("- The first thing, which was here",
                                                                 "- The first thing, which has been here"),
                        encoding="utf-8")
        self.both(0)

    def test_a_correction_that_adds_a_line_is_counted_by_the_line_it_adds(self):
        path = self.root / "CHANGELOG.md"
        path.write_text(path.read_text(encoding="utf-8").replace("  before any file.\n",
                                                                 "  before any file,\n  and after.\n"),
                        encoding="utf-8")
        self.both(1)

    def test_a_fold_with_a_correction_beside_it_grows_it_by_nothing(self):
        self.assertEqual(self.run_it("fold")[0], 0)
        path = self.root / "CHANGELOG.md"
        path.write_text(path.read_text(encoding="utf-8").replace("- The first thing, which was here",
                                                                 "- The first thing, which has been here"),
                        encoding="utf-8")
        self.both(0)

    def test_an_entry_corrected_and_folded_in_one_change_is_counted_however_it_is_committed(self):
        # The gate reads a change's two ends. Commit by commit a correction to the entry
        # and then a fold each pass; as the one change CI reads, the folded line is none
        # the entry files held where the change began. So the correction lands first.
        (self.root / "changelog.d" / "2-two.md").write_text(
            "- A second change, corrected.\n- And a third, in the same file.\n", encoding="utf-8")
        self.commit("a correction to the entry")
        self.assertEqual(changelog.grown(self.root, self.base, "HEAD"), 0, "the correction alone")
        self.assertEqual(self.run_it("fold")[0], 0)
        self.both(1)
        self.assertEqual(changelog.grown(self.root, "HEAD~1", "HEAD"), 0, "the fold alone")

    def test_an_entry_added_and_folded_in_one_change_is_counted(self):
        # The entry files held none of its lines where the change began.
        self.entry("3-three.md", "- A third, added by the change that folds it.\n")
        self.assertEqual(self.run_it("fold")[0], 0)
        self.both(1)

    def test_a_name_used_again_after_the_fold_in_one_change_is_still_a_fold(self):
        self.assertEqual(self.run_it("fold")[0], 0)
        self.commit("a fold")
        self.entry("1-one.md", "- A new entry under a name the fold freed.\n", landed=False)
        self.both(0)

    def test_a_pull_request_reads_the_same_as_its_merge_and_as_its_squash(self):
        # CI reads a pull request as one commit, its merge with master, and master's
        # run reads it as its squash: what the branch's own commits were is not there.
        for name, correct, want in (("a-fold", False, 0), ("a-correction-and-a-fold", True, 1)):
            with self.subTest(name):
                self.git("checkout", "--quiet", "-b", name, self.base)
                if correct:
                    (self.root / "changelog.d" / "2-two.md").write_text(
                        "- A second change, corrected.\n- And a third, in the same file.\n", encoding="utf-8")
                    self.commit("a correction to the entry")
                self.assertEqual(self.run_it("fold")[0], 0)
                self.commit("a fold")
                self.assertEqual(changelog.grown(self.root, "master", name), want, "as the branch")
                self.git("checkout", "--quiet", "-b", f"{name}-merged", "master")
                self.git("merge", "--quiet", "--no-ff", "-m", "Merge the pull request", name)
                self.assertEqual(changelog.grown(self.root, "master", f"{name}-merged"), want, "as its merge")
                self.git("checkout", "--quiet", "-b", f"{name}-squashed", "master")
                self.git("merge", "--quiet", "--squash", name)
                self.git("commit", "--quiet", "-m", "The pull request, squashed")
                self.assertEqual(changelog.grown(self.root, "HEAD~1", "HEAD"), want, "as its squash")
                self.git("checkout", "--quiet", "master")

    def test_a_line_that_is_not_ascii_folds_the_same_whatever_the_locale(self):
        self.entry("3-units.md", "- A latency in µs, and a name, Größe.\n")
        self.base = self.git("rev-parse", "HEAD").strip()
        self.assertEqual(self.run_it("fold")[0], 0)
        with unittest.mock.patch.dict(os.environ, {"LC_ALL": "C", "LANG": "C"}):
            self.both(0)

    def test_a_folded_line_corrected_after_the_fold_is_counted(self):
        # The corrected line is no line an entry file held: correct the file, then fold.
        self.assertEqual(self.run_it("fold")[0], 0)
        path = self.root / "CHANGELOG.md"
        path.write_text(path.read_text(encoding="utf-8").replace("- A second change.", "- A second change, corrected."),
                        encoding="utf-8")
        self.both(1)

    def test_an_entry_taken_out_in_one_place_excuses_none_appended_in_another(self):
        (self.root / "CHANGELOG.md").write_text(HEAD + RELEASED, encoding="utf-8")
        self.commit("a release below Unreleased")
        self.base = self.git("rev-parse", "HEAD").strip()
        (self.root / "CHANGELOG.md").write_text(
            HEAD + "- An entry appended as before.\n" + RELEASED.replace("- What the first release had.\n", ""),
            encoding="utf-8")
        self.both(1)

    def test_a_branch_behind_its_base_answers_for_its_own_lines_alone(self):
        # The base went on after the branch left it: a fold, and a new entry file. Read
        # against the base's tip, the branch would be credited with the lines the base
        # gained and with the files the branch never had.
        self.git("checkout", "--quiet", "-b", "a-branch")
        self.git("checkout", "--quiet", "master")
        self.assertEqual(self.run_it("fold")[0], 0)
        self.commit("a fold on master")
        self.entry("3-three.md", "- A third, on master alone.\n")
        self.git("checkout", "--quiet", "a-branch")
        self.assertEqual(changelog.grown(self.root, "master"), 0, "a branch that left the changelog alone")
        self.append("- A third, on master alone.\n")
        self.assertEqual(changelog.grown(self.root, "master"), 1, "in the working tree")
        self.commit("an appended line")
        self.assertEqual(changelog.grown(self.root, "master", "a-branch"), 1, "as a commit")
        # With master merged in, the branch's line is still the one line it appended.
        # The merge conflicts where both sides added at the end of the file, which is
        # what ADR 0075 is about; it is resolved by keeping both, master's lines first.
        merged = subprocess.run(["git", "merge", "--no-ff", "--no-commit", "master"], cwd=self.root,
                                capture_output=True, text=True)
        self.assertNotEqual(merged.returncode, 0, "the appended line conflicts with the fold")
        (self.root / "CHANGELOG.md").write_text(
            self.git("show", "master:CHANGELOG.md") + "- A third, on master alone.\n", encoding="utf-8")
        self.commit("Merge master")
        self.assertEqual(changelog.grown(self.root, "master", "a-branch"), 1, "with the base merged in")
        self.assertEqual(changelog.grown(self.root, "master"), 1, "with the base merged in, against the tree")

    def test_a_file_in_a_directory_under_changelog_d_is_no_entry_at_either_end(self):
        (self.root / "changelog.d" / "sub").mkdir()
        (self.root / "changelog.d" / "sub" / "9-nested.md").write_text("- Nested.\n", encoding="utf-8")
        self.commit("a nested file")
        self.base = self.git("rev-parse", "HEAD").strip()
        (self.root / "changelog.d" / "sub" / "9-nested.md").unlink()
        self.append("- Nested.\n")
        self.both(1)

    def test_a_change_git_cannot_read_has_no_answer(self):
        self.append("- An entry appended as before.\n")
        for base, head in (("no-such-commit", None), (self.base, "no-such-commit"), ("0" * 40, None)):
            with self.subTest(base=base, head=head), self.assertRaises(changelog.Unreadable) as raised:
                changelog.grown(self.root, base, head)
            self.assertIn("names no commit in this checkout", str(raised.exception))
        with self.assertRaises(changelog.Unreadable) as raised:
            changelog.files_at(self.root, "0" * 40, "changelog.d/")
        self.assertIn("git could not list changelog.d/", str(raised.exception))
        self.git("checkout", "--quiet", "--orphan", "unrelated")
        self.git("commit", "--quiet", "--allow-empty", "-m", "a history of its own")
        with self.assertRaises(changelog.Unreadable) as raised:
            changelog.grown(self.root, self.base, "unrelated")
        self.assertIn("share no commit", str(raised.exception))


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
