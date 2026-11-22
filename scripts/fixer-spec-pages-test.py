#!/usr/bin/env python3
"""Tests of fixer-spec-pages.py against made-up checkouts: which files under spec/ the
loop's agent may write, by the Accepted ADRs its branch adds (ADR 0140; work item 340),
and of the step in .github/workflows/claude.yml that asks it and checks its answer.
Standard library only, no network; `just test-scripts` runs it.

    python3 scripts/fixer-spec-pages-test.py
"""
import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "fixer-spec-pages.py"
DENY_ALL = '"Write(spec/**)", "Edit(spec/**)",'

SPEC = ("spec/README.md", "spec/cluster.md", "spec/udp.md", "spec/frame.md", "spec/vectors/cluster.py",
        "spec/vectors/cluster.json", "spec/vectors/udp.json")


def adr(status: str, *pages: str, before: str = "") -> str:
    named = " and ".join(f"`spec/{p}.md`" for p in pages) or "no page"
    return (f"# 0000. A decision\n\n{before}- **Status:** {status}\n- **Date:** 2026-11-21\n\n"
            f"## Decision\n\nIt changes {named}.\n")


class Checkout:
    """A repository whose `origin/master` holds spec/ and one old ADR, with HEAD on a branch."""

    def __init__(self, test: unittest.TestCase):
        self.dir = Path(tempfile.mkdtemp(prefix="fixer-spec-"))
        test.addCleanup(lambda: subprocess.run(["rm", "-rf", str(self.dir)], check=False))
        self.git("init", "-q", "-b", "master")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "user.name", "test")
        for path in SPEC:
            self.write(path, f"{path}\n")
        # On master alone: Accepted, and it names a page. It gives a branch nothing.
        self.write("docs/decisions/0001-an-old-decision.md", adr("Accepted", "udp"))
        self.commit("base")
        self.git("update-ref", "refs/remotes/origin/master", "HEAD")
        self.git("switch", "-q", "-c", "a-branch")

    def git(self, *args: str) -> str:
        return subprocess.run(("git",) + args, cwd=self.dir, check=True, capture_output=True, text=True).stdout

    def write(self, path: str, text: str) -> None:
        target = self.dir / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def commit(self, message: str) -> None:
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)

    def decide(self, *pages: str, status: str = "Accepted", name: str = "0002-a-new-decision.md") -> None:
        self.write(f"docs/decisions/{name}", adr(status, *pages))
        self.commit("the decision")

    def ask(self, **env: str):
        done = subprocess.run([sys.executable, "-I", str(SCRIPT)], cwd=self.dir, capture_output=True, text=True,
                              env={"PATH": os.environ["PATH"], **env})
        return done.returncode, done.stdout.strip(), done.stderr


def denied(answer: str) -> set:
    """The files an answer denies, read as the entries of a JSON array, as the workflow splices them."""
    rules = json.loads("[" + answer + ' "end"]')[:-1]
    writes = {r[len("Write("):-1] for r in rules if r.startswith("Write(")}
    edits = {r[len("Edit("):-1] for r in rules if r.startswith("Edit(")}
    assert writes == edits and len(rules) == 2 * len(writes), rules
    return writes


