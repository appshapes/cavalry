#!/usr/bin/env python3
"""bench-compare: the latency gate CLAUDE.md names for invariant 2 (ADR 0015).

Reads the newest result `just bench` wrote under bench/results/, finds the baseline for
its profile in bench/baseline.json, and fails when any benchmark's p99.99 is more than
5 percent above the baseline's and more than one step of the harness's clock above it:
the result carries the step, `clock_step_ns`, which the harness measures once at start
(10 ns on the bench host, whose counter advances in tens), and a row whose percentiles
sit at 150 to 170 ns moves 6 percent in one step on an unchanged commit (ADR 0029). The
baseline's entry carries the step its recording measured, once `--update` has written
one, and the floor is the smaller of the two: a change to the harness that over-read its
step would otherwise widen the gate by itself. A result without the field is judged by
the 5 percent rule alone. Max is printed beside the p99.99 but does not fail the gate: one
descheduling on a shared runner moves max, and the 5 percent rule on p99.99 is what
CLAUDE.md's gate table promises. A result made with `--repeat N` carries each
benchmark's median run by p99.99 as its p99.99 and lists every run's; the list is
printed as its lowest and highest, so the run-to-run band is beside the number the gate
reads. When the baseline has such a list, the gate compares the result's median with the
slowest run of the baseline: on the bench host one row's p99.99 moves by a fifth between
two processes of the same binary while the others hold within 2 percent, and a rule that
reads the baseline's band is the 5 percent rule on the rows the host can measure and
says so on the row it cannot, instead of failing an unchanged commit (ADR 0016). A
bench host's baseline is recorded from every process the host has run at the baseline's
tree, three at least, and the slowest run is the slowest over all of them; a later
process of the same tree that reads a row above it is the host's own band and joins the
baseline with `--join`: on kip a row's median moves by a tenth, and by half on the
exclusive claim row, between two processes of one tree, and a baseline from one process
at the bottom of that band failed master's own next run with no change behind it (ADR
0033). Only
the rows a message passes through, claim_commit_* and one_way_*, can fail the gate; the
timer and slot_padding_* rows hold no Cavalry code, they probe the host and the clock at
30 to 60 ns, where one 10 ns step of the clock is 15 to 33 percent, and they are printed
with their change marked "reported" (ADR 0017).

    bench-compare.py                        compare the newest result; what `just bench-compare` and CI run
    bench-compare.py <result>               compare that result file instead
    bench-compare.py --update [<result>...] write the baseline for the results' profile: from the newest
                                            result alone, or from each result file given, one process
                                            each, which is how a bench host's baseline is set from the
                                            artifacts of its CI runs at one tree (ADR 0033)
    bench-compare.py --join <result>...     add these processes of the same tree to the profile's baseline,
                                            which is how a run that read a row above the baseline on the
                                            baseline's own tree joins it (ADR 0033)

A profile that has no baseline yet is reported and passes: the baseline for a runner is
set from that runner's own first run (ADR 0015), and a laptop has none unless someone
records one. The exit code is 1 on a regression, a missing or unreadable result, or an
allocation count above zero per message in the result.

Python 3.9 standard library only (ADR 0003).
"""

from __future__ import annotations

import datetime
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


def recorded_order(path: Path) -> tuple[int, str]:
    """The unix time the harness puts last in a result's name, then the name: so the newest
    run is the newest, whatever its profile's suffix (`-unchecked`, `-only`), where a
    sort by name put `-only-<time>` after every later full run of the profile."""
    tail = path.stem.rsplit("-", 1)[-1]
    return (int(tail) if tail.isdigit() else 0, path.name)


