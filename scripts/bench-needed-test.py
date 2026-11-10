#!/usr/bin/env python3
"""Tests of bench-needed.py: the list, path by path on the side it is on, and the reading
of a change from its event against a fake GitHub API on loopback, with no network and no
token. Also the join that keeps the script and ci.yml's bench jobs in step. Standard
library only; `just test-scripts` runs it.

    python3 scripts/bench-needed-test.py
"""
import http.server
import importlib.util
import json
import os
import re
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from urllib.parse import urlsplit

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
spec = importlib.util.spec_from_file_location("bench_needed", HERE / "bench-needed.py")
needed = importlib.util.module_from_spec(spec)
spec.loader.exec_module(needed)

FAKE_TOKEN = "ghs_fakeFAKEfake1234567890abcdef"
REPO = "example-owner/cavalry"
BEFORE, AFTER = "a" * 40, "b" * 40

# Every path of the first column cannot move the bench; every path of the second can.
CANNOT = [
    "docs/how-to/run-the-bench.md",
    "docs/decisions/0071-run-the-bench-jobs-only-for-a-change-that-can-move-the-bench.md",
    "docs/SUMMARY.md",
    "spec/client-protocol.md",
    "spec/vectors/frame.py",
    ".claude/agents/reviewer.md",
    ".claude/skills/pr/SKILL.md",
    ".github/workflows/claude.yml",
    ".github/workflows/review-pull-request.yml",
    ".github/dependabot.yml",
    "scripts/soak.sh",
    "scripts/claude-workflow-test.py",
    "scripts/bench-compare-test.py",
    "scripts/lint-docs.py",
    "clients/python/cavalry/__init__.py",
    "conformance/go/quickstart/main.go",
    "CHANGELOG.md",
    "CLAUDE.md",
    "README.md",
    "LICENSE",
]
CAN = [
    "crates/cavalry-shm/src/log/file.rs",
    "crates/cavalry-driver/src/conductor.rs",
    "crates/cavalry-client/Cargo.toml",
    "crates/cavalry-ffi/include/cavalry.h",
    "crates/cavalry-ffi/cbindgen.toml",
    "crates/cavalry-shm/src/counters/generated.rs",
    # Under `crates/`, so they end a tree as ADR 0033 defines one and run the bench.
    "crates/cavalry-client/tests/udp_receive.rs",
    "crates/cavalry/examples/soak.rs",
    "crates/cavalry-proto/fuzz/fuzz_targets/frame.rs",
    "crates/cavalry-client/README.md",
    "bench/src/main.rs",
    "bench/Cargo.toml",
    "bench/baseline.json",
    "bench/host/check.sh",
    "bench/README.md",
    "Cargo.toml",
    "Cargo.lock",
    "counters.toml",
    "config.toml",
    "justfile",
    "clippy.toml",
    "rust-toolchain.toml",
    ".gitignore",
    "book.toml",
    ".github/workflows/ci.yml",
    "scripts/bench-compare.py",
    "scripts/bench-needed.py",
    "scripts/bench-needed-test.py",
    "scripts/wait-for-partner-job.py",
    "scripts/wait-for-partner-job.sh",
    "a-directory-nobody-listed/file.rs",
    "docs.rs",
    "crates/README.md",
    "crates/cavalry-client/tests.rs",
]


class TheList(unittest.TestCase):
    def test_every_path_known_not_to_matter_cannot_move_the_bench_and_says_why(self):
        for path in CANNOT:
            with self.subTest(path):
                self.assertTrue(needed.cannot_move(path), path)

    def test_every_other_path_can_move_the_bench_a_path_nobody_listed_among_them(self):
        for path in CAN:
            with self.subTest(path):
                self.assertIsNone(needed.cannot_move(path), path)

    def test_the_scripts_the_bench_jobs_and_this_decision_run_are_named_and_exist(self):
        for path in needed.RUN:
            with self.subTest(path):
                self.assertTrue((ROOT / path).is_file(), f"{path} is on the list and not in the repository")
                self.assertIsNone(needed.cannot_move(path))

    def test_a_change_of_pages_alone_skips_the_bench_and_names_each_with_its_reason(self):
        runs, lines = needed.decide(["docs/glossary.md", "CHANGELOG.md", "spec/udp.md"])
        self.assertFalse(runs)
        text = "\n".join(lines)
        self.assertIn("none of the change's 3 files can move the bench", text)
        self.assertIn("docs/glossary.md: a page of the book or an ADR", text)
        self.assertIn("ADR 0071", text)

    def test_one_file_that_can_move_the_bench_among_pages_runs_it_and_is_named(self):
        runs, lines = needed.decide(["docs/glossary.md", "crates/cavalry-shm/src/lib.rs", "CHANGELOG.md"])
        self.assertTrue(runs)
        self.assertIn("1 of the change's 3 files can move the bench, so it runs: crates/cavalry-shm/src/lib.rs",
                      lines[0])

    def test_a_change_with_no_file_runs_the_bench(self):
        runs, lines = needed.decide([])
        self.assertTrue(runs)

    def test_a_long_change_names_ten_files_and_counts_the_rest(self):
        runs, lines = needed.decide([f"docs/page-{n}.md" for n in range(25)])
        self.assertFalse(runs)
        self.assertEqual(len(lines), 1 + needed.SHOWN + 1)
        self.assertIn("and 15 more", lines[-1])
        runs, lines = needed.decide([f"crates/x/src/{n}.rs" for n in range(25)])
        self.assertTrue(runs)
        self.assertIn("and 15 more", lines[0])