class Pages(unittest.TestCase):
    def test_a_branch_with_no_adr_is_denied_all_of_spec(self):
        repo = Checkout(self)
        repo.write("crates/x.rs", "fn x() {}\n")
        repo.commit("code alone")
        code, answer, said = repo.ask()
        self.assertEqual((code, answer), (0, DENY_ALL))
        self.assertIn("every write under spec/ is denied", said)

    def test_an_accepted_adr_the_branch_adds_opens_its_page_its_vectors_and_the_table(self):
        repo = Checkout(self)
        repo.decide("cluster")
        code, answer, said = repo.ask()
        self.assertEqual(code, 0)
        self.assertEqual(denied(answer), {"spec/udp.md", "spec/frame.md", "spec/vectors/udp.json"})
        self.assertNotIn("spec/**", answer)
        self.assertTrue(answer.endswith(","), answer)
        self.assertEqual(len(answer.splitlines()), 1)
        self.assertIn("docs/decisions/0002-a-new-decision.md is Accepted and names spec/cluster.md", said)
        self.assertIn("writable: spec/README.md, spec/cluster.md, spec/vectors/cluster.json, spec/vectors/cluster.py", said)

    def test_two_adrs_open_both_their_pages_and_no_more(self):
        repo = Checkout(self)
        repo.decide("cluster")
        repo.decide("udp", name="0003-another.md")
        self.assertEqual(denied(repo.ask()[1]), {"spec/frame.md"})

    def test_an_adr_master_has_opens_nothing_changed_or_not(self):
        # 0001 is on master, Accepted, and names spec/udp.md.
        repo = Checkout(self)
        repo.write("crates/x.rs", "fn x() {}\n")
        repo.commit("code alone")
        self.assertEqual(repo.ask()[1], DENY_ALL)
        repo.write("docs/decisions/0001-an-old-decision.md", adr("Accepted", "udp") + "\n")
        repo.commit("one more line on master's decision")
        self.assertEqual(repo.ask()[1], DENY_ALL)

    def test_an_adr_that_is_not_accepted_opens_nothing(self):
        for status in ("Proposed", "Rejected", "Superseded by 0003", "Accepted, with reservations", "accepted"):
            with self.subTest(status):
                repo = Checkout(self)
                repo.decide("cluster", status=status)
                self.assertEqual(repo.ask()[1], DENY_ALL)

    def test_the_first_status_line_is_the_one_read(self):
        repo = Checkout(self)
        text = adr("Rejected", "cluster") + "\nAn example of a header:\n\n```\n- **Status:** Accepted\n```\n"
        repo.write("docs/decisions/0002-a-new-decision.md", text)
        repo.commit("rejected, with Accepted in an example")
        self.assertEqual(repo.ask()[1], DENY_ALL)

    def test_a_page_that_is_not_there_opens_nothing(self):
        repo = Checkout(self)
        repo.decide("nothing")
        self.assertEqual(repo.ask()[1], DENY_ALL)

    def test_a_page_the_branch_deleted_is_still_denied_by_name(self):
        repo = Checkout(self)
        repo.git("rm", "-q", "spec/frame.md")
        repo.decide("cluster")
        self.assertIn("spec/frame.md", denied(repo.ask()[1]))

    def test_when_every_file_is_writable_the_answer_still_has_an_entry(self):
        repo = Checkout(self)
        repo.git("rm", "-q", "spec/frame.md")
        repo.commit("no third page")
        repo.git("update-ref", "refs/remotes/origin/master", "HEAD")
        repo.decide("cluster")
        repo.decide("udp", name="0003-another.md")
        self.assertEqual(denied(repo.ask()[1]), {"spec/.nothing-else"})

    def test_an_adr_the_working_tree_holds_and_no_commit_does_opens_nothing(self):
        # What counts is what the branch has committed: an unsaved file is not a decision.
        repo = Checkout(self)
        repo.write("docs/decisions/0002-a-new-decision.md", adr("Accepted", "cluster"))
        self.assertEqual(repo.ask()[1], DENY_ALL)

    def test_at_a_merge_commit_of_the_branch_into_master_the_branchs_adr_counts(self):
        # A fix run is asked at the pull request's merge commit.
        repo = Checkout(self)
        repo.decide("cluster")
        repo.git("switch", "-q", "master")
        repo.write("crates/y.rs", "fn y() {}\n")
        repo.commit("master moves")
        repo.git("update-ref", "refs/remotes/origin/master", "HEAD")
        repo.git("merge", "-q", "--no-ff", "-m", "merge", "a-branch")
        self.assertEqual(denied(repo.ask()[1]), {"spec/udp.md", "spec/frame.md", "spec/vectors/udp.json"})

    def test_a_tag_named_origin_master_does_not_stand_in_for_the_branch(self):
        # The short name `origin/master` reads a tag of that name before the remote-tracking
        # branch. The script names the branch in full, so a tag at the base of everything,
        # which would make every ADR the branch's, changes nothing.
        repo = Checkout(self)
        empty = repo.git("commit-tree", "-m", "empty", repo.git("hash-object", "-t", "tree", "/dev/null").strip()).strip()
        repo.git("tag", "origin/master", empty)
        repo.write("crates/x.rs", "fn x() {}\n")
        repo.commit("code alone")
        self.assertEqual(repo.ask()[1], DENY_ALL)

    def test_a_file_name_the_settings_cannot_hold_denies_all(self):
        repo = Checkout(self)
        repo.write("spec/café.md", "a name git would quote\n")
        repo.decide("cluster")
        code, answer, said = repo.ask()
        self.assertEqual((code, answer), (0, DENY_ALL))
        self.assertIn("a name the settings cannot hold", said)

    def test_a_base_that_cannot_be_read_denies_all_and_does_not_fail(self):
        repo = Checkout(self)
        repo.decide("cluster")
        code, answer, said = repo.ask(FIXER_SPEC_BASE="refs/remotes/origin/no-such-branch")
        self.assertEqual((code, answer), (0, DENY_ALL))
        self.assertIn("could not read the branch", said)

    def test_outside_a_repository_all_is_denied(self):
        with tempfile.TemporaryDirectory() as empty:
            done = subprocess.run([sys.executable, "-I", str(SCRIPT)], cwd=empty, capture_output=True, text=True,
                                  env={"PATH": os.environ["PATH"], "GIT_CEILING_DIRECTORIES": empty})
        self.assertEqual((done.returncode, done.stdout.strip()), (0, DENY_ALL))


