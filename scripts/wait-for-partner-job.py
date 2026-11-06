#!/usr/bin/env python3
"""wait-for-partner-job: hold this job until the run's partner job is in progress (card 212).

    wait-for-partner-job.sh "bench (udp peer)"    in bench (udp), on bench
    wait-for-partner-job.sh "bench (udp)"         in bench (udp peer), on grip

`bench (udp)` and `bench (udp peer)` are the two ends of one measurement over the cable,
each on its own host, and the harness on each end waits ten minutes for the other to
connect (docs/how-to/run-the-bench.md, step 5). Both jobs queue once `bench` and
`bench-grip` end, but each host's runner takes jobs from its own queue, so the two ends
of one run can start minutes apart, and on 2026-11-05 that went wrong in two ways:

- The harness's ten minutes ended one end of the run's pair. In runs 42017993331 (pull
  request 120) and 42051225401 (master at 97d0412) the end that started first, the peer,
  failed after 600 s, and the run's `bench (udp)` started minutes later. In runs
  42025388898 (master at 34049ac) and 42051752280 (pull request 119) the peer started
  first and passed against another run's `bench (udp)`, and the run's own `bench (udp)`
  started after the peer ended and failed after 600 s.
- The ends that passed had met the end of another run, built from another tree. Run
  42051225401's `bench (udp)`, 15:23:48 to 15:28:40, measured against the peer of run
  42051665137 (branch `204-c-abi-adr`), 15:23:52 to 15:28:40; run 42017993331's, 12:02:57
  to 12:13:21, against the peer of run 42018433705, 12:08:37 to 12:13:20; and run
  42051752280's peer, 15:28:41 to 15:33:32, echoed run 42051665137's `bench (udp)`,
  15:28:41 to 15:33:33. So a green `udp_round_trip_32` could be round trips that were
  half another tree's code.

So each job runs this first, doing nothing until the run's other job is in progress, read
from the run's jobs through the API every 20 s. Both ends then reach the harness within
one poll of each other, so the harness's own wait holds, and each meets its own partner:
an end passes the wait only while its own partner holds the other host. That rests on one
runner per label: `bench-linux` names bench's one runner and `grip-linux` grip's
(docs/how-to/set-up-a-bench-host.md). With a second runner under either label, another
run's end could run beside this run's partner, and the look below could read the wrong
job as the one that holds the partner's host.

Each end holds its host while it waits, and each host's runner picks from its own queue,
not in creation order (on 2026-11-05 grip started run 42017993331's `bench (udp peer)`
before run 42018433705's older `bench (grip)`). So two runs' pairs can hold one host each
and wait on each other: bench runs run X's `bench (udp)`, waiting for X's peer, queued on
grip behind run Y's peer, which grip runs, waiting for Y's `bench (udp)`, queued on bench
behind X's. Neither queue moves until the bound, and both gate hosts stand idle. So while
the partner is queued, at the first poll and every LOOK_EVERY after it, this reads the
workflow's runs listed as in progress and as queued for the job that holds the partner's
host: a job in progress with the partner's labels. Both lists, because GitHub lists a run
whose jobs are part queued and part in progress under queued: on 2026-11-06 runs
42133574089 and 42143752791 each held a host that way and appeared under queued alone, so
the first version of this script, which read the runs in progress alone, never saw the
holder, and two pairs crossed for 38 and 23 minutes until a person cancelled one end. When that job is named as the partner and its own run's job
named as this one is still queued, the pair is crossed, and the end in the newer run, the
higher run id, ends at once, naming the older run; the older run's end keeps waiting. Both
ends read the same two runs, so both decide the same, and the older pair meets once the
newer end has freed its host. Its other end then finds it completed and ends too.

The job token has 1,000 requests an hour for the repository. At most two ends wait at
once, one on each host, and each makes 3 reads a minute, 180 an hour, plus 20 looks an
hour at 2 + (the workflow's other runs listed) reads each, stopping at the holder: with
5 other runs listed, about 320 an hour for an end, and 640 for both.

It exits 0 once the partner is in progress, and 1, with a message that says which reason
ended the wait and what to do, when:

- the partner completed after this job was created but before this job reached it, so
  the pair cannot meet;
- the partner completed before this job was created, so the attempt re-ran this end
  alone and the pair cannot meet, as below;
- the pair is crossed with another run's, as above, and this run is the newer;
- the partner, or this job, is not listed in the run's jobs after LISTED_WITHIN: the two
  jobs of the pair are created together, the moment their `needs` end, so a name still
  missing after the first minute is a wrong name;
- the API refused a request (any 4xx: a token without `actions: read`, a spent rate limit,
  a wrong run), or did not answer TRIES times in a row, POLL apart;
- the partner has not started within BOUND, 45 minutes of wall clock, which is a host
  whose runner is gone or a queue longer than any this repository has seen.

Each message that ends an end of the pair names the re-run that brings both ends back.
GitHub re-runs one job (`gh run rerun <run id> --job <job id>`, or the job's re-run
button), the failed jobs (`--failed`) or the whole run, each once the run has ended, and
never two chosen jobs; a job an attempt does not re-run keeps its earlier result. So when
both ends fail, `--failed` re-runs the pair, and when the partner passed, as the peer
does when `bench-compare` fails a link row on `bench (udp)`, `--failed` or `--job` re-runs
this end alone, and only the whole run re-runs both.

GitHub lists a job that an attempt did not re-run as a copy in that attempt: a new id and
the attempt's `run_attempt`, with the earlier attempt's `started_at`, `completed_at` and
conclusion, and a `created_at` of the attempt. Attempt 3 of run 42001521540 re-ran
`bench (udp peer)` alone, and lists `bench (udp)` as id 127175392697, `run_attempt` 3,
created at 09:58:17, completed at 09:58:08 with success; attempt 3 of run 41896534567,
which re-ran `gates (linux)` alone, lists every other job the same way. So a re-run of
one end finds its partner completed and of its own attempt, and what tells the two cases
apart is the time: a partner of this attempt is created with this job, so it cannot have
completed before this job was created, and a copy completed before its attempt began.

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
from datetime import datetime

POLL = 20               # s between reads of the run's jobs
LOOK_EVERY = 180        # s between looks at what holds the partner's host, while it is queued
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
        self.run_id = env["GITHUB_RUN_ID"]
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
        for tried in range(1, TRIES + 1):
            try:
                with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
                    return json.loads(response.read().decode("utf-8"))
            except urllib.error.HTTPError as e:
                message = api_message(e)
                if e.code < 500:
                    raise Fail(f"GET {where} answered HTTP {e.code}: {message}; {refusal_hint(e, self.run_id)}") from None
                reason = f"HTTP {e.code}: {message}"
            except (urllib.error.URLError, OSError, ValueError) as e:
                reason = str(getattr(e, "reason", e))
            if tried == TRIES:
                raise Fail(f"GET {where} went unanswered {TRIES} times in a row, {POLL} s apart: {reason}; "
                           f"https://www.githubstatus.com says whether Actions or the API is down; "
                           f"{rerun_failed(self.run_id, 'both are up and ')}")
            self.say(f"GET {where}: {reason}; trying again in {POLL} s")
            self.sleep(POLL)
        return None    # unreachable: every path above returns or raises


def api_message(error):
    try:
        return json.loads(error.read().decode("utf-8")).get("message", "")
    except (ValueError, OSError, AttributeError):
        return ""


def refusal_hint(error, run_id):
    """What the operator does about a 4xx."""
    if error.code == 429 or error.headers.get("x-ratelimit-remaining") == "0":
        reset = error.headers.get("x-ratelimit-reset")
        when = time.strftime("%H:%M:%S UTC", time.gmtime(int(reset))) if reset and reset.isdigit() else "its reset"
        return (f"the job token's rate limit, 1,000 requests an hour for the repository, is spent until "
                f"{when}; {rerun_failed(run_id, 'it has reset and ')}")
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


def rerun_failed(run_id, first=""):
    """The re-run once both ends have failed in this attempt: the failed jobs are the pair."""
    return f"once {first}the run has ended, `gh run rerun {run_id} --failed` re-runs both ends ({DOCS})"


def rerun_whole(run_id):
    """The re-run once one end has passed, which `--failed` and `--job` leave as it was."""
    return (f"GitHub re-runs one job (`--job`), the failed jobs (`--failed`) or the whole run, so only the "
            f"whole run re-runs both ends: once the run has ended, download the attempt's artifacts (step 4), "
            f"then run `gh run rerun {run_id}` ({DOCS})")


def when(job, key):
    """The job's time under <key>, as the API gives it (`2026-11-05T09:58:17Z`), or None."""
    try:
        return datetime.fromisoformat(job[key].replace("Z", "+00:00"))
    except (KeyError, AttributeError, ValueError):
        return None


