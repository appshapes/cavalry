#!/usr/bin/env python3
"""Tests of the step of claude.yml that approves the run GitHub held on a fixer's push
(card 215): the step's own script, read out of the workflow file and run under bash with a
fake `gh` on PATH and a `sleep` that moves bash's clock, so there is no network, no token
and no waiting. The fake `gh` answers each call the step makes from a scenario and keeps
a log of them; it evaluates no `--jq` filter, so a listing's rows are given as the step's
filter would print them, `<id> <workflow file> <status> <conclusion>`, and the choice of
what is held and what is ci's, which the step makes in awk, is what runs here. Standard
library only; `just test-scripts` runs it.

    python3 scripts/claude-workflow-test.py
"""
import json
import os
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = Path(os.environ.get("CLAUDE_WORKFLOW", ROOT / ".github" / "workflows" / "claude.yml"))
STEP = "Approve the runs GitHub held on the fixer's push"
REPO = "example-owner/cavalry"
HEAD = "bff0e77e3e3f31de105cc11c70e3abb65fca6eac"
BRANCH = "212-udp-rendezvous"
PR = "128"
CI, CLAUDE, DISPATCH = (".github/workflows/ci.yml", ".github/workflows/claude.yml",
                        ".github/workflows/dispatch-review.yml")


def step_script(name):
    """The `run: |` block of the step called `name`, with its indentation taken off."""
    lines = WORKFLOW.read_text(encoding="utf-8").split("\n")
    at = lines.index(f"      - name: {name}")
    run = next(i for i in range(at + 1, len(lines)) if lines[i].strip().startswith("run:"))
    if lines[run].strip() != "run: |":
        raise AssertionError(f"the step {name!r} has no `run: |` block")
    indent = len(lines[run]) - len(lines[run].lstrip()) + 2
    body = []
    for line in lines[run + 1:]:
        if line.strip() and len(line) - len(line.lstrip()) < indent:
            break
        body.append(line[indent:])
    return "\n".join(body).rstrip("\n") + "\n"


# The fake `gh`: one file, run by the step as `gh`. FAKE_GH names the scenario and
# FAKE_GH_LOG the log; the number of head_sha listings so far is the poll both listings
# answer for.
FAKE_GH = textwrap.dedent('''\
    #!/usr/bin/env python3
    import json, os, re, sys
    scenario = json.load(open(os.environ["FAKE_GH"]))
    log = os.environ["FAKE_GH_LOG"]
    argv = sys.argv[1:]
    def say(line):
        with open(log, "a") as f:
            f.write(line + "\\n")
    def polls():
        return sum(1 for l in open(log) if l.startswith("list ")) if os.path.exists(log) else 0
    def rows(listing, poll):
        return listing[min(poll, len(listing)) - 1] if listing else []
    if not argv or argv[0] != "api":
        sys.exit("fake gh: only `gh api` is served, got " + " ".join(argv))
    method, path, jq, i = "GET", None, "", 1
    while i < len(argv):
        if argv[i] == "-X":
            method, i = argv[i + 1], i + 2
        elif argv[i] == "--jq":
            jq, i = argv[i + 1], i + 2
        elif argv[i].startswith("-"):
            sys.exit("fake gh: an option the step is not expected to pass: " + argv[i])
        else:
            path, i = argv[i], i + 1
    repo, head = scenario["repo"], scenario["head"]
    approve = re.fullmatch("repos/" + re.escape(repo) + r"/actions/runs/(\\d+)/approve", path)
    if method == "POST" and approve:
        say("approve " + approve.group(1))
        if approve.group(1) in scenario.get("refuse", []):
            print("gh: Resource not accessible by personal access token (HTTP 403)", file=sys.stderr)
            sys.exit(1)
        sys.exit(0)
    if method != "GET":
        sys.exit("fake gh: an unexpected " + method + " " + path)
    if path == "repos/" + repo + "/pulls/" + scenario["pr"]:
        if ".mergeable" in jq:
            say("mergeable")
            print(scenario.get("mergeable", "true"))
        elif ".head.sha" in jq and ".head.ref" in jq:
            say("pull")
            if scenario.get("pull") is None:
                print("gh: Not Found (HTTP 404)", file=sys.stderr)
                sys.exit(1)
            print("\\t".join(scenario["pull"]))
        else:
            sys.exit("fake gh: an unexpected filter on the pull request: " + jq)
        sys.exit(0)
    row = '"\\\\(.id) \\\\(.path) \\\\(.status) \\\\(.conclusion // "")"'
    if path == "repos/" + repo + "/actions/runs?head_sha=" + head + "&per_page=50":
        if jq != ".workflow_runs[] | " + row:
            sys.exit("fake gh: the head_sha listing's filter is not the row the step reads: " + jq)
        say("list " + str(polls() + 1))
        print("\\n".join(rows(scenario.get("by_sha", []), polls())))
        sys.exit(0)
    if path == "repos/" + repo + "/actions/workflows/ci.yml/runs?per_page=100":
        want = ('.workflow_runs[] | select(.head_sha == "' + head + '" and (.head_repository.full_name // "") == "'
                + repo + '") | ' + row)
        if jq != want:
            sys.exit("fake gh: ci.yml's listing is not narrowed to the head and this repository: " + jq)
        say("list-ci " + str(polls()))
        print("\\n".join(rows(scenario.get("by_ci", []), polls())))
        sys.exit(0)
    sys.exit("fake gh: an unexpected call: " + method + " " + path)
''')


