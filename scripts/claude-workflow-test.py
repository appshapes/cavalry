#!/usr/bin/env python3
"""Tests of the step of claude.yml that approves the run GitHub held on a fixer's push
(card 215): the step's own script, read out of the workflow file and run under bash with a
fake `gh` on PATH and a `sleep` that returns at once, so there is no network, no token
and no waiting; the step counts its looks and reads no clock. The fake `gh` answers each
call the step makes from a scenario and keeps a log of them; it evaluates no `--jq`
filter, so it refuses a filter that is not the one the step is known to pass, and a
listing's rows are given as that filter prints them, `<id> <workflow file> <status>
<conclusion>`. The choice of what is held and what is ci's, which the step makes in awk,
is what runs here. Standard library only; `just test-scripts` runs it.

    python3 scripts/claude-workflow-test.py
"""
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = Path(os.environ.get("CLAUDE_WORKFLOW", ROOT / ".github" / "workflows" / "claude.yml"))
STEP = "Approve the runs GitHub held on the fixer's push"
REPO = "example-owner/cavalry"
HEAD = "bff0e77e3e3f31de105cc11c70e3abb65fca6eac"
STALE = "5c1f0a2d9e3b47a6b8c0d1e2f3a4b5c6d7e8f901"   # what a lagging pull request object names
BRANCH = "212-udp-rendezvous"
PR = "128"
LOOKS = 13                                            # ten seconds apart: the step's two minutes
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
# FAKE_GH_LOG the log. The number of head_sha listings so far is the look that both
# listings answer for, and a listing shorter than the looks repeats its last answer.
FAKE_GH = r'''#!/usr/bin/env python3
import json, os, re, sys

scenario = json.load(open(os.environ["FAKE_GH"]))
log = os.environ["FAKE_GH_LOG"]
calls = open(log).read().split("\n")[:-1] if os.path.exists(log) else []
looks = sum(1 for call in calls if call.startswith("list "))


def say(line):
    with open(log, "a") as f:
        f.write(line + "\n")


def answer(answers, asked):
    return answers[min(asked, len(answers) - 1)] if answers else None


def refuse(message):
    print("gh: " + message, file=sys.stderr)
    sys.exit(1)


argv = sys.argv[1:]
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
repo, head, pull = scenario["repo"], scenario["head"], scenario.get("pull")
row = r'"\(.id) \(.path) \(.status) \(.conclusion // "")"'

approve = re.fullmatch("repos/" + re.escape(repo) + r"/actions/runs/(\d+)/approve", path)
if method == "POST" and approve:
    say("approve " + approve.group(1))
    if approve.group(1) in scenario.get("refuse", []):
        refuse("Resource not accessible by personal access token (HTTP 403)")
    sys.exit(0)
if method != "GET":
    sys.exit("fake gh: an unexpected " + method + " " + path)
if path == "repos/" + repo + "/pulls/" + scenario["pr"]:
    if jq == "[.head.sha, .head.ref, .head.repo.full_name] | @tsv":
        say("pull")
        if pull is None:
            refuse("Not Found (HTTP 404)")
        print("\t".join(pull))
    elif jq == ".mergeable | tostring":
        say("mergeable")
        said = answer(scenario.get("mergeable", ["true"]), calls.count("mergeable"))
        if said is None:
            refuse("Bad Gateway (HTTP 502)")
        print(said)
    else:
        sys.exit("fake gh: an unexpected filter on the pull request: " + jq)
    sys.exit(0)
if pull and path == "repos/" + repo + "/git/ref/heads/" + pull[1]:
    say("ref")
    if jq != ".object.sha":
        sys.exit("fake gh: an unexpected filter on the branch's ref: " + jq)
    if scenario.get("tip") is None:
        refuse("Not Found (HTTP 404)")
    print(scenario["tip"])
    sys.exit(0)
if path == "repos/" + repo + "/actions/runs?head_sha=" + head + "&per_page=50":
    if jq != ".workflow_runs[] | " + row:
        sys.exit("fake gh: the head_sha listing's filter is not the row the step reads: " + jq)
    say("list " + str(looks + 1))
    print("\n".join(answer(scenario.get("by_sha", []), looks) or []))
    sys.exit(0)
if path == "repos/" + repo + "/actions/workflows/ci.yml/runs?per_page=100":
    want = ('.workflow_runs[] | select(.head_sha == "' + head + '" and (.head_repository.full_name // "") == "'
            + repo + '") | ' + row)
    if jq != want:
        sys.exit("fake gh: ci.yml's listing is not narrowed to the head and this repository: " + jq)
    say("list-ci " + str(looks))
    print("\n".join(answer(scenario.get("by_ci", []), looks - 1) or []))
    sys.exit(0)
sys.exit("fake gh: an unexpected call, perhaps for a head the scenario does not expect: " + method + " " + path)
'''


def row(id_, workflow, status, conclusion=""):
    return f"{id_} {workflow} {status} {conclusion}"


