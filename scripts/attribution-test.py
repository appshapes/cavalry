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
# 2026-11-25 hold them.
LINES = ("Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>",
         "Co-authored-by: Claude <noreply@anthropic.com>",
         "Claude-Session: https://claude.ai/code/session_0123456789abcdefghijklmn",
         "\U0001F916 Generated with [Claude Code](https://claude.com/claude-code)",
         "https://claude.ai/code/session_0123456789abcdefghijklmn")


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

    def check(self):
        """The command that `/pr` gives, with `<file>` in it."""
        found = re.findall(r"^   (grep -n -i -E '[^']+' <file> \|\| echo \"no attribution lines\")$",
                           read(".claude", "skills", "pr", "SKILL.md"), flags=re.M)
        self.assertEqual(len(found), 1, "the pr skill gives one check of a body's file")
        return found[0]

    def run_check(self, body):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "body.md")
            with open(path, "w", encoding="utf-8") as f:
                f.write(body)
            result = subprocess.run(["sh", "-c", self.check().replace("<file>", '"$1"'), "sh", path],
                                    capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def test_the_check_names_each_line_a_body_carries(self):
        for at, line in enumerate(LINES):
            out = self.run_check(f"What changed.\n\nThe evidence.\n\n{line}\n")
            self.assertEqual(out, f"5:{line}\n", LINES[at])
        out = self.run_check("What changed.\n\n" + "\n".join(LINES) + "\n")
        self.assertEqual(out.count("\n"), len(LINES))

    def test_the_check_passes_a_body_that_carries_none(self):
        """A person's co-author line, a body that names the lines in a sentence, as the
        body of work item 475's own pull request does, and the skill's own step."""
        for body in ("What changed.\n\nCloses #12\n",
                     "A fix.\n\nCo-authored-by: A Person <person@example.org>\n",
                     'The body has no `Co-Authored-By:` line that names Claude, no `Claude-Session:` line\n'
                     'and no "Generated with Claude Code" line. See https://claude.com/claude-code.\n',
                     read(".claude", "skills", "pr", "SKILL.md"), read(".claude", "skills", "commit", "SKILL.md"),
                     read("changelog.d", "475-no-attribution-lines-in-commits.md")):
            self.assertEqual(self.run_check(body), "no attribution lines\n", body[:60])


if __name__ == "__main__":
    unittest.main()
