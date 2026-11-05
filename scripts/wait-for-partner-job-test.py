#!/usr/bin/env python3
"""Tests of wait-for-partner-job.py against a fake GitHub API on loopback and a fake clock: no
network, no token, no waiting. Also the join that keeps the script and the workflow's two udp
jobs in step. Standard library only; `just test-scripts` runs it.

    python3 scripts/wait-for-partner-job-test.py
"""
import contextlib
import http.server
import importlib.util
import io
import json
import os
import re
import subprocess
import sys
import threading
import unittest
from pathlib import Path
from urllib.parse import urlsplit

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
spec = importlib.util.spec_from_file_location("wait_for_partner_job", HERE / "wait-for-partner-job.py")
wait = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wait)

FAKE_TOKEN = "ghs_fakeFAKEfake1234567890abcdef"
REPO = "example-owner/cavalry"
UDP, PEER = "bench (udp)", "bench (udp peer)"
BENCH, GRIP = ["self-hosted", "bench-linux"], ["self-hosted", "grip-linux"]


def job(id_, name, status, labels, runner=None, created="2026-11-05T15:10:52Z", done="2026-11-05T15:28:40Z"):
    return {"id": id_, "name": name, "status": status, "conclusion": "success" if status == "completed" else None,
            "labels": labels, "runner_name": runner if status != "queued" else None, "run_attempt": 1,
            "created_at": created, "completed_at": done if status == "completed" else None}


def concluded(jobs, name, conclusion):
    return [{**j, "conclusion": conclusion} if j["name"] == name else j for j in jobs]


def pair(run_id, udp, peer):
    """A run's jobs at the moment its pair is queued or running: the single-host jobs done,
    and the pair's two created together the moment they ended, as in run 42051225401."""
    return [job(run_id * 10 + 1, "bench (linux)", "completed", BENCH, "bench", "2026-11-05T15:00:00Z", "2026-11-05T15:10:52Z"),
            job(run_id * 10 + 2, "bench (grip)", "completed", GRIP, "grip", "2026-11-05T15:00:00Z", "2026-11-05T15:04:10Z"),
            job(run_id * 10 + 3, UDP, udp, BENCH, "bench", "2026-11-05T15:10:52Z"),
            job(run_id * 10 + 4, PEER, peer, GRIP, "grip", "2026-11-05T15:10:53Z")]


class Clock:
    """time.monotonic and time.sleep, with no waiting."""

    def __init__(self):
        self.now, self.slept = 0.0, []

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds
        self.slept.append(seconds)


class FakeGitHub:
    """Serves routes by path, the query dropped, and records every request. A route is a
    body, served with 200, or a function of the request's count on that path that returns
    a body or (status, body, headers)."""

    def __init__(self):
        self.routes, self.seen, self.on_request = {}, [], None
        fake = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                path = urlsplit(self.path).path
                fake.seen.append((self.path, dict(self.headers)))
                if fake.on_request:
                    fake.on_request(path)
                route = fake.routes.get(path)
                if callable(route):
                    route = route(sum(1 for p, _ in fake.seen if urlsplit(p).path == path))
                status, body, headers = route if isinstance(route, tuple) else (200, route, {})
                if body is None:
                    status, body = 404, {"message": "Not Found"}
                payload = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                for k, v in headers.items():
                    self.send_header(k, v)
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *args):
                pass

        self.server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def count(self, path):
        return sum(1 for p, _ in self.seen if urlsplit(p).path == f"/repos/{REPO}{path}")

    def close(self):
        self.server.shutdown()
        self.server.server_close()