class FakeGitHub:
    """Serves a route by its path and query, records every request, and answers 404 to
    what it was not given. A route is a body, served with 200, or (status, body)."""

    def __init__(self):
        self.routes, self.seen = {}, []
        fake = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                fake.seen.append((self.path, dict(self.headers)))
                route = fake.routes.get(self.path)
                status, body = route if isinstance(route, tuple) else (200, route)
                if body is None:
                    status, body = 404, {"message": "Not Found"}
                payload = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *args):
                pass

        self.server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def paths(self):
        return [urlsplit(path).path for path, _ in self.seen]

    def close(self):
        self.server.shutdown()
        self.server.server_close()


def files(*names):
    return [{"filename": name, "status": "modified"} for name in names]


class Event(unittest.TestCase):
    def setUp(self):
        self.api = FakeGitHub()
        self.addCleanup(self.api.close)
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.output = Path(self.tmp.name) / "output"

    def run_event(self, name, payload, **env):
        """Runs the script for an event: (the answer written to GITHUB_OUTPUT, what it printed)."""
        event = Path(self.tmp.name) / "event.json"
        event.write_text(json.dumps(payload), encoding="utf-8")
        self.output.write_text("", encoding="utf-8")
        environment = {"GH_TOKEN": FAKE_TOKEN, "GITHUB_API_URL": self.api.url, "GITHUB_REPOSITORY": REPO,
                       "GITHUB_EVENT_NAME": name, "GITHUB_EVENT_PATH": str(event),
                       "GITHUB_OUTPUT": str(self.output), **env}
        environment = {key: value for key, value in environment.items() if value is not None}
        printed = []
        self.assertEqual(needed.main([], environment, printed.append), 0)
        text = "\n".join(printed)
        self.assertNotIn(FAKE_TOKEN, text)
        return self.output.read_text(encoding="utf-8").strip(), text

    def pull(self, number, *pages):
        for page, listed in enumerate(pages, start=1):
            self.api.routes[f"/repos/{REPO}/pulls/{number}/files?per_page=100&page={page}"] = listed

    def compare(self, body, before=BEFORE, after=AFTER):
        self.api.routes[f"/repos/{REPO}/compare/{before}...{after}?per_page=100"] = body

    def runs_of(self, sha, *runs):
        """ci.yml's runs for `sha`: each run is (event, branch, status, conclusion)."""
        self.api.routes[f"/repos/{REPO}/actions/workflows/ci.yml/runs?head_sha={sha}&per_page=100"] = {
            "workflow_runs": [{"event": event, "head_branch": branch, "status": status, "conclusion": conclusion}
                              for event, branch, status, conclusion in runs]}


