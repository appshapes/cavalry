#!/usr/bin/env python3
"""bench-compare: the latency gate CLAUDE.md names for invariant 2 (ADR 0015).

Reads the newest result `just bench` wrote under bench/results/, finds the baseline for
its profile in bench/baseline.json, and fails when any benchmark's p99.99 is more than
5 percent above the baseline's and more than one step of the harness's clock above it:
the result carries the step, `clock_step_ns`, which the harness measures once at start
(10 ns on the bench host, whose counter advances in tens), and a row whose percentiles
sit at 150 to 170 ns moves 6 percent in one step on an unchanged commit (ADR 0029). A
result without the field is judged by the 5 percent rule alone. Max is printed beside
the p99.99 but does not fail the gate: one
descheduling on a shared runner moves max, and the 5 percent rule on p99.99 is what
CLAUDE.md's gate table promises. A result made with `--repeat N` carries each
benchmark's median run by p99.99 as its p99.99 and lists every run's; the list is
printed as its lowest and highest, so the run-to-run band is beside the number the gate
reads. When the baseline has such a list, the gate compares the result's median with the
slowest run of the baseline: on the bench host one row's p99.99 moves by a fifth between
two processes of the same binary while the others hold within 2 percent, and a rule that
reads the baseline's band is the 5 percent rule on the rows the host can measure and
says so on the row it cannot, instead of failing an unchanged commit (ADR 0016). Only
the rows a message passes through, claim_commit_* and one_way_*, can fail the gate; the
timer and slot_padding_* rows hold no Cavalry code, they probe the host and the clock at
30 to 60 ns, where one 10 ns step of the clock is 15 to 33 percent, and they are printed
with their change marked "reported" (ADR 0017).

    bench-compare.py              compare the newest result; what `just bench-compare` and CI run
    bench-compare.py <result>     compare that result file instead
    bench-compare.py --update     write the newest result's percentiles into the baseline for its
                                  profile, which is how a baseline is set from a CI run's artifact

A profile that has no baseline yet is reported and passes: the baseline for a runner is
set from that runner's own first run (ADR 0015), and a laptop has none unless someone
records one. The exit code is 1 on a regression, a missing or unreadable result, or an
allocation count above zero per message in the result.

Python 3.9 standard library only (ADR 0003).
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RESULTS = ROOT / "bench" / "results"
BASELINE = ROOT / "bench" / "baseline.json"
TOLERANCE = 0.05
# The floor under the tolerance: a row within one step of the clock above the baseline's
# slowest run has not moved by what the harness can measure (ADR 0029).
STEP_FIELD = "clock_step_ns"
GATED = "p99_99_ns"
SHOWN = ("p50_ns", "p99_ns", "p99_99_ns", "max_ns")
# The rows a message passes through; every other row is a probe of the host (ADR 0017).
GATED_ROWS = ("claim_commit_", "one_way_")


def newest_result() -> Path:
    files = sorted(RESULTS.glob("*.json")) if RESULTS.is_dir() else []
    if not files:
        sys.exit(f"bench-compare: no result under {RESULTS.relative_to(ROOT)}; run `just bench` first")
    return files[-1]


def load(path: Path) -> dict:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as e:
        sys.exit(f"bench-compare: cannot read {path}: {e}")


def fmt(ns) -> str:
    return f"{ns:,.0f}" if isinstance(ns, (int, float)) else "-"


def compare(result: dict, baseline: dict) -> int:
    profile = result["profile"]
    step = clock_step(result)
    print(f"bench-compare: {profile}, {result.get('messages', '?')} messages, git {result.get('git', '?')}, "
          f"clock step {step} ns")
    failures = 0
    allocations = result.get("allocations_per_message")
    if allocations is None or allocations > 0:
        print(f"bench-compare: allocations per message is {allocations}, the gate requires 0 (CLAUDE.md, gate 2)")
        failures += 1
    base = baseline.get("profiles", {}).get(profile)
    rows = []
    for name, now in sorted(result["benchmarks"].items()):
        then = (base or {}).get("benchmarks", {}).get(name)
        runs = now.get("p99_99_ns_runs") or []
        spread = f"{fmt(min(runs))} to {fmt(max(runs))}" if len(runs) > 1 else "-"
        line = [name] + [fmt(now.get(k)) for k in SHOWN] + [spread]
        if then and then.get(GATED):
            slowest = max(then.get("p99_99_ns_runs") or [then[GATED]])
            ratio = now[GATED] / slowest - 1
            verdict = f"{ratio:+.1%}"
            if not name.startswith(GATED_ROWS):
                verdict += " reported"
            elif ratio > TOLERANCE and now[GATED] - slowest > step:
                verdict += " REGRESSION"
                failures += 1
            elif ratio > TOLERANCE:
                verdict += " within a step"
            line += [fmt(slowest), verdict]
        else:
            line += ["-", "no baseline"]
        rows.append(line)
    header = ["benchmark", "p50", "p99", "p99.99", "max", "p99.99 runs", "baseline slowest", "p99.99 change"]
    widths = [max(len(r[i]) for r in rows + [header]) for i in range(len(header))]
    for r in [header] + rows:
        print("  " + "  ".join(c.rjust(w) if i else c.ljust(w) for i, (c, w) in enumerate(zip(r, widths))))
    if base is None:
        print(f"bench-compare: no baseline for profile {profile!r} in {BASELINE.relative_to(ROOT)}; "
              "nothing compared. Set one from this machine's run with --update (ADR 0015).")
    if failures:
        print(f"bench-compare: {failures} finding(s); a message row's p99.99 more than {TOLERANCE:.0%} and more "
              f"than one clock step ({step} ns) above the baseline's slowest run fails (CLAUDE.md, gate 2; "
              "ADR 0029). Measure again on an idle machine before you believe either number.")
        return 1
    print("bench-compare: ok")
    return 0


def clock_step(result: dict) -> int:
    """The step of the clock the result was measured with, in whole nanoseconds, at least 1;
    a result from before the field is judged by the tolerance alone."""
    step = result.get(STEP_FIELD)
    return int(step) if isinstance(step, (int, float)) and step >= 1 else 1


def update(result: dict, baseline: dict) -> int:
    profile = result["profile"]
    entry = {
        "git": result.get("git"),
        "date": result.get("date"),
        "messages": result.get("messages"),
        "repeat": result.get("repeat"),
        STEP_FIELD: clock_step(result),
        "benchmarks": {
            name: {k: now[k] for k in SHOWN + ("p99_99_ns_runs",) if k in now}
            for name, now in sorted(result["benchmarks"].items())
        },
    }
    baseline.setdefault("profiles", {})[profile] = entry
    BASELINE.write_text(json.dumps(baseline, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    shown = BASELINE.relative_to(ROOT) if BASELINE.is_relative_to(ROOT) else BASELINE
    print(f"bench-compare: baseline for {profile!r} written from {result.get('git')}; commit "
          f"{shown} with the histograms in the message")
    return 0


def main(argv: list[str]) -> int:
    args = [a for a in argv[1:] if not a.startswith("--")]
    flags = {a for a in argv[1:] if a.startswith("--")}
    unknown = flags - {"--update"}
    if unknown or len(args) > 1:
        sys.exit("usage: bench-compare.py [--update] [result.json]")
    path = Path(args[0]) if args else newest_result()
    result = load(path)
    baseline = load(BASELINE) if BASELINE.exists() else {"schema": 0, "profiles": {}}
    if result.get("schema") != 0:
        sys.exit(f"bench-compare: {path} has schema {result.get('schema')!r}; this script knows 0")
    print(f"bench-compare: {path.relative_to(ROOT) if path.is_relative_to(ROOT) else path}")
    return update(result, baseline) if "--update" in flags else compare(result, baseline)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