def row(id_, workflow, status, conclusion=""):
    return f"{id_} {workflow} {status} {conclusion}"


HELD = row(42078271138, CI, "completed", "action_required")
RUNNING = row(42078271138, CI, "queued")
OTHERS = [row(42079335746, CLAUDE, "completed", "success"), row(42079336391, DISPATCH, "completed", "success")]


class Step(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.script = step_script(STEP)

    def run_step(self, fix_landed="push", **scenario):
        scenario = {"repo": REPO, "head": HEAD, "pr": PR, "pull": [HEAD, BRANCH], **scenario}
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            gh = tmp / "bin" / "gh"
            gh.parent.mkdir()
            gh.write_text(FAKE_GH, encoding="utf-8")
            gh.chmod(gh.stat().st_mode | stat.S_IXUSR)
            (tmp / "scenario.json").write_text(json.dumps(scenario), encoding="utf-8")
            log = tmp / "calls.log"
            env = {"PATH": f"{gh.parent}{os.pathsep}{os.environ['PATH']}", "GITHUB_REPOSITORY": REPO,
                   "PR_NUMBER": PR, "FIX_LANDED": fix_landed, "GH_TOKEN": "not-a-token",
                   "FAKE_GH": str(tmp / "scenario.json"), "FAKE_GH_LOG": str(log)}
            # The step's `sleep 10` moves bash's own clock and returns at once.
            done = subprocess.run(["bash", "-c", "sleep() { SECONDS=$((SECONDS + $1)); }\n" + self.script],
                                  env=env, capture_output=True, text=True, timeout=60)
            calls = log.read_text(encoding="utf-8").split("\n")[:-1] if log.exists() else []
        return done.returncode, done.stdout + done.stderr, calls

    def approvals(self, calls):
        return [c.split()[1] for c in calls if c.startswith("approve ")]


class Listed(Step):
    def test_a_held_run_under_the_head_sha_is_approved_once_and_the_step_ends_after_two_quiet_polls(self):
        code, out, calls = self.run_step(by_sha=[OTHERS + [HELD], OTHERS + [RUNNING]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), ["42078271138"])
        self.assertIn(f"approved run 42078271138 on {HEAD}", out)
        self.assertIn("approved 1", out)
        self.assertEqual([c for c in calls if c.startswith("list ")], ["list 1", "list 2", "list 3"])
        self.assertFalse([c for c in calls if c.startswith("list-ci")], "ci.yml's listing is the second way only")

    def test_a_run_still_listed_as_held_after_its_approval_is_not_approved_again(self):
        code, out, calls = self.run_step(by_sha=[[HELD], [HELD], [RUNNING]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), ["42078271138"])

    def test_a_held_run_that_is_listed_late_is_approved(self):
        code, out, calls = self.run_step(by_sha=[OTHERS, OTHERS, OTHERS, OTHERS + [HELD], OTHERS + [RUNNING]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), ["42078271138"])

    def test_a_head_whose_ci_run_is_not_held_has_nothing_approved_and_ends_quietly(self):
        code, out, calls = self.run_step(fix_landed="", by_sha=[OTHERS + [RUNNING]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), [])
        self.assertIn("approved 0", out)
        self.assertEqual(len([c for c in calls if c.startswith("list ")]), 2)

    def test_every_held_run_of_the_head_is_approved_whatever_its_workflow(self):
        other = row(42078271200, CLAUDE, "completed", "action_required")
        code, out, calls = self.run_step(by_sha=[[HELD, other], [RUNNING]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), ["42078271138", "42078271200"])


class LeftOut(Step):
    """Card 215: the head_sha listing left the push's held ci run out for eleven minutes."""

    def test_a_held_run_the_head_sha_listing_leaves_out_is_found_in_ci_ymls_listing_and_approved(self):
        code, out, calls = self.run_step(by_sha=[OTHERS], by_ci=[[HELD], [RUNNING]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), ["42078271138"])
        self.assertIn("the head_sha listing leaves out ci's run", out)
        self.assertIn(f"approved run 42078271138 on {HEAD}", out)
        self.assertNotIn("::error::", out)

    def test_a_found_run_that_refuses_the_approval_fails_the_step_with_its_id(self):
        code, out, calls = self.run_step(by_sha=[OTHERS], by_ci=[[HELD]], refuse=["42078271138"])
        self.assertEqual(code, 1, out)
        self.assertEqual(self.approvals(calls), ["42078271138"])
        self.assertRegex(out, r"::error::run 42078271138, held on the fixer's push " + HEAD)
        self.assertIn(f"gh api -X POST repos/{REPO}/actions/runs/42078271138/approve", out)
        self.assertIn("docs/how-to/ask-the-agent.md", out)


class Nowhere(Step):
    def test_a_push_with_no_ci_run_in_either_listing_fails_the_step_after_its_two_minutes(self):
        code, out, calls = self.run_step(by_sha=[OTHERS], by_ci=[[]])
        self.assertEqual(code, 1, out)
        self.assertEqual(self.approvals(calls), [])
        self.assertIn(f"::error::the fixer pushed {HEAD} to #{PR} and no ci run on it is listed", out)
        self.assertIn(f"gh run list --workflow ci.yml --branch {BRANCH}", out)
        self.assertIn("docs/how-to/ask-the-agent.md", out)
        self.assertEqual(len([c for c in calls if c.startswith("list ")]), 13, "one look every 10 s for 120 s")
        self.assertEqual(len([c for c in calls if c.startswith("list-ci")]), 13)

    def test_a_push_to_a_conflicting_pull_request_has_no_run_to_find_and_the_step_goes_on(self):
        code, out, calls = self.run_step(by_sha=[OTHERS], by_ci=[[]], mergeable="false")
        self.assertEqual(code, 0, out)
        self.assertIn("::warning::", out)
        self.assertIn("conflicts with master", out)
        self.assertNotIn("::error::", out)

    def test_a_run_that_pushed_nothing_and_finds_no_ci_run_says_so_and_passes(self):
        for landed in ("", "body"):
            with self.subTest(landed=landed):
                code, out, calls = self.run_step(fix_landed=landed, by_sha=[OTHERS], by_ci=[[]])
                self.assertEqual(code, 0, out)
                self.assertIn("approved 0", out)
                self.assertNotIn("::error::", out)
                self.assertNotIn("mergeable", calls)

    def test_an_issue_is_not_a_pull_request_and_nothing_is_listed(self):
        code, out, calls = self.run_step(pull=None)
        self.assertEqual(code, 0, out)
        self.assertIn(f"#{PR} is not a pull request; nothing to approve", out)
        self.assertEqual(calls, ["pull"])


class Workflow(unittest.TestCase):
    def test_the_step_is_given_what_the_fix_landed_as_and_runs_after_a_run_that_ended_well(self):
        text = WORKFLOW.read_text(encoding="utf-8")
        block = text[text.index(f"      - name: {STEP}\n"):]
        block = block[:block.index("\n      - name: ", 1)]
        self.assertIn("FIX_LANDED: ${{ needs.agent.outputs.fix_landed }}", block)
        self.assertIn("if: needs.agent.result == 'success'", block)
        self.assertIn("GH_TOKEN: ${{ secrets.GH_ACTIONS_TOKEN }}", block)


if __name__ == "__main__":
    unittest.main()
