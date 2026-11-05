#!/usr/bin/env python3
"""wait-for-partner-job: hold this job until the run's partner job is in progress (card 212).

    wait-for-partner-job.sh "bench (udp peer)"    in bench (udp), on bench
    wait-for-partner-job.sh "bench (udp)"         in bench (udp peer), on grip

`bench (udp)` and `bench (udp peer)` are the two ends of one measurement over the cable,
each on its own host, and the harness on each end waits ten minutes for the other to
connect (docs/how-to/run-the-bench.md, step 5). Both jobs queue once `bench` and
`bench-grip` end, but each host's queue is its own: the partner can sit behind another
run's job on its host for longer than those ten minutes, and the end that started first
dies waiting, as runs 42017993331, 42051225401 and 42051752280 did on 2026-11-05. So each
job runs this first, doing nothing until the run's other job is in progress, read from the
run's jobs through the API every 20 s; then both reach the harness within one poll of each
other, and the harness's own wait holds.

It exits 0 once the partner is in progress, and 1, with a message that says which reason
ended the wait and what to do, when:

- the partner completed before this job reached it, so the pair cannot meet;
- the partner, or this job, is not listed in the run's jobs after LISTED_WITHIN: the two
  jobs of the pair are created together, the moment their `needs` end, so a name still
  missing after the first minute is a wrong name, not a wait;
- the API refused a request (any 4xx: a token without `actions: read`, a spent rate limit,
  a wrong run), or did not answer TRIES times in a row, POLL apart;
- the partner has not started within BOUND, 45 minutes of wall clock, which is a host
  whose runner is gone or a queue longer than any this repository has seen.

Environment, checked before the first request:
  GH_TOKEN           the job's token with `actions: read`; the step sets it from github.token.
                     Sent as a header, never on argv, never printed
  GITHUB_API_URL     Actions sets it; https only, loopback http for the tests
  GITHUB_REPOSITORY  Actions sets it
  GITHUB_RUN_ID      Actions sets it
  RUNNER_NAME        Actions sets it; this job is the run's job in progress on that runner

Standard library only; scripts/wait-for-partner-job-test.py tests it against a fake API on
loopback, and `just test-scripts` runs that.
"""
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

POLL = 20               # s between reads of the run's jobs
LISTED_WITHIN = 60      # s for both jobs of the pair to be listed, since they are created together
BOUND = 45 * 60         # s of wall clock for the partner to start
TIMEOUT = 20            # s for one request
TRIES = 3               # requests in a row that may go unanswered, POLL apart
DOCS = "docs/how-to/run-the-bench.md, step 5"
NEEDS = {
    "GH_TOKEN": "the step sets it: `env: GH_TOKEN: ${{ github.token }}`",
    "GITHUB_API_URL": "Actions sets it in every job",
    "GITHUB_REPOSITORY": "Actions sets it in every job",
    "GITHUB_RUN_ID": "Actions sets it in every job",
    "RUNNER_NAME": "Actions sets it in every job",
}


class Fail(Exception):
    """The end of the wait: what happened and what to do."""


class Api:
    """GET against the run's repository with the job's token, and nothing else."""

    def __init__(self, env, sleep, say):
        self.base = f"{env['GITHUB_API_URL'].rstrip('/')}/repos/{env['GITHUB_REPOSITORY']}"
        self._token = env["GH_TOKEN"]
        self.sleep, self.say = sleep, say
        # https only: the job token rides on every request. Loopback is the exception the tests run on.
        if not (self.base.startswith("https://") or re.match(r"^http://(127\.0\.0\.1|localhost)(:\d+)?/", self.base)):
            raise Fail(f"GITHUB_API_URL must use https (http is allowed only for 127.0.0.1/localhost): {env['GITHUB_API_URL']}")

    def get(self, path):
        """GET <path and query> -> the parsed JSON. A 4xx ends the wait at once with the API's
        own message, which carries no credential; anything else is tried TRIES times."""
        headers = {"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28",
                   "Authorization": f"Bearer {self._token}"}
        request = urllib.request.Request(self.base + path, headers=headers)
        where = path.split("?")[0]
        for attempt in range(1, TRIES + 1):
            try:
                with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
                    return json.loads(response.read().decode("utf-8"))
            except urllib.error.HTTPError as e:
                message = api_message(e)
                if e.code < 500:
                    raise Fail(f"GET {where} answered HTTP {e.code}: {message}; {refusal_hint(e)}") from None
                reason = f"HTTP {e.code}: {message}"
            except (urllib.error.URLError, OSError, ValueError) as e:
                reason = str(getattr(e, "reason", e))
            if attempt == TRIES:
                raise Fail(f"GET {where} went unanswered {TRIES} times in a row, {POLL} s apart: {reason}; "
                           f"if https://www.githubstatus.com shows Actions or the API down, re-run both jobs "
                           f"of the pair once it is up ({DOCS})")
            self.say(f"GET {where}: {reason}; trying again in {POLL} s")
            self.sleep(POLL)
        return None    # unreachable: every path above returns or raises


