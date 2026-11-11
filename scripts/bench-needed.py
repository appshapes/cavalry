#!/usr/bin/env python3
"""bench-needed: can this change move the bench? (ADR 0071; card 246)

    python3 -I scripts/bench-needed.py            in ci.yml's `changes` job
    python3 -I scripts/bench-needed.py --files crates/cavalry-shm/src/lib.rs docs/glossary.md

The latency gate runs on two hosts that take one job at a time (ADR 0016, ADR 0034),
and a run holds them for about 21 minutes between its three bench jobs. A change to a
page, an ADR, a workflow other than CI's, a script or a client's generated code
changes no instruction the harness runs, so its run on the hosts measures the tree
its base already measured and holds the hosts from a change that could move them: 22
of master's 80 commits up to cafffc7 were of that kind. This script reads the
change's files and says whether any of them can move the bench; ci.yml's bench jobs
run on their hosts when one can, and report on a hosted runner in seconds when none
can.

The list is of what cannot move the bench, [`CANNOT`], so that a path nobody listed
runs it. What can, by that rule: everything under `crates/` and `bench/`, the
workspace's manifest and lock, `counters.toml` and `config.toml`, which generate
sources, the `justfile`, `clippy.toml`, `ci.yml`, and the scripts the bench jobs and
this decision run, [`RUN`], which are named so that the rule for `scripts/` does not
take them: a change to the list is judged by running the bench. The list names no
path that ends a tree as ADR 0033 defines one (`crates/`, `bench/src`, `bench/host`,
`bench/Cargo.toml`, `Cargo.toml`, `Cargo.lock`, `justfile`), so a change that reports
without the hosts is at its base's tree, whose runs are that tree's processes; a
crate's tests and README are under `crates/` and run the bench for that reason, not
because they can move it.

The files come from the event (`GITHUB_EVENT_NAME`, `GITHUB_EVENT_PATH`):

- `pull_request`: the pull request's files against its base, from
  `pulls/<number>/files`, every page. A renamed file counts under both of its names.
- `push`: the files between the event's `before` and `after`, from
  `compare/<before>...<after>`, so that a push of several commits is read whole and
  not from its last commit alone. And the commit before must have been measured:
  the bench is skipped only when ci.yml has a run for `before` on the same branch,
  by a push or by hand, that is completed with success. Runs of one branch go one at
  a time with one waiting, and a newer push takes the waiting run's place, so a
  commit under `crates/` whose run was replaced that way, or whose bench failed, has
  no such run, and the push after it runs the bench over both, pages or not. A push
  that was itself skipped passed that test, so the last run on the hosts before any
  skipped push is green and nothing since could move the bench.
- anything else, `workflow_dispatch` above all: the bench runs. That is how a
  baseline's processes, or a look at a host, are had at a tree whose last change was
  a page.

A push that can move the bench has one way left to report without the hosts (ADR
0074): its tree was measured already. That is so when all of this holds:

- the pushed commit is the merge commit of one merged pull request, from a branch of
  this repository into the pushed branch;
- the commit before the push is in the pull request's head, so that a run of that
  head was of the head's own tree and not of a merge with a newer base, and the
  head's tree is the pushed commit's;
- no pull request from the same branch has another base, whose run at this head
  would have been of another tree;
- `ci.yml` has a run for that head, by a pull request event from that branch, in
  which the bench jobs of [`HOST_JOBS`] each passed on a host. The run need not have
  ended: a merge is made the moment the required checks pass, and `master`'s run
  asks seconds later, while the pull request's run may still be finishing a job that
  is not required.

A landing then takes the hosts once, for the pull request, and not again for the
same tree on `master`. The answer names the pull request and the run: the next
change's "before" figures are that run's.

It answers "the bench runs" whenever it cannot list the change: a push with no commit
before it, a comparison whose two commits have diverged, a listing as long as the API
returns at most (3,000 files of a pull request, 300 of a comparison), a request the API
refuses or does not answer. A skip needs every file read and every one on the list,
and on a push a green run for the commit before.

It prints what it read and why, writes `bench=true` or `bench=false` to
`GITHUB_OUTPUT` when that is set, and exits 0 either way: the answer is the output,
and a failure to decide is an answer of true. With `--files` it reads no event and
no API and prints the decision for the paths given, for a person or a session.

Environment, for the event's two forms that read the API:
  GH_TOKEN           the job's token; the step sets it from github.token. Sent as a
                     header, never on argv, never printed
  GITHUB_API_URL     Actions sets it; https only, loopback http for the tests
  GITHUB_REPOSITORY  Actions sets it

Standard library only; scripts/bench-needed-test.py tests it against a fake API on
loopback, and `just test-scripts` runs that.
"""
import json
import os
import re
import sys
import urllib.error
import urllib.request

