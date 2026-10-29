#!/usr/bin/env python3
"""Tests of bench-compare.py's rule on made-up results and baselines: a gated row fails only
beyond 5 percent and beyond one step of the harness's clock, the smaller of the result's
step and the baseline's (ADR 0029); the probe rows are reported; `--update` writes the step
into the baseline and the next comparison reads it back. Standard library only;
`just test-scripts` runs it.

    python3 scripts/bench-compare-test.py
"""
import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("bench_compare", HERE / "bench-compare.py")
bench_compare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench_compare)


def row(p99_99, runs=None):
    return {"p50_ns": 100, "p99_ns": 150, "p99_99_ns": p99_99, "max_ns": 1000,
            "p99_99_ns_runs": runs or [p99_99]}


def result(step, **rows):
    return {"schema": 0, "profile": "test-host", "git": "abc1234", "date_unix": 1793229856,
            "messages": 1000, "repeat": 3, "allocations_per_message": 0,
            "clock_step_ns": step, "benchmarks": rows}


def baseline(step=None, **rows):
    """A baseline for the profile, with the step its recording measured when one is given;
    a baseline from before the field has none."""
    entry = {"git": "0000000", "benchmarks": rows}
    if step is not None:
        entry["clock_step_ns"] = step
    return {"schema": 0, "profiles": {"test-host": entry}}


def run(res, base):
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        code = bench_compare.compare(res, base)
    return code, out.getvalue()


class Rule(unittest.TestCase):
    def test_one_step_up_on_a_short_row_passes_though_over_five_percent(self):
        # 170 against 160 is +6.25%, one 10 ns step: not a move the clock can measure.
        code, out = run(result(10, claim_commit_32_client=row(170)),
                        baseline(step=10, claim_commit_32_client=row(160)))
        self.assertEqual(code, 0, out)
        self.assertIn("+6.2% within a step", out)
        self.assertIn("bench-compare: ok", out)

    def test_two_steps_up_on_a_short_row_fails(self):
        code, out = run(result(10, claim_commit_32_client=row(180)),
                        baseline(step=10, claim_commit_32_client=row(160)))
        self.assertEqual(code, 1, out)
        self.assertIn("+12.5% REGRESSION", out)
        self.assertIn("more than one clock step (10 ns) above the baseline's slowest run", out)

    def test_a_fine_clock_keeps_the_five_percent_rule(self):
        # With a 1 ns step, 170 against 160 is a measured 6.3 percent.
        code, out = run(result(1, claim_commit_32_client=row(170)),
                        baseline(step=1, claim_commit_32_client=row(160)))
        self.assertEqual(code, 1, out)
        self.assertIn("REGRESSION", out)

    def test_twenty_percent_up_fails_on_any_clock(self):
        code, out = run(result(10, one_way_32_client=row(360)),
                        baseline(step=10, one_way_32_client=row(300)))
        self.assertEqual(code, 1, out)

    def test_within_five_percent_passes_without_the_step(self):
        code, out = run(result(10, one_way_32_client=row(310)),
                        baseline(step=10, one_way_32_client=row(300)))
        self.assertEqual(code, 0, out)
        self.assertNotIn("within a step", out)

    def test_the_baseline_slowest_run_is_the_number_compared(self):
        code, out = run(result(10, one_way_32_client=row(330)),
                        baseline(step=10, one_way_32_client=row(300, runs=[290, 300, 320])))
        self.assertEqual(code, 0, out)  # 330 against 320 is +3.1%

    def test_a_probe_row_is_reported_never_failed(self):
        code, out = run(result(10, timer=row(40)), baseline(step=10, timer=row(20)))
        self.assertEqual(code, 0, out)
        self.assertIn("+100.0% reported", out)

    def test_a_result_without_the_step_is_judged_by_the_tolerance_alone(self):
        res = result(10, claim_commit_32_client=row(170))
        del res["clock_step_ns"]
        code, out = run(res, baseline(step=10, claim_commit_32_client=row(160)))
        self.assertEqual(code, 1, out)
        self.assertIn("clock step unknown, the 5 percent rule alone", out)
        self.assertIn("clock step unknown, so the rule has no floor", out)
        self.assertNotIn("(0 ns)", out)

    def test_allocations_fail_the_gate(self):
        res = result(10, one_way_32_client=row(300))
        res["allocations_per_message"] = 0.5
        code, out = run(res, baseline(step=10, one_way_32_client=row(300)))
        self.assertEqual(code, 1, out)