def api_message(error):
    try:
        return json.loads(error.read().decode("utf-8")).get("message", "")
    except (ValueError, OSError, AttributeError):
        return ""


def refusal_hint(error):
    """What the operator does about a 4xx."""
    if error.code == 429 or error.headers.get("x-ratelimit-remaining") == "0":
        reset = error.headers.get("x-ratelimit-reset")
        when = time.strftime("%H:%M:%S UTC", time.gmtime(int(reset))) if reset and reset.isdigit() else "its reset"
        return (f"the job token's rate limit, 1,000 requests an hour for the repository, is spent until "
                f"{when}; re-run both jobs of the pair after it ({DOCS})")
    if error.code == 401:
        return "the token was refused; the step must set `GH_TOKEN: ${{ github.token }}`"
    return ("the job's token needs `actions: read` (the job's `permissions` in .github/workflows/ci.yml), "
            "and GITHUB_REPOSITORY and GITHUB_RUN_ID must name this run")


def environment(env):
    for name, why in NEEDS.items():
        if not env.get(name):
            raise Fail(f"{name} is not set; {why}")
    if not env["GITHUB_RUN_ID"].isdigit():
        raise Fail(f"GITHUB_RUN_ID is not a run id: {env['GITHUB_RUN_ID']}")


class Wait:
    def __init__(self, partner, env, clock, sleep, say):
        self.partner, self.clock, self.sleep, self.say = partner, clock, sleep, say
        self.api = Api(env, sleep, say)
        self.run_id = int(env["GITHUB_RUN_ID"])
        self.runner = env["RUNNER_NAME"]

    def jobs(self, run_id):
        body = self.api.get(f"/actions/runs/{run_id}/jobs?per_page=100")
        jobs = body.get("jobs") if isinstance(body, dict) else None
        if not isinstance(jobs, list):
            raise Fail(f"GET /actions/runs/{run_id}/jobs answered without a list of jobs: {str(body)[:200]}")
        return jobs

    def run(self):
        start = self.clock()
        while True:
            jobs = self.jobs(self.run_id)
            waited = int(self.clock() - start)
            me = next((j for j in jobs if j.get("runner_name") == self.runner and j.get("status") == "in_progress"), None)
            partner = next((j for j in jobs if j.get("name") == self.partner), None)
            if me is not None and partner is not None and me.get("id") == partner.get("id"):
                raise Fail(f"{self.partner!r} is this job's own name; pass the other job of the pair")
            if me is None or partner is None:
                if waited >= LISTED_WITHIN:
                    raise Fail(self.not_listed(jobs, me, waited))
                self.say(f"{self.partner} is not listed yet after {waited} s")
            elif partner.get("status") == "in_progress":
                self.say(f"{self.partner} is in progress after {waited} s")
                return
            elif partner.get("status") == "completed":
                raise Fail(f"{self.partner} completed before this job reached it, so the pair cannot meet; "
                           f"re-run both jobs of the pair when both hosts are free ({DOCS})")
            else:
                self.say(f"{self.partner} is {partner.get('status')} after {waited} s")
            if self.clock() - start >= BOUND:
                raise Fail(f"{self.partner} did not start within {BOUND // 60} minutes; is its host's runner "
                           f"online, or its queue that long? ({DOCS})")
            self.sleep(POLL)

    def not_listed(self, jobs, me, waited):
        listed = ", ".join(f"{j.get('name')!r} ({j.get('status')}, runner {j.get('runner_name')!r})" for j in jobs)
        if me is None:
            return (f"no job of run {self.run_id} is in progress on runner {self.runner!r} after {waited} s, "
                    f"so this job cannot find itself; the run's jobs are {listed or 'none'}")
        return (f"{self.partner!r} is not a job of run {self.run_id} after {waited} s, and the pair's two "
                f"jobs are created together; pass the other job's `name:` from .github/workflows/ci.yml. "
                f"The run's jobs are {listed or 'none'}")


def main(argv, env=None, clock=time.monotonic, sleep=time.sleep):
    env = os.environ if env is None else env
    say = lambda line: print(f"wait-for-partner-job: {line}", flush=True)
    if len(argv) != 1 or not argv[0]:
        print("usage: wait-for-partner-job.sh <the partner job's name, as the workflow names it>", file=sys.stderr)
        return 2
    try:
        environment(env)
        Wait(argv[0], env, clock, sleep, say).run()
        return 0
    except Fail as e:
        print(f"wait-for-partner-job: {e}", file=sys.stderr, flush=True)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