HELD = row(42078271138, CI, "completed", "action_required")
RUNNING = row(42078271138, CI, "queued")
OTHERS = [row(42079335746, CLAUDE, "completed", "success"), row(42079336391, DISPATCH, "completed", "success")]


class Step(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.script = step_script(STEP)

    def run_step(self, **scenario):
        """Runs the step against a scenario: the exit code, what it printed, the fake's calls.
        `head` is the commit whose runs the step is expected to list; any other is refused."""
        scenario = {"repo": REPO, "head": HEAD, "pr": PR, "pull": [HEAD, BRANCH, REPO], "tip": HEAD, **scenario}
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            gh = tmp / "bin" / "gh"
            gh.parent.mkdir()
            gh.write_text(FAKE_GH, encoding="utf-8")
            gh.chmod(gh.stat().st_mode | stat.S_IXUSR)
            (tmp / "scenario.json").write_text(json.dumps(scenario), encoding="utf-8")
            log = tmp / "calls.log"
            env = {"PATH": f"{gh.parent}{os.pathsep}{os.environ['PATH']}", "GITHUB_REPOSITORY": REPO,
                   "PR_NUMBER": PR, "GH_TOKEN": "not-a-token",
                   "FAKE_GH": str(tmp / "scenario.json"), "FAKE_GH_LOG": str(log)}
            done = subprocess.run(["bash", "-c", "sleep() { :; }\n" + self.script],
                                  env=env, capture_output=True, text=True, timeout=120)
            calls = log.read_text(encoding="utf-8").split("\n")[:-1] if log.exists() else []
        return done.returncode, done.stdout + done.stderr, calls

    def approvals(self, calls):
        return [c.split()[1] for c in calls if c.startswith("approve ")]

    def looks(self, calls):
        return len([c for c in calls if c.startswith("list ")])


class Listed(Step):
    def test_a_held_run_under_the_head_sha_is_approved_once_and_the_step_ends_after_two_quiet_looks(self):
        code, out, calls = self.run_step(by_sha=[OTHERS + [HELD], OTHERS + [RUNNING]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), ["42078271138"])
        self.assertIn(f"approved run 42078271138 on {HEAD}", out)
        self.assertIn("approved 1", out)
        self.assertEqual(self.looks(calls), 3)
        self.assertFalse([c for c in calls if c.startswith("list-ci")], "ci.yml's listing is the second way only")
        self.assertNotIn("mergeable", calls)

    def test_a_run_still_listed_as_held_after_its_approval_is_not_approved_again(self):
        code, out, calls = self.run_step(by_sha=[[HELD], [HELD], [RUNNING]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), ["42078271138"])

    def test_a_held_run_that_is_listed_late_is_approved(self):
        code, out, calls = self.run_step(by_sha=[OTHERS, OTHERS, OTHERS, OTHERS + [HELD], OTHERS + [RUNNING]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), ["42078271138"])
        self.assertEqual(self.looks(calls), 6)

    def test_a_head_whose_ci_run_is_not_held_has_nothing_approved_and_ends_after_two_looks(self):
        code, out, calls = self.run_step(by_sha=[OTHERS + [RUNNING]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), [])
        self.assertIn("approved 0", out)
        self.assertEqual(self.looks(calls), 2)

    def test_every_held_run_of_the_head_is_approved_whatever_its_workflow(self):
        other = row(42078271200, CLAUDE, "completed", "action_required")
        code, out, calls = self.run_step(by_sha=[[HELD, other], [RUNNING]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), ["42078271138", "42078271200"])

    def test_a_workflow_file_named_with_its_ref_is_still_cis(self):
        held = row(42078271138, CI + "@refs/pull/128/merge", "completed", "action_required")
        code, out, calls = self.run_step(by_sha=[[held], [row(42078271138, CI + "@refs/pull/128/merge", "queued")]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), ["42078271138"])
        self.assertEqual(self.looks(calls), 3)


class LeftOut(Step):
    """Card 215: the head_sha listing left the push's held ci run out for eleven minutes."""

    def test_a_held_run_the_head_sha_listing_leaves_out_is_found_in_ci_ymls_listing_and_approved(self):
        code, out, calls = self.run_step(by_sha=[OTHERS], by_ci=[[HELD]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), ["42078271138"])
        self.assertEqual(out.count("the head_sha listing leaves out ci's run"), 1, out)
        self.assertIn(f"approved run 42078271138 on {HEAD}", out)
        self.assertNotIn("::error::", out)
        self.assertEqual([c for c in calls if c.startswith("list-ci")], ["list-ci 1"], "read once, then remembered")
        self.assertEqual(self.looks(calls), 3)

    def test_a_run_that_only_ci_ymls_listing_shows_later_is_approved_then(self):
        code, out, calls = self.run_step(by_sha=[OTHERS], by_ci=[[], [], [HELD]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), ["42078271138"])
        self.assertEqual(self.looks(calls), 5)

    def test_a_found_run_that_refuses_the_approval_fails_the_step_with_its_id(self):
        code, out, calls = self.run_step(by_sha=[OTHERS], by_ci=[[HELD]], refuse=["42078271138"])
        self.assertEqual(code, 1, out)
        self.assertEqual(self.approvals(calls), ["42078271138"])
        self.assertRegex(out, r"::error::run 42078271138, held on the fixer's push " + HEAD)
        self.assertIn(f"gh api -X POST repos/{REPO}/actions/runs/42078271138/approve", out)
        self.assertIn("docs/how-to/ask-the-agent.md", out)


class Lagging(Step):
    """The pull request object can name the head before the push (pull request 106)."""

    def test_the_run_approved_is_the_one_on_the_branchs_tip_when_the_object_lags(self):
        code, out, calls = self.run_step(pull=[STALE, BRANCH, REPO], tip=HEAD, by_sha=[[HELD], [RUNNING]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), ["42078271138"])
        self.assertIn(f"the pull request object names {STALE} and {BRANCH} is at {HEAD}", out)

    def test_a_branch_whose_ref_cannot_be_read_falls_back_to_the_objects_head(self):
        code, out, calls = self.run_step(tip=None, by_sha=[[HELD], [RUNNING]])
        self.assertEqual(code, 0, out)
        self.assertEqual(self.approvals(calls), ["42078271138"])
        self.assertIn("ref", calls)

    def test_a_forks_branch_is_not_read_as_a_ref_of_this_repository(self):
        code, out, calls = self.run_step(pull=[HEAD, BRANCH, "someone-else/cavalry"], tip=STALE,
                                         by_sha=[[RUNNING]])
        self.assertEqual(code, 0, out)
        self.assertNotIn("ref", calls)


class Nowhere(Step):
    def test_a_head_with_no_ci_run_in_either_listing_fails_the_step_after_its_two_minutes(self):
        code, out, calls = self.run_step(by_sha=[OTHERS], by_ci=[[]])
        self.assertEqual(code, 1, out)
        self.assertEqual(self.approvals(calls), [])
        self.assertIn(f"::error::{HEAD}, the head of #{PR}, has no ci run listed after two minutes", out)
        self.assertIn(f"gh run list --workflow ci.yml --branch {BRANCH}", out)
        self.assertIn("docs/how-to/ask-the-agent.md", out)
        self.assertEqual(self.looks(calls), LOOKS)
        self.assertEqual(len([c for c in calls if c.startswith("list-ci")]), LOOKS)
        self.assertEqual(calls.count("mergeable"), 1, "read once it says true")

    def test_a_head_of_a_conflicting_pull_request_has_no_run_to_find_and_the_step_goes_on_at_once(self):
        code, out, calls = self.run_step(by_sha=[OTHERS], by_ci=[[]], mergeable=["false"])
        self.assertEqual(code, 0, out)
        self.assertIn("::warning::", out)
        self.assertIn("conflicts with master", out)
        self.assertNotIn("::error::", out)
        self.assertEqual(self.looks(calls), 2)

    def test_a_conflict_github_reports_late_is_still_read(self):
        code, out, calls = self.run_step(by_sha=[OTHERS], by_ci=[[]], mergeable=["null", "null", "false"])
        self.assertEqual(code, 0, out)
        self.assertIn("conflicts with master", out)
        self.assertEqual(self.looks(calls), 4)

    def test_a_pull_request_whose_conflict_is_never_known_warns_and_goes_on(self):
        for answers in (["null"], [None]):
            with self.subTest(mergeable=answers):
                code, out, calls = self.run_step(by_sha=[OTHERS], by_ci=[[]], mergeable=answers)
                self.assertEqual(code, 0, out)
                self.assertIn("::warning::", out)
                self.assertIn("could not be read", out)
                self.assertNotIn("::error::", out)
                self.assertEqual(self.looks(calls), LOOKS)

    def test_an_issue_is_not_a_pull_request_and_nothing_is_listed(self):
        code, out, calls = self.run_step(pull=None)
        self.assertEqual(code, 0, out)
        self.assertIn(f"#{PR} is not a pull request; nothing to approve", out)
        self.assertEqual(calls, ["pull"])


class Workflow(unittest.TestCase):
    def test_the_step_runs_with_the_owners_token_after_an_agent_run_that_ended_well(self):
        text = WORKFLOW.read_text(encoding="utf-8")
        block = text[text.index(f"      - name: {STEP}\n"):]
        block = block[:block.index("\n      - name: ", 1)]
        self.assertIn("if: needs.agent.result == 'success'", block)
        self.assertIn("GH_TOKEN: ${{ secrets.GH_ACTIONS_TOKEN }}", block)
        self.assertIn("PR_NUMBER: ${{ github.event.pull_request.number || github.event.issue.number }}", block)


if __name__ == "__main__":
    unittest.main()
