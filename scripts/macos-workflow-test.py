#!/usr/bin/env python3
"""Tests of where the macOS gates run (ADR 0170; work item 479): `ci.yml`'s gates job
runs on one runner a run, Linux for every event of its own and macOS only when it is
called for it, and `macos.yml` is the caller, each Monday and by hand, with no steps
of its own. The pages that say when the week's run starts say what the schedule says.

The workflows are read as text, since Python's standard library reads no YAML, and
the expression that chooses the runner is worked out here for each event the way
GitHub works it out: `a && b || c`, with an input that no event set read as false.

Run by `just test` through `test-scripts`. Python 3.9 standard library (ADR 0003).
"""

import json
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CI = ROOT / ".github" / "workflows" / "ci.yml"
MACOS = ROOT / ".github" / "workflows" / "macos.yml"
HOW_TO = ROOT / "docs" / "how-to" / "read-the-weekly-run-of-the-macos-gates.md"
ADR = ROOT / "docs" / "decisions" / "0170-run-the-macos-gates-each-week-and-at-a-release-and-not-for-every-commit.md"

DAYS = ("Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday")


def code_of(path):
    """A workflow's lines that are not comments: a comment says what the job does not
    do, in the very words a test looks for."""
    return "\n".join(line for line in path.read_text(encoding="utf-8").splitlines()
                     if not line.lstrip().startswith("#")) + "\n"


def block(code, key):
    """A top-level key of a workflow with everything under it, as it stands."""
    found = re.search(rf"^{key}:.*\n(?:[ \t]+.*\n|\n)*", code, re.M)
    if found is None:
        raise AssertionError(f"the workflow has no top-level key `{key}`")
    return found.group(0).rstrip("\n") + "\n"


def runners(code, macos):
    """The runners the gates job's matrix gives, for a run with `inputs.macos` as given.
    The matrix is one expression, `fromJSON(inputs.macos && '<json>' || '<json>')`."""
    found = re.search(r"^      matrix: \$\{\{ fromJSON\(inputs\.macos && '([^']+)' \|\| '([^']+)'\) \}\}$", code, re.M)
    if found is None:
        raise AssertionError("ci.yml's gates job chooses its matrix by `inputs.macos` alone, in the form "
                             "fromJSON(inputs.macos && '<json>' || '<json>'): this test reads that form (ADR 0170)")
    return [(row["name"], row["runner"]) for row in json.loads(found.group(1 if macos else 2))["include"]]


class Ci(unittest.TestCase):
    def setUp(self):
        self.code = code_of(CI)

    def test_no_event_of_its_own_runs_the_macos_job(self):
        """A push to `master`, a pull request and a run by hand set no `inputs.macos`,
        so each runs `gates (linux)` and nothing else."""
        self.assertEqual(runners(self.code, macos=False), [("linux", "blacksmith-4vcpu-ubuntu-2404")])

    def test_a_call_for_macos_runs_the_macos_job_alone(self):
        self.assertEqual(runners(self.code, macos=True), [("macos", "blacksmith-6vcpu-macos-latest")])

    def test_its_events_are_a_push_to_master_a_pull_request_a_run_by_hand_and_a_call(self):
        """No schedule: a scheduled run of `ci` at `master`'s head would be listed as a
        run of `ci` for the commit, and would share the group of `master`'s runs."""
        on = block(self.code, "on")
        self.assertEqual(re.findall(r"^  (\w+):", on, re.M), ["push", "pull_request", "workflow_dispatch", "workflow_call"])
        self.assertIn("  push:\n    branches: [master]\n", on)
        self.assertIn("      macos:\n", on)
        self.assertIn("        default: false\n        type: boolean\n", on)

    def test_the_job_keeps_the_names_branch_protection_and_the_pages_use(self):
        self.assertIn("    name: gates (${{ matrix.name }})\n", self.code)

    def test_a_called_run_is_in_a_group_of_its_own(self):
        """`github` in a called workflow is the caller's, so the ref alone would put a
        week's run at `master`'s head in the group of `master`'s own runs."""
        self.assertIn("concurrency:\n  group: ${{ inputs.macos && 'macos' || 'ci' }}-${{ github.ref }}\n", self.code)


class Macos(unittest.TestCase):
    def setUp(self):
        self.code = code_of(MACOS)

    def schedule(self):
        found = re.findall(r"^    - cron: '(\d+) (\d+) \* \* (\d)'$", block(self.code, "on"), re.M)
        self.assertEqual(len(found), 1, "macos.yml has one schedule, at one minute of one hour of one day of the week")
        minute, hour, day = (int(part) for part in found[0])
        return f"{hour:02d}:{minute:02d} UTC each {DAYS[day % 7]}"

    def test_it_runs_once_a_week_and_by_hand_and_for_nothing_else(self):
        self.assertEqual(re.findall(r"^  (\w+):", block(self.code, "on"), re.M), ["schedule", "workflow_dispatch"])
        self.schedule()

    def test_its_one_job_is_cis_gates_job_called_for_macos(self):
        """No steps of its own, so the two operating systems cannot come to run
        different gates."""
        self.assertEqual(block(self.code, "jobs"),
                         "jobs:\n  macos:\n    name: macos\n    uses: ./.github/workflows/ci.yml\n    with:\n      macos: true\n")
        self.assertNotIn("steps:", self.code)

    def test_it_holds_a_token_that_reads_and_no_secret(self):
        self.assertEqual(block(self.code, "permissions"), "permissions:\n  contents: read\n")
        self.assertNotIn("secrets", self.code)

    def test_it_has_no_group_of_its_own_to_wait_on_the_one_ci_gives_its_call(self):
        self.assertNotIn("concurrency", self.code)

    def test_the_pages_give_the_time_the_schedule_gives(self):
        when = self.schedule()
        for path in (HOW_TO, ADR, MACOS):
            text = " ".join(path.read_text(encoding="utf-8").replace("#", " ").split())
            self.assertIn(when, text, f"{path.name} says when the week's run starts, and the schedule says {when}")


if __name__ == "__main__":
    unittest.main()
