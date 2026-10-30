#!/usr/bin/env python3
"""Tests of bench-compare.py's rule on made-up results and baselines: a gated row fails only
beyond 5 percent and beyond one step of the harness's clock, the smaller of the result's
step and the baseline's (ADR 0029); the probe rows are reported; `--update` writes the step
into the baseline and the next comparison reads it back; a baseline from three processes
holds every process's runs and the gate compares with the slowest over all of them (ADR
0033). Standard library only; `just test-scripts` runs it.

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
            "messages": 1000, "repeat": 3, "allocations_per_message": 0, "allocations": 0,
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

    def test_an_only_result_with_no_message_row_passes_on_a_count_of_zero(self):
        """`--only timer` writes a `null` ratio and the count; with the count 0 the run
        passes, and its `-only` profile has no baseline (card 161)."""
        res = result(10, timer=row(20))
        res["profile"] = "test-host-only"
        res["allocations_per_message"] = None
        code, out = run(res, baseline(step=10, one_way_32_client=row(300)))
        self.assertEqual(code, 0, out)
        self.assertIn("no baseline", out)

    def test_a_null_ratio_with_allocations_counted_fails(self):
        res = result(10, timer=row(20))
        res["profile"] = "test-host-only"
        res["allocations_per_message"] = None
        res["allocations"] = 3
        code, out = run(res, baseline(step=10, one_way_32_client=row(300)))
        self.assertEqual(code, 1, out)
        self.assertIn("3 allocations", out)

    def test_a_result_with_neither_allocation_field_fails(self):
        res = result(10, one_way_32_client=row(300))
        del res["allocations_per_message"]
        del res["allocations"]
        code, out = run(res, baseline(step=10, one_way_32_client=row(300)))
        self.assertEqual(code, 1, out)
        self.assertIn("neither", out)


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


class Newest(unittest.TestCase):
    """`newest_result` picks the newest run by the time in the file's name, so a `-only`
    result does not shadow every later full run of its profile (card 161)."""

    def test_the_newest_result_is_by_time_not_by_name(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        results = Path(directory.name)
        for name in ("p-1000.json", "p-only-1500.json", "p-2000.json", "p-unchecked-1900.json"):
            (results / name).write_text("{}", encoding="utf-8")
        with mock.patch.object(bench_compare, "RESULTS", results):
            self.assertEqual(bench_compare.newest_result().name, "p-2000.json")


class Update(unittest.TestCase):
    """`--update` against a baseline file in a directory of its own, removed after each
    test, with the module's path restored."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        patched = mock.patch.object(bench_compare, "BASELINE", Path(directory.name) / "baseline.json")
        patched.start()
        self.addCleanup(patched.stop)

    def update(self, *results):
        """Run `--update` on an empty baseline, one result per process, and return what it
        wrote to the file."""
        with contextlib.redirect_stdout(io.StringIO()):
            bench_compare.update(list(results), {"schema": 0, "profiles": {}})
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

    def test_one_result_is_one_process_and_the_comparison_does_not_say_so(self):
        written = self.update(result(10, one_way_32_client=row(300)))
        self.assertEqual(written["profiles"]["test-host"]["processes"], 1)
        code, out = run(result(10, one_way_32_client=row(300)), written)
        self.assertEqual(code, 0, out)
        self.assertNotIn("processes", out)


