#!/usr/bin/env python3
"""Tests of the rule that a commit message and a pull request body carry no attribution
lines (work item 475; the owner's ruling of 2026-11-26): the project's settings turn
Claude Code's lines off, the two skills state the rule, and the check that `/pr` gives
for a pull request's body finds each line and nothing else.

Run by `just test` through `test-scripts`. Python 3.9 standard library and `grep`
(ADR 0003).
"""

import json
import os
import re
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The lines as Claude Code writes them, and as master's commits of 2026-11-22 to
# 2026-11-25 hold them; then other forms of the same three: a co-author line that names
# Claude with another address or has Anthropic's address alone, one inside a quote and
# one in a list, a session's line without its link, and the "Generated with" line with
# no link and with another.
LINES = ("Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>",
         "Co-authored-by: Claude <noreply@anthropic.com>",
         "Claude-Session: https://claude.ai/code/session_0123456789abcdefghijklmn",
         "\U0001F916 Generated with [Claude Code](https://claude.com/claude-code)",
         "https://claude.ai/code/session_0123456789abcdefghijklmn",
         "Co-Authored-By: Claude <someone@example.org>",
         "Co-authored-by: A Model <noreply@anthropic.com>",
         "Co-authored-by: Claude",
         "> Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>",
         "  - co-authored-by: Claude <noreply@anthropic.com>",
         "Claude-Session: 0123456789abcdef",
         "Generated with Claude Code",
         "\U0001F916 Generated with [Claude Code](https://example.org/another-link)")


def read(*path):
    with open(os.path.join(ROOT, *path), encoding="utf-8") as f:
        return f.read()


class Settings(unittest.TestCase):
    def test_the_projects_settings_turn_every_attribution_line_off(self):
        """`commit` and `pr` as empty strings hide the two texts, and `sessionUrl` false
        the session's link, in the form that a Claude Code before 2.1.281 reads too
        (the settings reference, `attribution`)."""
        settings = json.loads(read(".claude", "settings.json"))
        self.assertEqual(settings["attribution"], {"commit": "", "pr": "", "sessionUrl": False})

    def test_the_projects_settings_hold_nothing_else(self):
        """Every session in the repository reads this file, and so does the loop's agent,
        whose permissions are `claude.yml`'s. A key added here is added for all of them:
        change this test with it, and say why in the commit message."""
        self.assertEqual(sorted(json.loads(read(".claude", "settings.json"))), ["attribution"])


class Skills(unittest.TestCase):
    def test_both_skills_and_the_agent_state_the_rule_with_each_lines_name(self):
        for path in ((".claude", "skills", "commit", "SKILL.md"), (".claude", "skills", "pr", "SKILL.md"),
                     (".claude", "agents", "developer.md"), ("docs", "how-to", "implement-a-feature.md")):
            text = " ".join(read(*path).split())
            self.assertRegex(text, r"(?i)no attribution lines", path)
            for name in ("`Claude-Session:`", '"Generated with Claude Code"'):
                self.assertIn(name, text, path)
            self.assertRegex(text, r"(?i)`co-authored-by:?`", path)
            self.assertIn("work item 475", text, path)

    def test_each_skill_states_the_rule_in_the_step_that_writes_the_text(self):
        """`/commit` where the message is written and `/pr` where the body is, so that a
        session reads the rule at the moment it applies."""
        for path, start, end in (((".claude", "skills", "commit", "SKILL.md"),
                                  "\n## 5. Write the commit message\n", "\n## 6. Execute\n"),
                                 ((".claude", "skills", "pr", "SKILL.md"),
                                  "\n6. **Push and open.**", "\n7. **Confirm the review started.**")):
            text = read(*path)
            self.assertEqual((text.count(start), text.count(end)), (1, 1), path)
            step = text[text.index(start):text.index(end)]
            self.assertRegex(step, r"(?i)no attribution lines", path)
            self.assertIn("work item 475", " ".join(step.split()), path)
        self.assertIn(self.check(), step, "the check is in the step of /pr that writes the body")

    def check(self):
        """The command that `/pr` gives, with `<file>` where the body's file goes."""
        found = [line.strip() for line in read(".claude", "skills", "pr", "SKILL.md").splitlines()
                 if line.strip().startswith("grep ") and "no attribution lines" in line]
        self.assertEqual(len(found), 1, "the pr skill gives one check of a body's file, on one line that "
                         "begins with `grep` and prints `no attribution lines`")
        self.assertIn("<file>", found[0], "the check names the body's file as `<file>`: this test puts a path there")
        return found[0]

    def run_check(self, body, missing=False):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "body.md")
            if not missing:
                with open(path, "w", encoding="utf-8") as f:
                    f.write(body)
            result = subprocess.run(["sh", "-c", self.check().replace("<file>", '"$1"'), "sh", path],
                                    stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def test_the_check_names_each_line_a_body_carries(self):
        for at, line in enumerate(LINES):
            out = self.run_check(f"What changed.\n\nThe evidence.\n\n{line}\n")
            self.assertEqual(out, f"5:{line}\n", LINES[at])
        out = self.run_check("What changed.\n\n" + "\n".join(LINES) + "\n")
        self.assertEqual(out.count("\n"), len(LINES))

    def test_the_check_finds_a_line_after_a_byte_order_mark(self):
        out = self.run_check("\ufeff" + LINES[0] + "\n\nWhat changed.\n")
        self.assertEqual(out, "1:\ufeff" + LINES[0] + "\n")

    def test_a_file_the_check_cannot_read_is_no_pass(self):
        """`grep` ends with 2 for a path that is no file. A check that printed its
        passing words then would pass a body it never read."""
        out = self.run_check("", missing=True)
        self.assertNotIn("no attribution lines", out)
        self.assertIn("the check did not read", out)

    def test_the_check_passes_a_body_that_carries_none(self):
        """A person's co-author line, one of a person whose name begins as Claude's does,
        a body that names the lines in a sentence or quotes one inside a sentence, as
        the body of work item 475's own pull request does, and the skills' own steps."""
        for body in ("What changed.\n\nCloses #12\n",
                     "A fix.\n\nCo-authored-by: A Person <person@example.org>\n",
                     "A fix.\n\nCo-authored-by: Claudette Dupont <dupont@example.org>\n",
                     'The body has no `Co-Authored-By:` line that names Claude, no `Claude-Session:` line\n'
                     'and no "Generated with Claude Code" line. See https://claude.com/claude-code.\n',
                     "No\n`Co-Authored-By: Claude <noreply@anthropic.com>` line, no\n"
                     '"Generated with Claude Code" line and no\n`Claude-Session:` line.\n',
                     "The line was `Generated with [Claude Code](https://claude.com/claude-code)` until now.\n",
                     read(".claude", "skills", "pr", "SKILL.md"), read(".claude", "skills", "commit", "SKILL.md"),
                     read("changelog.d", "475-no-attribution-lines-in-commits.md")):
            self.assertEqual(self.run_check(body), "no attribution lines\n", body[:60])


if __name__ == "__main__":
    unittest.main()