class Workflow(unittest.TestCase):
    """claude.yml asks master's copy of the script before anything is installed, takes the answer only
    when it is deny entries for spec/, and denies all without one."""

    def setUp(self):
        self.text = (ROOT / ".github" / "workflows" / "claude.yml").read_text(encoding="utf-8")
        start = self.text.index("      - name: Which pages of spec/ this run may write\n")
        self.step = self.text[start:self.text.index("\n      - name: ", start + 1)]
        self.fallback = "${{ steps.spec.outputs.deny || '" + DENY_ALL + "' }}"

    def settings(self, answer: str) -> dict:
        block = self.text[self.text.index("          settings: |\n"):]
        block = block[:block.index("          # Absent from `allow` on purpose")]
        body = block.split("          settings: |\n", 1)[1].replace(self.fallback, answer)
        return json.loads(re.sub(r"^            ", "", body, flags=re.M))

    def test_the_step_reads_masters_copy_by_the_branchs_full_name(self):
        self.assertIn("git show refs/remotes/origin/master:scripts/fixer-spec-pages.py | env -u FIXER_SPEC_BASE python3 -I -",
                      self.step)
        self.assertNotRegex(self.step, r"git show origin/master")
        self.assertIn("        id: spec\n", self.step)
        self.assertIn("        continue-on-error: true\n", self.step)
        self.assertIn("        shell: bash\n", self.step)

    def test_the_step_runs_after_the_checkout_and_before_anything_is_installed_or_restored(self):
        at = self.text.index("      - name: Which pages of spec/ this run may write\n")
        self.assertLess(self.text.index("      - name: Checkout\n"), at)
        for later in ("Install the Rust toolchain", "Install just", "Install mdbook", "Restore the Rust cache", "Code"):
            with self.subTest(later):
                self.assertLess(at, self.text.index(f"      - name: {later}\n"))

    def test_the_step_takes_only_deny_entries_for_spec(self):
        pattern = re.search(r"entries='(.+)'\n", self.step).group(1)
        taken = lambda answer: re.fullmatch(pattern[1:-1], answer) is not None    # without ^ and $
        self.assertTrue(pattern.startswith("^") and pattern.endswith("$"))
        self.assertIn('if [[ "$answer" =~ $entries ]]; then\n            echo "deny=$answer" >> "$GITHUB_OUTPUT"', self.step)
        self.assertTrue(taken(DENY_ALL))
        self.assertTrue(taken('"Write(spec/udp.md)", "Edit(spec/udp.md)", "Write(spec/vectors/udp.json)", "Edit(spec/vectors/udp.json)",'))
        self.assertTrue(taken('"Write(spec/.nothing-else)", "Edit(spec/.nothing-else)",'))
        for hostile in ('"x"], "allow": ["Bash", "Write", "Edit", "WebFetch"], "deny": [',
                        '"Write(spec/a.md)", "x"], "allow": ["Bash"], "deny": [',
                        DENY_ALL + '\n"x"], "allow": ["Bash"],',
                        '"Write(src/main.rs)", "Edit(src/main.rs)",',
                        '"Bash(spec/x)",',
                        '"Write("spec/caf\\303\\251.md")",',
                        ""):
            with self.subTest(hostile):
                self.assertFalse(taken(hostile))

    def test_the_settings_take_the_answer_and_deny_all_without_one(self):
        self.assertEqual(self.text.count(self.fallback), 1)
        block = self.text[self.text.index("          settings: |\n"):]
        block = block[:block.index("          # Absent from `allow` on purpose")]
        # Nothing under spec/ is named in the settings but through the step's answer.
        self.assertEqual(block.count("spec/"), self.fallback.count("spec/"))
        for answer in (DENY_ALL, '"Write(spec/udp.md)", "Edit(spec/udp.md)",'):
            with self.subTest(answer):
                deny = self.settings(answer)["permissions"]["deny"]
                for entry in json.loads("[" + answer + ' "end"]')[:-1]:
                    self.assertIn(entry, deny)
                self.assertIn("Write(bench/baseline.json)", deny)
                self.assertIn("Edit(.claude/**)", deny)
                self.assertIn("Write(.github/workflows/**)", deny)


if __name__ == "__main__":
    unittest.main()
