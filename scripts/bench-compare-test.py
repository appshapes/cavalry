#!/usr/bin/env python3
"""Tests of bench-compare.py's rule on made-up results and baselines: a gated row fails only
beyond 5 percent and beyond one step of the harness's clock (ADR 0029); the probe rows are
reported; `--update` keeps the step. Standard library only; `just test-scripts` runs it.

    python3 scripts/bench-compare-test.py
"""
import contextlib
import importlib.util
import io
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("bench_compare", HERE / "bench-compare.py")
bench_compare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench_compare)


def row(p99_99, runs=None):
    return {"p50_ns": 100, "p99_ns": 150, "p99_99_ns": p99_99, "max_ns": 1000,
            "p99_99_ns_runs": runs or [p99_99]}


def result(step, **rows):
    return {"schema": 0, "profile": "test-host", "git": "abc1234", "messages": 1000,
            "repeat": 3, "allocations_per_message": 0, "clock_step_ns": step,
            "benchmarks": rows}


def baseline(**rows):
    return {"schema": 0, "profiles": {"test-host": {"git": "0000000", "benchmarks": rows}}}


def run(res, base):
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        code = bench_compare.compare(res, base)
    return code, out.getvalue()


class Rule(unittest.TestCase):
    def test_one_step_up_on_a_short_row_passes_though_over_five_percent(self):
        # 170 against 160 is +6.25%, one 10 ns step: not a move the clock can measure.
        code, out = run(result(10, claim_commit_32_client=row(170)),
                        baseline(claim_commit_32_client=row(160)))
        self.assertEqual(code, 0, out)
        self.assertIn("+6.2% within a step", out)
        self.assertIn("bench-compare: ok", out)

    def test_two_steps_up_on_a_short_row_fails(self):
        code, out = run(result(10, claim_commit_32_client=row(180)),
                        baseline(claim_commit_32_client=row(160)))
        self.assertEqual(code, 1, out)
        self.assertIn("+12.5% REGRESSION", out)

    def test_a_fine_clock_keeps_the_five_percent_rule(self):
        # With a 1 ns step, 170 against 160 is a measured 6.3 percent.
        code, out = run(result(1, claim_commit_32_client=row(170)),
                        baseline(claim_commit_32_client=row(160)))
        self.assertEqual(code, 1, out)
        self.assertIn("REGRESSION", out)

    def test_twenty_percent_up_fails_on_any_clock(self):
        code, out = run(result(10, one_way_32_client=row(360)),
                        baseline(one_way_32_client=row(300)))
        self.assertEqual(code, 1, out)

    def test_within_five_percent_passes_without_the_step(self):
        code, out = run(result(10, one_way_32_client=row(310)),
                        baseline(one_way_32_client=row(300)))
        self.assertEqual(code, 0, out)
        self.assertNotIn("within a step", out)

    def test_the_baseline_slowest_run_is_the_number_compared(self):
        code, out = run(result(10, one_way_32_client=row(330)),
                        baseline(one_way_32_client=row(300, runs=[290, 300, 320])))
        self.assertEqual(code, 0, out)  # 330 against 320 is +3.1%

    def test_a_probe_row_is_reported_never_failed(self):
        code, out = run(result(10, timer=row(40)), baseline(timer=row(20)))
        self.assertEqual(code, 0, out)
        self.assertIn("+100.0% reported", out)

    def test_a_result_without_the_step_is_judged_by_the_tolerance_alone(self):
        res = result(10, claim_commit_32_client=row(170))
        del res["clock_step_ns"]
        code, out = run(res, baseline(claim_commit_32_client=row(160)))
        self.assertEqual(code, 1, out)
        self.assertIn("clock step unknown", out)

    def test_allocations_fail_the_gate(self):
        res = result(10, one_way_32_client=row(300))
        res["allocations_per_message"] = 0.5
        code, out = run(res, baseline(one_way_32_client=row(300)))
        self.assertEqual(code, 1, out)


class Update(unittest.TestCase):
    def test_update_keeps_the_step_and_the_runs(self):
        bench_compare.BASELINE = Path(__import__("tempfile").mkdtemp()) / "baseline.json"
        base = {"schema": 0, "profiles": {}}
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            bench_compare.update(result(10, one_way_32_client=row(300, runs=[290, 300, 310])), base)
        entry = base["profiles"]["test-host"]
        self.assertEqual(entry["clock_step_ns"], 10)
        self.assertEqual(entry["benchmarks"]["one_way_32_client"]["p99_99_ns_runs"], [290, 300, 310])
        self.assertTrue(bench_compare.BASELINE.exists())

    def test_update_leaves_the_step_out_when_the_result_has_none(self):
        bench_compare.BASELINE = Path(__import__("tempfile").mkdtemp()) / "baseline.json"
        base = {"schema": 0, "profiles": {}}
        res = result(10, one_way_32_client=row(300))
        del res["clock_step_ns"]
        with contextlib.redirect_stdout(io.StringIO()):
            bench_compare.update(res, base)
        self.assertNotIn("clock_step_ns", base["profiles"]["test-host"])


if __name__ == "__main__":
    unittest.main()
