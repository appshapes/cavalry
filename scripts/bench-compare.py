#!/usr/bin/env python3
"""bench-compare: the latency gate CLAUDE.md names for invariant 2 (ADR 0015).

Reads the newest result `just bench` wrote under bench/results/, finds the baseline for
its profile in bench/baseline.json, and fails when any benchmark's p99.99 is more than
5 percent above the baseline's. Max is printed beside it but does not fail the gate: one
descheduling on a shared runner moves max, and the 5 percent rule on p99.99 is what
CLAUDE.md's gate table promises.

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
GATED = "p99_99_ns"
SHOWN = ("p50_ns", "p99_ns", "p99_99_ns", "max_ns")


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
    print(f"bench-compare: {profile}, {result.get('messages', '?')} messages, git {result.get('git', '?')}")
    failures = 0
    allocations = result.get("allocations_per_message")
    if allocations is None or allocations > 0:
        print(f"bench-compare: allocations per message is {allocations}, the gate requires 0 (CLAUDE.md, gate 2)")
        failures += 1
    base = baseline.get("profiles", {}).get(profile)
    rows = []
    for name, now in sorted(result["benchmarks"].items()):
        then = (base or {}).get("benchmarks", {}).get(name)
        line = [name] + [fmt(now.get(k)) for k in SHOWN]
        if then and then.get(GATED):
            ratio = now[GATED] / then[GATED] - 1
            verdict = f"{ratio:+.1%}"
            if ratio > TOLERANCE:
                verdict += " REGRESSION"
                failures += 1
            line += [fmt(then[GATED]), verdict]
        else:
            line += ["-", "no baseline"]
        rows.append(line)
    header = ["benchmark", "p50", "p99", "p99.99", "max", "baseline p99.99", "p99.99 change"]
    widths = [max(len(r[i]) for r in rows + [header]) for i in range(len(header))]
    for r in [header] + rows:
        print("  " + "  ".join(c.rjust(w) if i else c.ljust(w) for i, (c, w) in enumerate(zip(r, widths))))
    if base is None:
        print(f"bench-compare: no baseline for profile {profile!r} in {BASELINE.relative_to(ROOT)}; "
              "nothing compared. Set one from this machine's run with --update (ADR 0015).")
    if failures:
        print(f"bench-compare: {failures} finding(s); a p99.99 more than {TOLERANCE:.0%} above the baseline "
              "fails (CLAUDE.md, gate 2). Measure again on an idle machine before you believe either number.")
        return 1
    print("bench-compare: ok")
    return 0


def update(result: dict, baseline: dict) -> int:
    profile = result["profile"]
    entry = {
        "git": result.get("git"),
        "date": result.get("date"),
        "messages": result.get("messages"),
        "benchmarks": {
            name: {k: now[k] for k in SHOWN if k in now} for name, now in sorted(result["benchmarks"].items())
        },
    }
    baseline.setdefault("profiles", {})[profile] = entry
    BASELINE.write_text(json.dumps(baseline, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"bench-compare: baseline for {profile!r} written from {result.get('git')}; commit "
          f"{BASELINE.relative_to(ROOT)} with the histograms in the message")
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