class Floor(unittest.TestCase):
    """The floor is the smaller of the result's step and the baseline's (ADR 0029): a
    change to the harness that over-read its step cannot widen the gate by itself."""

    def test_equal_steps_are_said_once(self):
        code, out = run(result(10, claim_commit_32_client=row(170)),
                        baseline(step=10, claim_commit_32_client=row(160)))
        self.assertEqual(code, 0, out)
        self.assertIn("clock step 10 ns\n", out)

    def test_a_result_that_over_reads_its_step_is_held_to_the_baselines(self):
        # 175 against 160 is +9.4%, 15 ns: within the result's 20 ns step, beyond the
        # baseline's 10 ns.
        code, out = run(result(20, claim_commit_32_client=row(175)),
                        baseline(step=10, claim_commit_32_client=row(160)))
        self.assertEqual(code, 1, out)
        self.assertIn("clock step 10 ns, the smaller of the result's 20 and the baseline's 10", out)
        self.assertIn("+9.4% REGRESSION", out)
        self.assertIn("more than one clock step (10 ns)", out)

    def test_a_baseline_with_the_coarser_step_does_not_widen_the_gate_either(self):
        code, out = run(result(10, claim_commit_32_client=row(175)),
                        baseline(step=20, claim_commit_32_client=row(160)))
        self.assertEqual(code, 1, out)
        self.assertIn("clock step 10 ns, the smaller of the result's 10 and the baseline's 20", out)

    def test_a_baseline_without_the_step_takes_the_results(self):
        code, out = run(result(10, claim_commit_32_client=row(170)),
                        baseline(claim_commit_32_client=row(160)))
        self.assertEqual(code, 0, out)
        self.assertIn("clock step 10 ns, none in the baseline", out)
        self.assertIn("within a step", out)

    def test_a_result_without_the_step_takes_no_floor_from_the_baseline(self):
        res = result(10, claim_commit_32_client=row(170))
        del res["clock_step_ns"]
        code, out = run(res, baseline(step=10, claim_commit_32_client=row(160)))
        self.assertEqual(code, 1, out)
        self.assertIn("clock step unknown", out)


class Update(unittest.TestCase):
    """`--update` against a baseline file in a directory of its own, removed after each
    test, with the module's path restored."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        patched = mock.patch.object(bench_compare, "BASELINE", Path(directory.name) / "baseline.json")
        patched.start()
        self.addCleanup(patched.stop)

    def update(self, res):
        """Run `--update` on an empty baseline and return what it wrote to the file."""
        with contextlib.redirect_stdout(io.StringIO()):
            bench_compare.update(res, {"schema": 0, "profiles": {}})
        return json.loads(bench_compare.BASELINE.read_text(encoding="utf-8"))

    def test_update_writes_the_step_and_the_runs(self):
        written = self.update(result(10, one_way_32_client=row(300, runs=[290, 300, 310])))
        entry = written["profiles"]["test-host"]
        self.assertEqual(entry["clock_step_ns"], 10)
        self.assertEqual(entry["benchmarks"]["one_way_32_client"]["p99_99_ns_runs"], [290, 300, 310])

    def test_update_leaves_the_step_out_when_the_result_has_none(self):
        res = result(10, one_way_32_client=row(300))
        del res["clock_step_ns"]
        written = self.update(res)
        self.assertNotIn("clock_step_ns", written["profiles"]["test-host"])

    def test_update_writes_the_date_from_the_results_date_unix(self):
        """The harness writes `date_unix`; the baseline says when it was measured (card 158)."""
        written = self.update(result(10, one_way_32_client=row(300)))
        self.assertEqual(written["profiles"]["test-host"]["date"], "2026-10-28T23:24:16Z")

    def test_update_writes_no_date_when_the_result_has_no_date_unix(self):
        res = result(10, one_way_32_client=row(300))
        del res["date_unix"]
        written = self.update(res)
        self.assertIsNone(written["profiles"]["test-host"]["date"])

    def test_the_step_update_wrote_is_the_floor_of_the_next_comparison(self):
        written = self.update(result(10, claim_commit_32_client=row(160)))
        code, out = run(result(20, claim_commit_32_client=row(175)), written)
        self.assertEqual(code, 1, out)
        self.assertIn("the smaller of the result's 20 and the baseline's 10", out)


if __name__ == "__main__":
    unittest.main()