def newest_result() -> Path:
    files = sorted(RESULTS.glob("*.json"), key=recorded_order) if RESULTS.is_dir() else []
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
    base = baseline.get("profiles", {}).get(profile)
    step, shown = floor_step(result, base)
    processes = (base or {}).get("processes")
    from_processes = f", baseline from {processes} processes" if isinstance(processes, int) and processes > 1 else ""
    print(f"bench-compare: {profile}, {result.get('messages', '?')} messages, git {result.get('git', '?')}, "
          f"{shown}{from_processes}")
    floor = step or 0
    failures = 0
    # The gate requires zero allocations in the measured loops. The harness writes the
    # count as `allocations` and, when a message row ran, the ratio per message; a
    # `--only` run of the timer, slot or record rows has a `null` ratio and is judged by
    # the count. A result with neither field is from before the count and fails, as a
    # result without the ratio always did.
    count = result.get("allocations")
    ratio = result.get("allocations_per_message")
    if count is None and ratio is None:
        print("bench-compare: the result carries neither allocations nor allocations_per_message; "
              "the gate requires 0 allocations (CLAUDE.md, gate 2); run `just bench` again with this harness")
        failures += 1
    elif (count or 0) > 0 or (ratio or 0) > 0:
        print(f"bench-compare: {count if count is not None else '?'} allocations in the measured loops, "
              f"{ratio if ratio is not None else 'no'} per message; the gate requires 0 (CLAUDE.md, gate 2)")
        failures += 1
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
            elif ratio > TOLERANCE and now[GATED] - slowest > floor:
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
        rule = (f"more than {TOLERANCE:.0%} and more than one clock step ({step} ns) above the baseline's "
                "slowest run" if step else
                f"more than {TOLERANCE:.0%} above the baseline's slowest run, clock step unknown, so the rule "
                "has no floor")
        print(f"bench-compare: {failures} finding(s); a message row's p99.99 fails the gate when it is {rule} "
              "(CLAUDE.md, gate 2; ADR 0029). Measure again on an idle machine before you believe either number.")
        return 1
    print("bench-compare: ok")
    return 0


def clock_step(result: dict):
    """The step of the clock the result was measured with, in whole nanoseconds, at least 1,
    or None for a result from before the field, which is judged by the tolerance alone."""
    step = result.get(STEP_FIELD)
    return int(step) if isinstance(step, (int, float)) and step >= 1 else None


def floor_step(result: dict, base) -> tuple:
    """The step the floor is one of, and how the first line says it. The result under
    judgement carries the step the harness measured; the baseline's entry carries the one
    its recording measured, once `--update` has written it; the floor is the smaller, so
    that a change to the harness that over-read its step cannot widen the gate by itself
    (ADR 0029). None for a result from before the field, which is judged by the tolerance
    alone, whatever the baseline carries."""
    now = clock_step(result)
    then = clock_step(base or {})
    if now is None:
        return None, "clock step unknown, the 5 percent rule alone"
    if then is None:
        return now, f"clock step {now} ns, none in the baseline"
    if now == then:
        return now, f"clock step {now} ns"
    return min(now, then), f"clock step {min(now, then)} ns, the smaller of the result's {now} and the baseline's {then}"