TIMEOUT = 20            # s for one request
PER_PAGE = 100
PULL_FILES_MOST = 3000  # what `pulls/<n>/files` lists at most
COMPARE_FILES_MOST = 300  # what `compare/<a>...<b>` lists at most
SHOWN = 10              # files named in the answer
DOCS = "docs/how-to/run-the-bench.md, \"When the bench jobs run\""
# The bench jobs whose pass on a host is the proof that a tree was measured, by the
# names ci.yml gives them, and the label a job on a host runs under. `bench (udp
# peer)` is the other end of `bench (udp)` and fails with it.
HOST_JOBS = ("bench (linux)", "bench (grip)", "bench (udp)")
HOST_LABEL = "self-hosted"
RUNS_READ = 5           # runs of a head whose jobs are read, newest first

# The scripts the bench jobs and this decision run, by name, before the rule that the
# rest of `scripts/` cannot move the bench.
RUN = frozenset({
    ".github/workflows/ci.yml",
    "scripts/bench-compare.py",
    "scripts/bench-needed.py",
    "scripts/bench-needed-test.py",
    "scripts/wait-for-partner-job.py",
    "scripts/wait-for-partner-job.sh",
})

# What cannot move the bench: a pattern a path must match in full, and why.
CANNOT = (
    (r"docs/.*", "a page of the book or an ADR"),
    (r"spec/.*", "a spec page or its vectors, which the tests read and the harness does not"),
    (r"\.claude/.*", "an agent's brief, a skill or a rule"),
    (r"\.github/.*", "a workflow other than ci.yml, or the repository's settings"),
    (r"scripts/.*", "a script the bench jobs do not run"),
    (r"clients/.*", "a client generated over the C ABI"),
    (r"conformance/.*", "the conformance suite"),
    (r"[^/]+\.md", "the root's markdown"),
    (r"LICENSE", "the licence"),
)


def cannot_move(path):
    """Why `path` cannot move the bench, or None when it can or nobody has said."""
    if path in RUN:
        return None
    for pattern, why in CANNOT:
        if re.fullmatch(pattern, path):
            return why
    return None


def decide(paths):
    """(runs, lines): whether the bench runs for a change of `paths`, and what to print."""
    paths = sorted(set(paths))
    if not paths:
        return True, ["the change lists no file, which is not a change this can read: the bench runs"]
    movers = [path for path in paths if cannot_move(path) is None]
    if movers:
        shown = ", ".join(movers[:SHOWN]) + (f" and {len(movers) - SHOWN} more" if len(movers) > SHOWN else "")
        return True, [f"{len(movers)} of the change's {len(paths)} files can move the bench, so it runs: {shown}"]
    lines = [f"none of the change's {len(paths)} files can move the bench, so the bench jobs report without "
             f"their hosts (ADR 0071; {DOCS}):"]
    lines += [f"  {path}: {cannot_move(path)}" for path in paths[:SHOWN]]
    if len(paths) > SHOWN:
        lines.append(f"  and {len(paths) - SHOWN} more, each on the list in scripts/bench-needed.py")
    return False, lines