class Base(unittest.TestCase):
    RUN, OLDER, WORKFLOW = 200, 150, 7

    def setUp(self):
        self.gh = FakeGitHub()
        self.addCleanup(self.gh.close)
        self.clock = Clock()
        self.env = {"GH_TOKEN": FAKE_TOKEN, "GITHUB_API_URL": self.gh.url, "GITHUB_REPOSITORY": REPO,
                    "GITHUB_RUN_ID": str(self.RUN), "RUNNER_NAME": "bench"}
        for run_id in (self.RUN, self.OLDER):
            self.route(f"/actions/runs/{run_id}", {"id": run_id, "workflow_id": self.WORKFLOW})
        self.in_progress(self.RUN)

    def route(self, path, body):
        self.gh.routes[f"/repos/{REPO}{path}"] = body

    def in_progress(self, *run_ids):
        """The workflow's runs in progress, newest first, as GitHub lists them."""
        runs = [{"id": r, "html_url": f"https://github.example/{REPO}/actions/runs/{r}"} for r in sorted(run_ids, reverse=True)]
        self.route(f"/actions/workflows/{self.WORKFLOW}/runs", {"total_count": len(runs), "workflow_runs": runs})

    def jobs(self, run_id, body):
        self.route(f"/actions/runs/{run_id}/jobs", body if callable(body) or isinstance(body, tuple) else {"jobs": body})

    def wait_for(self, partner=PEER, env=None):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = wait.main([partner], env={**self.env, **(env or {})}, clock=self.clock, sleep=self.clock.sleep)
        self.out, self.err = out.getvalue(), err.getvalue()
        self.assertNotIn(FAKE_TOKEN, self.out + self.err, "the output carries the job token")
        return code

    def passes(self, *wanted, **kw):
        self.assertEqual(self.wait_for(**kw), 0, self.out + self.err)
        for w in wanted:
            self.assertIn(w, self.out)

    def fails(self, *wanted, **kw):
        self.assertEqual(self.wait_for(**kw), 1, self.out + self.err)
        for w in wanted:
            self.assertIn(w, self.err)


def then(*bodies):
    """A route that serves each body once, in order, and the last one from then on."""
    return lambda n: {"jobs": bodies[min(n, len(bodies)) - 1]}


class Meets(Base):
    def test_a_partner_in_progress_ends_the_wait_at_once(self):
        self.jobs(self.RUN, pair(self.RUN, "in_progress", "in_progress"))
        self.passes(f"{PEER} is in progress after 0 s")
        self.assertEqual(self.clock.slept, [])

    def test_a_queued_partner_is_waited_for_until_it_starts(self):
        queued = pair(self.RUN, "in_progress", "queued")
        self.jobs(self.RUN, then(queued, queued, queued, pair(self.RUN, "in_progress", "in_progress")))
        self.passes(f"{PEER} is queued after 0 s", f"{PEER} is queued after 40 s", f"{PEER} is in progress after 60 s")

    def test_the_peer_end_waits_for_bench_udp_the_same_way(self):
        self.jobs(self.RUN, then(pair(self.RUN, "queued", "in_progress"), pair(self.RUN, "in_progress", "in_progress")))
        self.passes(f"{UDP} is queued after 0 s", f"{UDP} is in progress after 20 s", partner=UDP, env={"RUNNER_NAME": "grip"})

    def test_a_partner_listed_late_within_the_minute_is_waited_for(self):
        early = [j for j in pair(self.RUN, "in_progress", "queued") if j["name"] != PEER]
        self.jobs(self.RUN, then(early, early, pair(self.RUN, "in_progress", "in_progress")))
        self.passes(f"{PEER} is not listed yet after 20 s", f"{PEER} is in progress after 40 s")