def copied(partner, me):
    """Whether the partner completed before this job was created: the copy of a job that this
    attempt did not re-run (the docstring above). The jobs list gives the copy this attempt's
    `run_attempt`, so the times are what tell. A partner without both times is read as this
    attempt's, whose message names the same re-run and gives the race as the reason."""
    done, created = when(partner, "completed_at"), when(me, "created_at")
    return done is not None and created is not None and done < created


def labels(job):
    return sorted(job.get("labels") or [])


def started(job):
    return job.get("status") in ("in_progress", "completed")


class Wait:
    def __init__(self, partner, env, clock, sleep, say):
        self.partner, self.clock, self.sleep, self.say = partner, clock, sleep, say
        self.api = Api(env, sleep, say)
        self.run_id = int(env["GITHUB_RUN_ID"])
        self.runner = env["RUNNER_NAME"]
        self.workflow = None

    def listed(self, path, key):
        body = self.api.get(path)
        items = body.get(key) if isinstance(body, dict) else None
        if not isinstance(items, list):
            raise Fail(f"GET {path.split('?')[0]} answered without a list of {key}: {str(body)[:200]}")
        return items

    def jobs(self, run_id):
        return self.listed(f"/actions/runs/{run_id}/jobs?per_page=100", "jobs")

    def look_across(self, me, partner):
        """What holds the partner's host, and whether it is the end of another run's pair whose
        own partner is queued on this host, behind this job. Then neither queue moves until the
        bound, so the newer run's end ends and the older's keeps waiting: both ends read the
        same two runs, so both decide the same. GitHub numbers runs as it creates them, so the
        lower id is the older run."""
        if self.workflow is None:
            run = self.api.get(f"/actions/runs/{self.run_id}")
            self.workflow = run.get("workflow_id") if isinstance(run, dict) else None
            if not self.workflow:
                raise Fail(f"GET /actions/runs/{self.run_id} answered without a workflow_id: {str(run)[:200]}")
        # A run with one job in progress and the other queued is listed under queued, not
        # in progress (the module doc names two), so both lists are read, each run once.
        runs, seen = [], set()
        for status in ("in_progress", "queued"):
            for run in self.listed(f"/actions/workflows/{self.workflow}/runs?status={status}&per_page=100", "workflow_runs"):
                if run.get("id") not in seen:
                    seen.add(run.get("id"))
                    runs.append(run)
        for run in runs:
            if run.get("id") == self.run_id:
                continue
            jobs = self.jobs(run["id"])
            holder = next((j for j in jobs if j.get("status") == "in_progress" and labels(j) == labels(partner)), None)
            if holder is None:
                continue
            theirs = next((j for j in jobs if j.get("name") == me.get("name") and labels(j) == labels(me)), None)
            where = f"run {run['id']} ({run.get('html_url', 'no address')})"
            if holder.get("name") != partner.get("name") or theirs is None or started(theirs):
                self.say(f"{self.partner}'s host runs {holder.get('name')} of {where}")
                return
            if self.run_id > run["id"]:
                raise Fail(f"{self.partner} is queued, and its host runs {holder.get('name')} of {where}, which "
                           f"waits for its own {me.get('name')}, queued on this host behind this job: each "
                           f"end holds the host the other's partner needs. Run {run['id']} is the older, so this "
                           f"job ends to free this host for it, and this run's {self.partner} ends when it "
                           f"starts; {rerun_failed(self.run_id)}")
            self.say(f"{self.partner}'s host runs {holder.get('name')} of {where}, which waits for its own "
                     f"{me.get('name')}, queued behind this job; that run is the newer, so its end ends and "
                     f"this one keeps waiting")
            return
        self.say(f"no other run of this workflow is in progress on {self.partner}'s host; if that lasts, "
                 f"is its runner online?")

    def run(self):
        start = self.clock()
        next_look = start
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
                raise Fail(self.completed(partner, me))
            else:
                self.say(f"{self.partner} is {partner.get('status')} after {waited} s")
                if self.clock() >= next_look:
                    next_look = self.clock() + LOOK_EVERY
                    self.look_across(me, partner)
            if self.clock() - start >= BOUND:
                raise Fail(f"{self.partner} did not start within {BOUND // 60} minutes; is its host's runner "
                           f"online, or its queue that long? When it starts, it finds this job completed and "
                           f"ends too; {rerun_failed(self.run_id)}")
            self.sleep(POLL)

    def completed(self, partner, me):
        """Why a completed partner cannot meet this job, and the re-run that brings both ends
        back: `--failed` once both have failed, the whole run if the partner passed."""
        conclusion = partner.get("conclusion")
        if copied(partner, me):
            done = (f"{self.partner} completed ({conclusion}) at {partner['completed_at']}, before this job was "
                    f"created at {me['created_at']}, so it is an earlier attempt's result that this attempt "
                    f"kept: the attempt re-ran this end alone, and the pair cannot meet")
            if conclusion == "success":
                return f"{done}. {rerun_whole(self.run_id)}"
            return f"{done}; {rerun_failed(self.run_id)}"
        done = f"{self.partner} completed ({conclusion}) before this job reached it, so the pair cannot meet"
        if conclusion == "success":
            return f"{done}, and `--failed` would re-run this end alone. {rerun_whole(self.run_id)}"
        return f"{done}; {rerun_failed(self.run_id)}"

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