class Unread(Exception):
    """The change could not be listed: why, in words for the log."""


class Api:
    """GET against the repository with the job's token, and nothing else."""

    def __init__(self, env):
        for name in ("GH_TOKEN", "GITHUB_API_URL", "GITHUB_REPOSITORY"):
            if not env.get(name):
                raise Unread(f"{name} is not set")
        self.base = f"{env['GITHUB_API_URL'].rstrip('/')}/repos/{env['GITHUB_REPOSITORY']}"
        self.owner = env["GITHUB_REPOSITORY"].split("/")[0]
        self.repository = env["GITHUB_REPOSITORY"]
        self._token = env["GH_TOKEN"]
        # https only: the job token rides on every request. Loopback is the exception the tests run on.
        if not (self.base.startswith("https://") or re.match(r"^http://(127\.0\.0\.1|localhost)(:\d+)?/", self.base)):
            raise Unread(f"GITHUB_API_URL must use https: {env['GITHUB_API_URL']}")

    def get(self, path):
        headers = {"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28",
                   "Authorization": f"Bearer {self._token}"}
        request = urllib.request.Request(self.base + path, headers=headers)
        where = path.split("?")[0]
        try:
            with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
                return json.loads(response.read().decode("utf-8"))
        except urllib.error.HTTPError as e:
            e.close()
            raise Unread(f"GET {where} answered HTTP {e.code}") from None
        except (urllib.error.URLError, OSError, ValueError) as e:
            raise Unread(f"GET {where} was not answered: {getattr(e, 'reason', e)}") from None


def names(files):
    """Every name a listing's entries carry: a renamed file's old name beside its new one."""
    found = []
    for entry in files:
        if not isinstance(entry, dict) or not isinstance(entry.get("filename"), str):
            raise Unread("a listing's entry has no filename")
        found.append(entry["filename"])
        if isinstance(entry.get("previous_filename"), str):
            found.append(entry["previous_filename"])
    return found