class Ends(Base):
    def test_a_partner_that_failed_first_is_re_run_with_the_failed_jobs(self):
        self.jobs(self.RUN, concluded(pair(self.RUN, "in_progress", "completed"), PEER, "failure"))
        self.fails(f"{PEER} completed (failure) before this job reached it, so the pair cannot meet",
                   "once the run has ended, `gh run rerun 200 --failed` re-runs both ends")
        self.assertEqual(self.clock.slept, [])

    def test_a_partner_that_passed_in_this_attempt_is_re_run_with_the_whole_run(self):
        self.jobs(self.RUN, pair(self.RUN, "in_progress", "completed"))
        self.fails(f"{PEER} completed (success) before this job reached it",
                   "`--failed` would re-run this end alone", "only the whole run re-runs both ends",
                   "download the attempt's artifacts (step 4), then run `gh run rerun 200` (")
        self.assertNotIn("gh run rerun 200 --failed", self.err)

    def test_a_partner_that_never_starts_ends_at_the_bound(self):
        self.jobs(self.RUN, pair(self.RUN, "in_progress", "queued"))
        self.fails(f"{PEER} did not start within 45 minutes", "runner online",
                   "it finds this job completed and ends too", "`gh run rerun 200 --failed` re-runs both ends")
        self.assertGreaterEqual(self.clock.now, wait.BOUND)
        self.assertLess(self.clock.now, wait.BOUND + wait.POLL)

    def test_the_bound_is_wall_clock_and_counts_a_slow_api(self):
        self.jobs(self.RUN, pair(self.RUN, "in_progress", "queued"))
        self.gh.on_request = lambda path: setattr(self.clock, "now", self.clock.now + 15)
        self.fails("did not start within 45 minutes")
        self.assertLess(self.clock.now, wait.BOUND + wait.POLL + 2 * 15)
        self.assertLess(len(self.clock.slept), wait.BOUND // wait.POLL)

    def test_a_wrong_partner_name_ends_after_a_minute_and_lists_the_names(self):
        self.jobs(self.RUN, pair(self.RUN, "in_progress", "queued"))
        self.fails("'bench (udp peers)' is not a job of run 200 after 60 s", f"'{PEER}' (queued",
                   "`name:` from .github/workflows/ci.yml", partner="bench (udp peers)")
        self.assertEqual(self.clock.now, 60)

    def test_a_job_that_cannot_find_itself_ends_after_a_minute(self):
        self.jobs(self.RUN, pair(self.RUN, "in_progress", "queued"))
        self.fails("no job of run 200 is in progress on runner 'kip' after 60 s", "runner 'bench'",
                   env={"RUNNER_NAME": "kip"})

    def test_its_own_name_is_refused(self):
        self.jobs(self.RUN, pair(self.RUN, "in_progress", "queued"))
        self.fails(f"'{UDP}' is this job's own name", partner=UDP)
        self.assertEqual(self.clock.slept, [])

    def test_a_missing_variable_ends_before_any_request(self):
        for name in wait.NEEDS:
            with self.subTest(name):
                self.fails(f"{name} is not set", env={name: ""})
        self.assertEqual(self.gh.seen, [])

    def test_an_http_api_url_that_is_not_loopback_is_refused(self):
        self.fails("GITHUB_API_URL must use https", env={"GITHUB_API_URL": "http://api.example"})


def listed(id_, name, attempt, status, conclusion, labels, runner, created, done):
    """One job as the jobs list gives it, with the fields the wait reads."""
    return {"id": id_, "name": name, "run_attempt": attempt, "status": status, "conclusion": conclusion,
            "labels": labels, "runner_name": runner, "created_at": created, "completed_at": done}


class ReRun(Base):
    """GitHub lists a job that an attempt did not re-run as a copy in that attempt: a new id
    and the attempt's `run_attempt`, with the earlier attempt's times and conclusion. So a
    re-run of one end finds its partner completed before this job was created, which a
    partner of the same attempt cannot be. The listings are run 42001521540's bench jobs, as
    `gh api repos/appshapes/cavalry/actions/runs/42001521540/jobs` gives them: attempt 3
    re-ran `bench (udp peer)` alone, and in attempt 2 the peer failed before `bench (udp)`
    started."""

    RUN = 42001521540

    def attempt_3(self, udp="success", udp_done="2026-11-05T09:58:08Z"):
        """Attempt 3 from the peer's end, re-run alone: `bench (udp)` is attempt 2's, copied."""
        return [listed(127175389992, PEER, 3, "in_progress", None, GRIP, "grip", "2026-11-05T09:58:17Z", None),
                listed(127175391268, "bench (grip)", 3, "completed", "success", GRIP, "grip", "2026-11-05T09:58:17Z", "2026-11-05T09:24:10Z"),
                listed(127175392697, UDP, 3, "completed", udp, BENCH, "bench", "2026-11-05T09:58:17Z", udp_done),
                listed(127175449669, "bench (linux)", 3, "completed", "success", BENCH, "bench", "2026-11-05T09:58:24Z", "2026-11-05T09:27:34Z")]

    def test_the_peer_re_run_alone_against_a_copied_success_names_the_whole_run(self):
        self.jobs(self.RUN, self.attempt_3())
        self.fails(f"{UDP} completed (success) at 2026-11-05T09:58:08Z, before this job was created at "
                   "2026-11-05T09:58:17Z, so it is an earlier attempt's result that this attempt kept: the attempt "
                   "re-ran this end alone, and the pair cannot meet",
                   "GitHub re-runs one job (`--job`), the failed jobs (`--failed`) or the whole run",
                   "download the attempt's artifacts (step 4), then run `gh run rerun 42001521540` (",
                   partner=UDP, env={"RUNNER_NAME": "grip"})
        self.assertNotIn("--failed` re-runs both ends", self.err)
        self.assertNotIn("before this job reached it", self.err)
        self.assertEqual(self.clock.slept, [])

    def test_the_same_against_a_copied_failure_names_the_failed_jobs(self):
        self.jobs(self.RUN, self.attempt_3(udp="failure"))
        self.fails(f"{UDP} completed (failure) at 2026-11-05T09:58:08Z, before this job was created at",
                   "the attempt re-ran this end alone, and the pair cannot meet",
                   "once the run has ended, `gh run rerun 42001521540 --failed` re-runs both ends",
                   partner=UDP, env={"RUNNER_NAME": "grip"})
        self.assertNotIn("then run `gh run rerun 42001521540` (", self.err)
        self.assertEqual(self.clock.slept, [])

    def test_a_partner_that_completed_after_this_job_was_created_is_the_race(self):
        # Attempt 2 from bench (udp)'s end, at 09:53:17: the peer, created with it at 09:27:34,
        # failed at 09:40:33, having waited its ten minutes.
        self.jobs(self.RUN, [
            listed(127158086123, "bench (linux)", 2, "completed", "success", BENCH, "bench", "2026-11-05T09:21:07Z", "2026-11-05T09:27:34Z"),
            listed(127158086388, "bench (grip)", 2, "completed", "success", GRIP, "grip", "2026-11-05T09:21:07Z", "2026-11-05T09:24:10Z"),
            listed(127161076193, PEER, 2, "completed", "failure", GRIP, "grip", "2026-11-05T09:27:34Z", "2026-11-05T09:40:33Z"),
            listed(127161076211, UDP, 2, "in_progress", None, BENCH, "bench", "2026-11-05T09:27:34Z", None)])
        self.fails(f"{PEER} completed (failure) before this job reached it, so the pair cannot meet",
                   "once the run has ended, `gh run rerun 42001521540 --failed` re-runs both ends")
        self.assertNotIn("re-ran this end alone", self.err)

    def test_a_partner_without_its_times_is_read_as_this_attempts(self):
        self.jobs(self.RUN, self.attempt_3(udp_done=None))
        self.fails(f"{UDP} completed (success) before this job reached it",
                   "then run `gh run rerun 42001521540` (", partner=UDP, env={"RUNNER_NAME": "grip"})


class Crossed(Base):
    """Run 150's bench (udp) holds bench and waits for its peer, queued on grip; run 200's peer
    holds grip and waits for its bench (udp), queued on bench behind 150's. Each end reads the
    same two runs, so the newer, 200, ends and the older keeps waiting."""

    def setUp(self):
        super().setUp()
        self.in_progress(self.RUN, self.OLDER)
        self.jobs(self.OLDER, pair(self.OLDER, "in_progress", "queued"))
        self.jobs(self.RUN, pair(self.RUN, "queued", "in_progress"))

    def test_the_newer_end_ends_at_once_naming_the_older_run(self):
        self.fails(f"{UDP} is queued, and its host runs {UDP} of run 150", f"waits for its own {PEER}, queued on this host",
                   "Run 150 is the older, so this job ends", f"and this run's {UDP} ends when it starts",
                   "once the run has ended, `gh run rerun 200 --failed` re-runs both ends",
                   partner=UDP, env={"RUNNER_NAME": "grip"})
        self.assertEqual(self.clock.slept, [])

    def test_the_older_end_keeps_waiting_and_meets_its_own_partner(self):
        crossed = pair(self.OLDER, "in_progress", "queued")
        self.jobs(self.OLDER, then(crossed, crossed, pair(self.OLDER, "in_progress", "in_progress")))
        self.jobs(self.RUN, pair(self.RUN, "queued", "in_progress"))
        self.passes(f"{PEER}'s host runs {PEER} of run 200", "that run is the newer, so its end ends and this one keeps waiting",
                    f"{PEER} is in progress after 40 s", env={"GITHUB_RUN_ID": str(self.OLDER)})

    def test_the_newer_ends_other_end_then_finds_it_completed(self):
        self.jobs(self.RUN, concluded(pair(self.RUN, "in_progress", "completed"), PEER, "failure"))
        self.fails(f"{PEER} completed (failure) before this job reached it", "`gh run rerun 200 --failed` re-runs both ends")

    def test_a_holder_that_is_not_a_pair_end_is_waited_for(self):
        self.jobs(self.OLDER, [j if j["name"] != "bench (grip)" else {**j, "status": "in_progress"}
                               for j in pair(self.OLDER, "queued", "queued")])
        self.jobs(self.RUN, then(pair(self.RUN, "in_progress", "queued"), pair(self.RUN, "in_progress", "in_progress")))
        self.passes(f"{PEER}'s host runs bench (grip) of run 150", f"{PEER} is in progress after 20 s")

    def test_a_holder_whose_own_partner_started_is_not_crossed(self):
        for theirs in ("in_progress", "completed"):
            with self.subTest(theirs):
                self.jobs(self.OLDER, pair(self.OLDER, theirs, "in_progress"))
                self.jobs(self.RUN, then(pair(self.RUN, "in_progress", "queued"), pair(self.RUN, "in_progress", "in_progress")))
                self.gh.seen.clear()
                self.passes(f"{PEER}'s host runs {PEER} of run 150", f"{PEER} is in progress", env={"RUNNER_NAME": "bench"})

    def test_a_job_named_as_the_partner_on_another_host_does_not_hold_its_host(self):
        self.jobs(self.OLDER, [{**j, "labels": ["self-hosted", "kip-linux"]} if j["name"] == PEER else j
                               for j in pair(self.OLDER, "queued", "in_progress")])
        self.jobs(self.RUN, then(pair(self.RUN, "in_progress", "queued"), pair(self.RUN, "in_progress", "in_progress")))
        self.passes(f"no other run of this workflow is in progress on {PEER}'s host", f"{PEER} is in progress")

    def test_it_looks_at_the_first_queued_poll_and_every_three_minutes(self):
        queued = pair(self.RUN, "in_progress", "queued")
        self.jobs(self.OLDER, pair(self.OLDER, "completed", "completed"))
        self.jobs(self.RUN, lambda n: {"jobs": queued if n <= 20 else pair(self.RUN, "in_progress", "in_progress")})
        self.passes(f"{PEER} is in progress after 400 s")
        self.assertEqual(self.gh.count(f"/actions/workflows/{self.WORKFLOW}/runs"), 3)    # at 0, 180 and 360 s
        self.assertEqual(self.gh.count(f"/actions/runs/{self.RUN}"), 1)

    def test_a_run_without_a_workflow_id_is_not_read_as_waiting(self):
        self.route(f"/actions/runs/{self.RUN}", {"id": self.RUN})
        self.jobs(self.RUN, pair(self.RUN, "in_progress", "queued"))
        self.fails(f"GET /actions/runs/{self.RUN} answered without a workflow_id")

    def test_the_whole_wait_stays_inside_a_third_of_the_tokens_hour(self):
        others = [101, 102, 103, 104, 105]
        self.in_progress(self.RUN, *others)
        for r in others:
            self.jobs(r, pair(r, "completed", "completed"))
        self.jobs(self.RUN, pair(self.RUN, "in_progress", "queued"))
        self.fails("did not start within 45 minutes")
        per_hour = len(self.gh.seen) / (wait.BOUND / 3600)
        self.assertLessEqual(per_hour, 1000 / 3, f"{len(self.gh.seen)} requests in 45 minutes")


class Refused(Base):
    def test_a_token_without_actions_read_ends_at_once_with_the_reason(self):
        self.jobs(self.RUN, (403, {"message": "Resource not accessible by integration"}, {}))
        self.fails("GET /actions/runs/200/jobs answered HTTP 403: Resource not accessible by integration",
                   "needs `actions: read`")
        self.assertEqual(self.clock.slept, [])

    def test_a_spent_rate_limit_says_so(self):
        self.jobs(self.RUN, (403, {"message": "API rate limit exceeded"}, {"x-ratelimit-remaining": "0", "x-ratelimit-reset": "1793898355"}))
        self.fails("HTTP 403: API rate limit exceeded", "rate limit, 1,000 requests an hour", "spent until 17:05:55 UTC",
                   "once it has reset and the run has ended, `gh run rerun 200 --failed` re-runs both ends")

    def test_a_refused_token_says_so(self):
        self.jobs(self.RUN, (401, {"message": "Bad credentials"}, {}))
        self.fails("HTTP 401: Bad credentials", "GH_TOKEN")

    def test_a_wrong_run_says_so(self):
        self.fails("answered HTTP 404: Not Found", "GITHUB_RUN_ID must name this run")

    def test_a_server_error_is_tried_again(self):
        good = {"jobs": pair(self.RUN, "in_progress", "in_progress")}
        self.jobs(self.RUN, lambda n: (502, {"message": "Bad Gateway"}, {}) if n < 3 else good)
        self.passes("HTTP 502: Bad Gateway; trying again in 20 s", f"{PEER} is in progress")

    def test_an_api_that_stays_down_ends_after_three_tries(self):
        self.jobs(self.RUN, (502, {"message": "Bad Gateway"}, {}))
        self.fails("went unanswered 3 times in a row, 20 s apart: HTTP 502: Bad Gateway", "githubstatus",
                   "once both are up and the run has ended, `gh run rerun 200 --failed` re-runs both ends")
        self.assertEqual(self.gh.count("/actions/runs/200/jobs"), 3)

    def test_a_body_without_jobs_is_not_read_as_waiting(self):
        self.jobs(self.RUN, (200, {"total_count": 0}, {}))
        self.fails("answered without a list of jobs")

    def test_the_token_rides_in_a_header(self):
        self.jobs(self.RUN, pair(self.RUN, "in_progress", "in_progress"))
        self.passes()
        path, headers = self.gh.seen[0]
        self.assertNotIn(FAKE_TOKEN, path)
        self.assertEqual(headers.get("Authorization"), f"Bearer {FAKE_TOKEN}")


class Wrapper(Base):
    """The name the workflow calls, run as the workflow runs it."""

    def run_sh(self, *args, env=None):
        full = {"PATH": os.environ["PATH"], **self.env, **(env or {})}
        return subprocess.run(["bash", str(HERE / "wait-for-partner-job.sh"), *args],
                              capture_output=True, text=True, env=full)

    def test_it_runs_the_python(self):
        self.jobs(self.RUN, pair(self.RUN, "in_progress", "completed"))
        r = self.run_sh(PEER)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn(f"{PEER} completed (success) before this job reached it", r.stderr)

    def test_no_argument_is_a_usage_error(self):
        r = self.run_sh()
        self.assertEqual(r.returncode, 2)
        self.assertIn("usage: wait-for-partner-job.sh", r.stderr)


class Workflow(unittest.TestCase):
    """ci.yml's two udp jobs each pass the other's `name:`, with the token the script reads."""

    def test_each_udp_job_waits_for_the_other(self):
        text = (ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
        jobs = re.split(r"\n  (?=[a-z][a-z0-9-]*:\n)", text.split("\njobs:\n", 1)[1])
        names, waits = {}, {}
        for block in jobs:
            name = re.search(r"^    name: (.+)$", block, re.M)
            called = re.search(r'scripts/wait-for-partner-job\.sh "([^"]+)"', block)
            if name and called:
                names[name.group(1)] = block
                waits[name.group(1)] = called.group(1)
        self.assertEqual(waits, {UDP: PEER, PEER: UDP})
        for name, block in names.items():
            with self.subTest(name):
                self.assertRegex(block, r"\n      actions: read\b")
                self.assertIn("GH_TOKEN: ${{ github.token }}", block)


if __name__ == "__main__":
    unittest.main()