class PullRequest(Event):
    def test_a_pull_request_of_pages_alone_answers_false(self):
        self.pull(178, files("docs/how-to/set-up-a-soak-host.md", "scripts/soak.sh", "CHANGELOG.md"))
        answer, text = self.run_event("pull_request", {"number": 178})
        self.assertEqual(answer, "bench=false")
        self.assertIn("none of the change's 3 files can move the bench", text)

    def test_a_pull_request_with_one_source_file_answers_true(self):
        self.pull(174, files("docs/SUMMARY.md", "crates/cavalry-driver/src/conductor.rs"))
        answer, text = self.run_event("pull_request", {"number": 174})
        self.assertEqual(answer, "bench=true")
        self.assertIn("crates/cavalry-driver/src/conductor.rs", text)

    def test_every_page_of_a_long_pull_request_is_read(self):
        first = files(*[f"docs/page-{n}.md" for n in range(100)])
        self.pull(9, first, files("docs/last.md", "bench/src/main.rs"))
        answer, _ = self.run_event("pull_request", {"number": 9})
        self.assertEqual(answer, "bench=true", "the file that can move the bench is on the second page")
        self.assertEqual(len(self.api.seen), 2)

    def test_a_file_moved_out_of_a_crates_source_counts_under_its_old_name(self):
        self.pull(5, [{"filename": "docs/notes.md", "previous_filename": "crates/cavalry-shm/src/notes.rs",
                       "status": "renamed"}])
        answer, text = self.run_event("pull_request", {"number": 5})
        self.assertEqual(answer, "bench=true")
        self.assertIn("crates/cavalry-shm/src/notes.rs", text)

    def test_a_pull_request_as_long_as_the_api_lists_runs_the_bench(self):
        full = files(*[f"docs/page-{n}.md" for n in range(100)])
        self.pull(3, *[full] * (needed.PULL_FILES_MOST // needed.PER_PAGE))
        answer, text = self.run_event("pull_request", {"number": 3})
        self.assertEqual(answer, "bench=true")
        self.assertIn("as many as the API returns", text)

    def test_a_listing_the_api_refuses_runs_the_bench(self):
        self.api.routes[f"/repos/{REPO}/pulls/7/files?per_page=100&page=1"] = (403, {"message": "Forbidden"})
        answer, text = self.run_event("pull_request", {"number": 7})
        self.assertEqual(answer, "bench=true")
        self.assertIn("HTTP 403", text)

    def test_the_token_goes_in_a_header_and_nowhere_else(self):
        self.pull(2, files("docs/a.md"))
        self.run_event("pull_request", {"number": 2})
        path, headers = self.api.seen[0]
        self.assertEqual(headers.get("Authorization"), f"Bearer {FAKE_TOKEN}")
        self.assertNotIn(FAKE_TOKEN, path)


GREEN = ("push", "master", "completed", "success")
PUSH = {"before": BEFORE, "after": AFTER, "ref": "refs/heads/master"}


class Push(Event):
    def test_a_push_of_pages_alone_on_a_measured_commit_answers_false(self):
        self.compare({"status": "ahead", "files": files("docs/decisions/0061.md", "docs/SUMMARY.md")})
        self.runs_of(BEFORE, GREEN)
        answer, _ = self.run_event("push", PUSH)
        self.assertEqual(answer, "bench=false")

    def test_a_push_of_several_commits_is_read_whole_from_before_to_after(self):
        # The last commit of the push changed a page; an earlier one changed a crate.
        self.compare({"status": "ahead", "ahead_by": 3,
                      "files": files("docs/glossary.md", "crates/cavalry-log/src/recorder.rs")})
        self.runs_of(BEFORE, GREEN)
        answer, text = self.run_event("push", PUSH)
        self.assertEqual(answer, "bench=true")
        self.assertEqual(self.api.paths(), [f"/repos/{REPO}/compare/{BEFORE}...{AFTER}"])

    def test_pages_pushed_onto_a_commit_whose_run_was_replaced_while_it_waited_run_the_bench(self):
        # Runs of one branch go one at a time with one waiting; a newer push takes the
        # waiting run's place, and the replaced run reads cancelled. The commit before
        # this push changed a crate and was never measured on this branch.
        self.compare({"status": "ahead", "files": files("docs/glossary.md")})
        self.runs_of(BEFORE, ("push", "master", "completed", "cancelled"))
        answer, text = self.run_event("push", PUSH)
        self.assertEqual(answer, "bench=true")
        self.assertIn("has no completed, successful run", text)
        self.assertIn(BEFORE[:7], text)

    def test_pages_pushed_onto_a_commit_whose_bench_failed_run_the_bench(self):
        self.compare({"status": "ahead", "files": files("CHANGELOG.md")})
        self.runs_of(BEFORE, ("push", "master", "completed", "failure"))
        answer, _ = self.run_event("push", PUSH)
        self.assertEqual(answer, "bench=true")

    def test_pages_pushed_onto_a_commit_with_no_run_or_one_still_running_run_the_bench(self):
        self.compare({"status": "ahead", "files": files("CHANGELOG.md")})
        for runs in ((), (("push", "master", "in_progress", None),), (("push", "master", "queued", None),)):
            with self.subTest(runs=runs):
                self.runs_of(BEFORE, *runs)
                answer, _ = self.run_event("push", PUSH)
                self.assertEqual(answer, "bench=true")

    def test_a_green_run_of_the_commit_before_counts_only_on_this_branch_and_from_a_push_or_by_hand(self):
        self.compare({"status": "ahead", "files": files("CHANGELOG.md")})
        for run, answer in ((("pull_request", "master", "completed", "success"), "bench=true"),
                            (("push", "another-branch", "completed", "success"), "bench=true"),
                            (("workflow_dispatch", "master", "completed", "success"), "bench=false")):
            with self.subTest(run=run):
                self.runs_of(BEFORE, run)
                self.assertEqual(self.run_event("push", PUSH)[0], answer)

    def test_a_green_run_among_others_of_the_commit_before_is_enough(self):
        self.compare({"status": "ahead", "files": files("CHANGELOG.md")})
        self.runs_of(BEFORE, ("push", "master", "completed", "cancelled"), GREEN)
        self.assertEqual(self.run_event("push", PUSH)[0], "bench=false")

    def test_a_push_that_names_no_branch_or_whose_runs_cannot_be_listed_runs_the_bench(self):
        self.compare({"status": "ahead", "files": files("CHANGELOG.md")})
        self.runs_of(BEFORE, GREEN)
        answer, _ = self.run_event("push", {"before": BEFORE, "after": AFTER, "ref": "refs/tags/v1"})
        self.assertEqual(answer, "bench=true")
        del self.api.routes[f"/repos/{REPO}/actions/workflows/ci.yml/runs?head_sha={BEFORE}&per_page=100"]
        answer, text = self.run_event("push", PUSH)
        self.assertEqual(answer, "bench=true")
        self.assertIn("HTTP 404", text)

    def test_a_push_with_no_commit_before_it_runs_the_bench_and_asks_nothing(self):
        answer, text = self.run_event("push", {"before": "0" * 40, "after": AFTER, "ref": "refs/heads/master"})
        self.assertEqual(answer, "bench=true")
        self.assertIn("no commit before it", text)
        self.assertEqual(self.api.seen, [])

    def test_a_forced_push_runs_the_bench(self):
        self.compare({"status": "diverged", "files": files("docs/a.md")})
        self.runs_of(BEFORE, GREEN)
        answer, text = self.run_event("push", PUSH)
        self.assertEqual(answer, "bench=true")
        self.assertIn("diverged", text)

    def test_a_push_as_long_as_the_api_lists_runs_the_bench(self):
        self.compare({"status": "ahead",
                      "files": files(*[f"docs/page-{n}.md" for n in range(needed.COMPARE_FILES_MOST)])})
        self.runs_of(BEFORE, GREEN)
        answer, _ = self.run_event("push", PUSH)
        self.assertEqual(answer, "bench=true")

    def test_a_comparison_the_api_does_not_have_runs_the_bench(self):
        answer, text = self.run_event("push", PUSH)
        self.assertEqual(answer, "bench=true")
        self.assertIn("HTTP 404", text)


class Otherwise(Event):
    def test_a_run_by_hand_runs_the_bench_whatever_changed(self):
        answer, text = self.run_event("workflow_dispatch", {})
        self.assertEqual(answer, "bench=true")
        self.assertIn("workflow_dispatch", text)
        self.assertEqual(self.api.seen, [])

    def test_a_missing_token_an_unsafe_url_or_an_unread_event_runs_the_bench(self):
        for env in ({"GH_TOKEN": None}, {"GITHUB_API_URL": "http://example.com"},
                    {"GITHUB_EVENT_PATH": "/nonexistent/event.json"}):
            with self.subTest(env=env):
                answer, text = self.run_event("pull_request", {"number": 1}, **env)
                self.assertEqual(answer, "bench=true")
                self.assertIn("the bench runs, since a skip needs", text)

    def test_files_on_the_command_line_decide_without_an_event(self):
        printed = []
        self.assertEqual(needed.main(["--files", "docs/a.md", "README.md"], {}, printed.append), 0)
        self.assertEqual(printed[-1], "bench=false")
        printed = []
        self.assertEqual(needed.main(["--files", "bench/src/main.rs"], {}, printed.append), 0)
        self.assertEqual(printed[-1], "bench=true")


class Workflow(unittest.TestCase):
    """ci.yml and the script agree: the job that decides, and the four that read it."""

    BENCH = {"bench": "bench (linux)", "bench-grip": "bench (grip)", "bench-udp": "bench (udp)",
             "bench-udp-peer": "bench (udp peer)"}

    @classmethod
    def setUpClass(cls):
        text = (ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
        jobs = text[text.index("\njobs:\n") + len("\njobs:\n"):]
        cls.jobs = {}
        for block in re.split(r"(?m)^(?=  [a-z][a-z-]*:\n)", jobs):
            name = re.match(r"  ([a-z][a-z-]*):\n", block)
            if name:
                cls.jobs[name.group(1)] = block

    def test_the_changes_job_runs_the_script_with_the_jobs_token_and_publishes_its_answer(self):
        block = self.jobs["changes"]
        self.assertIn("run: python3 -I scripts/bench-needed.py", block)
        self.assertIn("GH_TOKEN: ${{ github.token }}", block)
        self.assertIn("bench: ${{ steps.decide.outputs.bench }}", block)
        self.assertRegex(block, r"\n      pull-requests: read\b")
        self.assertNotIn("self-hosted", block)

    def test_each_bench_job_keeps_its_name_and_takes_its_host_unless_the_answer_is_false(self):
        for job, name in self.BENCH.items():
            with self.subTest(job):
                block = self.jobs[job]
                self.assertIn(f"    name: {name}\n", block)
                self.assertRegex(block, r"\n    needs: (changes|\[changes, bench, bench-grip\])\n")
                self.assertIn("needs.changes.outputs.bench == 'false' && 'blacksmith-", block)
                self.assertIn("|| fromJSON('[\"self-hosted\", ", block)
                self.assertIn("      SKIP: ${{ needs.changes.outputs.bench == 'false' }}\n", block)
                # It runs when `changes` failed too, which reads as "run on the host".
                self.assertRegex(block, r"\n    if: \$\{\{ !cancelled\(\) && ")

    def test_every_step_of_a_bench_job_is_skipped_when_the_job_reports_without_its_host(self):
        for job in self.BENCH:
            block = self.jobs[job]
            steps = re.split(r"(?m)^      - ", block[block.index("\n    steps:\n"):])[1:]
            self.assertGreater(len(steps), 5, job)
            first, rest = steps[0], steps[1:]
            with self.subTest(job, step="the note"):
                self.assertIn("if: env.SKIP == 'true'", first)
            for step in rest:
                with self.subTest(job, step=step.split("\n")[0][:50]):
                    self.assertRegex(step, r"\n        if: (env\.SKIP != 'true'|\$\{\{ [^}]*env\.SKIP != 'true'[^}]*\}\})\n")

    def test_every_script_the_bench_path_names_is_on_the_side_that_runs_the_bench(self):
        # `scripts/` is the one directory whose rule is "cannot move the bench" with the
        # exceptions named, so a script the bench comes to run must be named: the bench
        # jobs and the deciding job of ci.yml, the bench recipes of the justfile, and
        # the host's check and set-up scripts.
        named = set()
        for job in ("changes", *self.BENCH):
            named |= set(re.findall(r"scripts/[\w.-]+", self.jobs[job]))
        justfile = (ROOT / "justfile").read_text(encoding="utf-8")
        for recipe in ("bench", "bench-unchecked", "bench-compare"):
            body = re.search(rf"(?m)^{recipe}(?: [^\n]*)?:[^\n]*\n((?:    [^\n]*\n)+)", justfile)
            self.assertIsNotNone(body, recipe)
            named |= set(re.findall(r"scripts/[\w.-]+", body.group(1)))
        for script in sorted((ROOT / "bench" / "host").glob("*.sh")):
            named |= set(re.findall(r"scripts/[\w.-]+", script.read_text(encoding="utf-8")))
        self.assertIn("scripts/bench-needed.py", named)
        self.assertIn("scripts/bench-compare.py", named)
        self.assertIn("scripts/wait-for-partner-job.sh", named)
        for path in sorted(named):
            with self.subTest(path):
                self.assertIn(path, needed.RUN, f"{path} is run on the bench path and a change to it would skip the bench")

    def test_the_changes_job_may_read_the_run_of_the_commit_before_a_push(self):
        self.assertRegex(self.jobs["changes"], r"\n      actions: read\b")

    def test_the_pair_still_waits_on_both_single_host_jobs_having_passed(self):
        for job in ("bench-udp", "bench-udp-peer"):
            with self.subTest(job):
                self.assertIn("needs.bench.result == 'success' && needs.bench-grip.result == 'success'",
                              self.jobs[job])


if __name__ == "__main__":
    unittest.main()