class Processes(Update):
    """A bench host's baseline is recorded from three processes, and the slowest run the
    gate compares with is the slowest over all of them (ADR 0033). The numbers are kip's
    exclusive claim row on master at 4a09b6e, three runs of `bench (kip)`."""

    def processes(self):
        a = result(1, claim_commit_32_exclusive=row(124, runs=[122, 124, 154]))
        b = result(1, claim_commit_32_exclusive=row(106, runs=[102, 106, 160]))
        c = result(1, claim_commit_32_exclusive=row(102, runs=[96, 102, 123]))
        for i, r in enumerate((a, b, c)):
            r["date_unix"] += i * 3600
        return a, b, c

    def test_three_processes_write_every_run_and_the_slowest_process_s_percentiles(self):
        written = self.update(*self.processes())
        entry = written["profiles"]["test-host"]
        self.assertEqual(entry["processes"], 3)
        self.assertEqual(entry["git"], "abc1234")
        self.assertEqual(entry["date"], "2026-10-29T01:24:16Z")  # the newest process's
        exclusive = entry["benchmarks"]["claim_commit_32_exclusive"]
        self.assertEqual(exclusive["p99_99_ns_runs"], [122, 124, 154, 102, 106, 160, 96, 102, 123])
        self.assertEqual(exclusive["p99_99_ns"], 124)

    def test_the_gate_compares_with_the_slowest_run_over_every_process(self):
        written = self.update(*self.processes())
        # 130 is over any one process's median and over the third process's slowest run,
        # 123, and within the slowest run the three measured, 160.
        code, out = run(result(1, claim_commit_32_exclusive=row(130, runs=[119, 130, 152])), written)
        self.assertEqual(code, 0, out)
        self.assertIn("baseline from 3 processes", out)
        self.assertIn("-18.8%", out)
        code, out = run(result(1, claim_commit_32_exclusive=row(170)), written)
        self.assertEqual(code, 1, out)
        self.assertIn("+6.2% REGRESSION", out)

    def test_the_second_process_is_sorted_by_date_not_by_the_order_given(self):
        a, b, c = self.processes()
        written = self.update(c, a, b)
        runs = written["profiles"]["test-host"]["benchmarks"]["claim_commit_32_exclusive"]["p99_99_ns_runs"]
        self.assertEqual(runs, [122, 124, 154, 102, 106, 160, 96, 102, 123])

    def test_processes_at_two_commits_list_both(self):
        a, b, c = self.processes()
        c["git"] = "def5678"
        written = self.update(a, b, c)
        self.assertEqual(written["profiles"]["test-host"]["git"], ["abc1234", "def5678"])

    def test_two_profiles_are_refused(self):
        a, b, c = self.processes()
        b["profile"] = "other-host"
        with self.assertRaises(SystemExit) as refused:
            self.update(a, b, c)
        self.assertIn("one profile", str(refused.exception))

    def test_two_harness_settings_are_refused(self):
        for field, value in (("clock_step_ns", 10), ("repeat", 21), ("messages", 5000000)):
            a, b, c = self.processes()
            c[field] = value
            with self.assertRaises(SystemExit) as refused:
                self.update(a, b, c)
            self.assertIn(f"one {field}", str(refused.exception))

    def test_different_rows_are_refused(self):
        a, b, c = self.processes()
        c["benchmarks"]["timer"] = row(20)
        with self.assertRaises(SystemExit) as refused:
            self.update(a, b, c)
        self.assertIn("same rows", str(refused.exception))

    def test_two_or_four_processes_are_refused(self):
        """Three is fixed, not a minimum (ADR 0033): a fourth process loosens the slowest
        run by collection, and two is a baseline half recorded. Nothing is written."""
        a, b, c = self.processes()
        d = self.processes()[0]
        d["date_unix"] += 4 * 3600
        for given in ((a, b), (a, b, c, d)):
            with self.assertRaises(SystemExit) as refused:
                self.update(*given)
            self.assertIn(f"these are {len(given)}", str(refused.exception))
            self.assertIn("Three is fixed, not a minimum", str(refused.exception))
            self.assertFalse(bench_compare.BASELINE.exists())

    def test_main_updates_from_several_files_and_compares_one(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        paths = []
        for i, r in enumerate(self.processes()):
            path = Path(directory.name) / f"test-host-{i}.json"
            path.write_text(json.dumps(r), encoding="utf-8")
            paths.append(str(path))
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(bench_compare.main(["bench-compare.py", "--update", *paths]), 0)
        written = json.loads(bench_compare.BASELINE.read_text(encoding="utf-8"))
        self.assertEqual(written["profiles"]["test-host"]["processes"], 3)
        with self.assertRaises(SystemExit) as refused:
            bench_compare.main(["bench-compare.py", *paths])
        self.assertIn("usage", str(refused.exception))


if __name__ == "__main__":
    unittest.main()