def recorded_at(result: dict) -> str | None:
    """The result's date as the harness wrote it, `date_unix`, as a UTC date-time: the
    baseline shows when its numbers were measured, which the git field alone does not,
    since a tree is re-recorded after a host changes (card 158)."""
    unix = result.get("date_unix")
    if unix is None:
        return None
    return datetime.datetime.fromtimestamp(int(unix), datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def update(results: list[dict], baseline: dict, join: bool = False) -> int:
    """Write the baseline entry for the results' profile. One result is one process of the
    harness; a bench host's baseline is recorded from every process it has run at one
    tree, three at least, never two (ADR 0033). The entry carries, for each percentile, the
    largest value over the processes, so p50 can be one process's and p99 another's; every
    process's runs in order in `p99_99_ns_runs`, each `repeat` long, so the slowest run the
    gate compares with is the slowest over all of them; and `processes`.
    The results must be of one profile and one harness setting, the same messages, repeat
    and clock step, with the same rows; `git` is the one commit, or the list when the
    processes ran at several, and `date` is the newest process's. With `join`, the
    results are added to the entry the profile already has, which must be of the same
    harness setting and rows: the entry's processes come first, as they were written."""
    if join:
        results = joined(results, baseline)
    results = sorted(results, key=lambda r: r.get("date_unix") or 0)
    for field in ("profile", "messages", "repeat", STEP_FIELD):
        values = sorted({json.dumps(r.get(field)) for r in results})
        if len(values) > 1:
            sys.exit(f"bench-compare: --update takes results of one {field}; these have {', '.join(values)}. "
                     "A baseline is recorded from processes of one profile and one harness setting (ADR 0033).")
    if len({tuple(sorted(r["benchmarks"])) for r in results}) > 1:
        sys.exit("bench-compare: --update takes results with the same rows; these differ. "
                 "Record every process with the same harness.")
    if len(results) == 2:
        sys.exit("bench-compare: two processes make no baseline; a bench host's baseline is every process "
                 "it has run at the tree, three at least, and a laptop's is one (ADR 0033).")
    newest = results[-1]
    profile = newest["profile"]
    gits = []
    for r in results:
        if r.get("git") not in gits:
            gits.append(r.get("git"))
    entry = {
        "git": gits[0] if len(gits) == 1 else gits,
        "date": recorded_at(newest),
        "messages": newest.get("messages"),
        "repeat": newest.get("repeat"),
        "processes": len(results),
        **({STEP_FIELD: clock_step(newest)} if clock_step(newest) else {}),
        "benchmarks": {
            name: {
                **{k: max(r["benchmarks"][name][k] for r in results)
                   for k in SHOWN if all(k in r["benchmarks"][name] for r in results)},
                "p99_99_ns_runs": [run for r in results
                                   for run in (r["benchmarks"][name].get("p99_99_ns_runs") or [r["benchmarks"][name][GATED]])],
            }
            for name in sorted(newest["benchmarks"])
        },
    }
    baseline.setdefault("profiles", {})[profile] = entry
    BASELINE.write_text(json.dumps(baseline, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    shown = BASELINE.relative_to(ROOT) if BASELINE.is_relative_to(ROOT) else BASELINE
    count = f"{len(results)} process" + ("es" if len(results) != 1 else "")
    print(f"bench-compare: baseline for {profile!r} written from {count} at {' and '.join(str(g) for g in gits)}; "
          f"commit {shown} with the histograms in the message")
    return 0


def joined(results: list[dict], baseline: dict) -> list[dict]:
    """The processes already in the results' profile's entry, as result-shaped dicts in the
    order they were written, followed by the new results, for `--join`. The entry holds
    every process's runs in order, each `repeat` long, and each percentile from the slowest
    process; a process read back from it carries the entry's percentiles, which the
    maximum over processes leaves unchanged, and its own runs. A process has no date of
    its own in the entry, so the earlier processes sort before every new result."""
    profile = results[0]["profile"]
    entry = baseline.get("profiles", {}).get(profile)
    if entry is None:
        sys.exit(f"bench-compare: --join needs a baseline for profile {profile!r} to add to; "
                 "record one with --update first (ADR 0033).")
    repeat = entry.get("repeat") or 1
    count = entry.get("processes") or 1
    gits = entry["git"] if isinstance(entry["git"], list) else [entry["git"]]
    earlier = []
    for i in range(count):
        earlier.append({
            "profile": profile, "git": gits[min(i, len(gits) - 1)], "date_unix": -count + i,
            "messages": entry.get("messages"), "repeat": entry.get("repeat"),
            **({STEP_FIELD: entry[STEP_FIELD]} if STEP_FIELD in entry else {}),
            "benchmarks": {
                name: {**{k: row[k] for k in SHOWN if k in row},
                       "p99_99_ns_runs": (row.get("p99_99_ns_runs") or [row[GATED]] * repeat)[i * repeat:(i + 1) * repeat]}
                for name, row in entry["benchmarks"].items()
            },
        })
    return earlier + results


def main(argv: list[str]) -> int:
    args = [a for a in argv[1:] if not a.startswith("--")]
    flags = {a for a in argv[1:] if a.startswith("--")}
    updating = "--update" in flags
    joining = "--join" in flags
    if flags - {"--update", "--join"} or (updating and joining) or (len(args) > 1 and not updating and not joining) \
            or (joining and not args):
        sys.exit("usage: bench-compare.py [result.json]\n       bench-compare.py --update [result.json ...]\n"
                 "       bench-compare.py --join result.json [result.json ...]")
    paths = [Path(a) for a in args] or [newest_result()]
    results = [load(path) for path in paths]
    baseline = load(BASELINE) if BASELINE.exists() else {"schema": 0, "profiles": {}}
    for path, result in zip(paths, results):
        if result.get("schema") != 0:
            sys.exit(f"bench-compare: {path} has schema {result.get('schema')!r}; this script knows 0")
        print(f"bench-compare: {path.relative_to(ROOT) if path.is_relative_to(ROOT) else path}")
    if updating or joining:
        return update(results, baseline, join=joining)
    return compare(results[0], baseline)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