def pull_request_files(api, number):
    files = []
    for page in range(1, PULL_FILES_MOST // PER_PAGE + 1):
        listed = api.get(f"/pulls/{number}/files?per_page={PER_PAGE}&page={page}")
        if not isinstance(listed, list):
            raise Unread(f"pulls/{number}/files did not answer with a list")
        files += listed
        if len(listed) < PER_PAGE:
            return names(files)
    raise Unread(f"pull request {number} lists {PULL_FILES_MOST} files, as many as the API returns, "
                 f"so there may be more")


def push_files(api, before, after):
    before, after = sha(before, "the push event's before"), sha(after, "the push event's after")
    if set(before) == {"0"}:
        raise Unread("the push has no commit before it, a new branch")
    compared = api.get(f"/compare/{before}...{after}?per_page={PER_PAGE}")
    if not isinstance(compared, dict) or compared.get("status") != "ahead":
        status = compared.get("status") if isinstance(compared, dict) else None
        raise Unread(f"the push's two commits are not one ahead of the other ({status}), a forced push")
    files = compared.get("files")
    if not isinstance(files, list):
        raise Unread("the comparison lists no files")
    if len(files) >= COMPARE_FILES_MOST:
        raise Unread(f"the push lists {len(files)} files, as many as the API returns, so there may be more")
    return names(files)


def sha(value, what):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{40}", value):
        raise Unread(f"{what} names no commit")
    return value


def branch_of(ref):
    if not isinstance(ref, str) or not ref.startswith("refs/heads/"):
        raise Unread("the push event names no branch")
    return ref[len("refs/heads/"):]


def ci_runs(api, commit, what):
    """ci.yml's runs for a commit, as the API lists them."""
    listed = api.get(f"/actions/workflows/ci.yml/runs?head_sha={commit}&per_page={PER_PAGE}")
    runs = listed.get("workflow_runs") if isinstance(listed, dict) else None
    if not isinstance(runs, list):
        raise Unread(f"the runs of {what} could not be listed")
    return [run for run in runs if isinstance(run, dict)]


def measured(api, before, ref):
    """Whether ci.yml has a completed, successful run for `before` on the push's branch,
    started by a push or by hand: the commit the push stands on was measured and passed,
    by its own run on the hosts or, for a push that was skipped, by this same test."""
    branch = branch_of(ref)
    return any(run.get("head_branch") == branch and run.get("event") in ("push", "workflow_dispatch")
               and run.get("status") == "completed" and run.get("conclusion") == "success"
               for run in ci_runs(api, before, "the commit before the push"))


def tree_of(api, commit):
    listed = api.get(f"/git/commits/{commit}")
    return sha((listed.get("tree") or {}).get("sha") if isinstance(listed, dict) else None,
               f"commit {commit[:7]}'s tree")


def not_on_a_host(api, run):
    """The first of the bench jobs that did not pass on a host in a run, or None."""
    listed = api.get(f"/actions/runs/{run}/jobs?filter=latest&per_page={PER_PAGE}")
    jobs = listed.get("jobs") if isinstance(listed, dict) else None
    if not isinstance(jobs, list):
        raise Unread(f"the jobs of run {run} could not be listed")
    for name in HOST_JOBS:
        if not any(isinstance(job, dict) and job.get("name") == name and job.get("conclusion") == "success"
                   and isinstance(job.get("labels"), list) and HOST_LABEL in job["labels"] for job in jobs):
            return name
    return None


def measured_as(api, before, after, ref):
    """(number, head, run): the pull request whose run measured the pushed commit's
    tree on the hosts, its head and that run's id. Unread, in words for the log, when
    the push is not such a merge: the module's text has each condition."""
    branch = branch_of(ref)
    pulls = api.get(f"/commits/{after}/pulls?per_page={PER_PAGE}")
    if not isinstance(pulls, list):
        raise Unread("the pull requests of the pushed commit could not be listed")
    merged = [pull for pull in pulls if isinstance(pull, dict) and pull.get("merge_commit_sha") == after
              and pull.get("merged_at") and (pull.get("base") or {}).get("ref") == branch]
    if len(merged) != 1:
        raise Unread(f"the pushed commit is the merge commit of {len(merged)} pull requests into {branch}, not of one")
    number, source = merged[0].get("number"), merged[0].get("head") or {}
    if not isinstance(number, int):
        raise Unread("the pull request of the pushed commit has no number")
    head, source_branch = sha(source.get("sha"), f"pull request {number}'s head"), source.get("ref")
    if (source.get("repo") or {}).get("full_name") != api.repository or not isinstance(source_branch, str):
        raise Unread(f"pull request {number} is not from a branch of this repository")
    # A pull request's run is of its head merged with its base as the base stood then.
    # With the commit before the push in the head, every such base was in the head
    # too, and the run was of the head's own tree.
    compared = api.get(f"/compare/{before}...{head}?per_page=1")
    status = compared.get("status") if isinstance(compared, dict) else None
    if status not in ("ahead", "identical"):
        raise Unread(f"the commit before the push, {before[:7]}, is not in pull request {number}'s head "
                     f"({status}), so the head's run may have been of another tree")
    if tree_of(api, after) != tree_of(api, head):
        raise Unread(f"pull request {number}'s head {head[:7]} has another tree than the pushed commit")
    # The runs of a head do not say which pull request they were for once it has
    # merged, so no pull request from this branch may have had another base.
    siblings = api.get(f"/pulls?state=all&head={api.owner}:{source_branch}&per_page={PER_PAGE}")
    if not isinstance(siblings, list) or len(siblings) >= PER_PAGE:
        raise Unread(f"the pull requests from {source_branch} could not be listed whole")
    if any(not isinstance(pull, dict) or (pull.get("base") or {}).get("ref") != branch for pull in siblings):
        raise Unread(f"{source_branch} has a pull request into another branch than {branch}, whose run at this "
                     f"head would have been of another tree")
    runs = sorted((run["id"] for run in ci_runs(api, head, f"pull request {number}'s head")
                   if run.get("event") == "pull_request" and run.get("head_branch") == source_branch
                   and isinstance(run.get("id"), int)), reverse=True)
    if not runs:
        raise Unread(f"pull request {number}'s head {head[:7]} has no run of ci.yml by a pull request event "
                     f"from {source_branch}")
    lacking = []
    for run in runs[:RUNS_READ]:
        name = not_on_a_host(api, run)
        if name is None:
            return number, head, run
        lacking.append(f"run {run} has no `{name}` that passed on its host")
    raise Unread(f"no run of pull request {number}'s head {head[:7]} passed its bench jobs on the hosts: "
                 + "; ".join(lacking))


def changed_files(env):
    """(files, base): the files of the event's change, and for a push what `measured`
    and `measured_as` need, the API, the commits before and after and the branch, else
    None. Unread when the event is one this does not list."""
    event = env.get("GITHUB_EVENT_NAME", "")
    if event not in ("pull_request", "push"):
        raise Unread(f"the event is {event or 'not named'}, which runs the bench whatever changed")
    try:
        with open(env.get("GITHUB_EVENT_PATH", ""), encoding="utf-8") as f:
            payload = json.load(f)
    except (OSError, ValueError) as e:
        raise Unread(f"the event's payload could not be read: {e}") from None
    api = Api(env)
    if event == "pull_request":
        number = payload.get("number") or (payload.get("pull_request") or {}).get("number")
        if not isinstance(number, int):
            raise Unread("the pull request event names no number")
        return pull_request_files(api, number), None
    before, after = payload.get("before"), payload.get("after")
    return push_files(api, before, after), (api, before, after, payload.get("ref"))


def main(argv, env, out=print):
    if argv[:1] == ["--files"]:
        runs, lines = decide(argv[1:])
        for line in lines:
            out(f"bench-needed: {line}")
        out(f"bench={'true' if runs else 'false'}")
        return 0
    if argv:
        out("usage: bench-needed.py [--files <path>...]")
        return 2
    try:
        files, base = changed_files(env)
        runs, lines = decide(files)
        if base is not None:
            api, before, after, ref = base
            # Asked only when the files alone would skip the bench: a push that runs it
            # measures the commit before it as well, whatever became of that commit's run.
            if not runs and not measured(api, before, ref):
                raise Unread(f"the commit before the push, {before[:7]}, has no completed, successful run of "
                             f"ci.yml on its branch: its run was replaced while it waited, failed or is not "
                             f"there, so this run measures both")
            # A push that can move the bench does not, when its tree was measured
            # already by the pull request it merged (ADR 0074).
            if runs:
                try:
                    number, head, run = measured_as(api, before, after, ref)
                except Unread as why:
                    lines.append(f"and no pull request's run measured this tree on the hosts: {why}")
                else:
                    runs = False
                    lines = [f"the push is the merge of pull request {number}, whose head {head[:7]} has this "
                             f"commit's tree and whose run {run} passed its bench jobs on the hosts, so the "
                             f"bench jobs report without their hosts (ADR 0074; {DOCS}): the next change's "
                             f"\"before\" figures are run {run}'s"]
    except Unread as why:
        runs, lines = True, [f"the bench runs, since a skip needs every file read and the base measured: {why}"]
    for line in lines:
        out(f"bench-needed: {line}")
    answer = f"bench={'true' if runs else 'false'}"
    out(answer)
    if env.get("GITHUB_OUTPUT"):
        with open(env["GITHUB_OUTPUT"], "a", encoding="utf-8") as f:
            f.write(answer + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:], os.environ))
