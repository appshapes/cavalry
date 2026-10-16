#!/usr/bin/env python3
"""Tests of release-notes.py against a made-up checkout and a fake GitHub API on loopback: no
network, no token, no agent. Also the joins that keep the script, the workflow file, the brief
and the sample draft in step. Standard library only; `just test-scripts` runs it.

    python3 scripts/release-notes-test.py
"""
import datetime
import http.server
import importlib.util
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from urllib.parse import urlsplit

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
SCRIPT = HERE / "release-notes.py"
SAMPLE = (HERE / "release-notes-email-sample.md").read_text(encoding="utf-8")
WORKFLOW = ROOT / ".github" / "workflows"
BRIEF = ROOT / ".claude" / "agents" / "release-notes-email.md"
spec = importlib.util.spec_from_file_location("release_notes", SCRIPT)
rn = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rn)

FAKE_TOKEN = "ghs_fakeFAKEfake1234567890abcdef"
REPO = "example-owner/cavalry"
SERVER = "https://github.example"
REPO_URL = f"{SERVER}/{REPO}"
WORKFLOW_PATH = f".github/workflows/{rn.WORKFLOW_FILE}"


class FakeGitHub:
    """Serves canned JSON by path, the query dropped, and records every request."""

    def __init__(self):
        self.routes, self.seen = {}, []
        fake = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                fake.seen.append((self.path, dict(self.headers)))
                body = fake.routes.get(urlsplit(self.path).path)
                payload = json.dumps(body if body is not None else {"message": "Not Found"}).encode()
                self.send_response(200 if body is not None else 404)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *args):
                pass

        self.server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def asked(self, prefix):
        return any(path.startswith(prefix) for path, _ in self.seen)

    def close(self):
        self.server.shutdown()
        self.server.server_close()


def run_data(id_, created, head_sha, head_branch="master", conclusion="success", path=WORKFLOW_PATH):
    return {"id": id_, "created_at": created, "head_sha": head_sha, "head_branch": head_branch,
            "conclusion": conclusion, "path": path}


def jobs_data(send_conclusion):
    return {"jobs": [{"name": "email", "steps": [{"name": "Draft the email", "conclusion": "success"},
                                                  {"name": rn.SEND_STEP, "conclusion": send_conclusion}]}]}


class Repo:
    """A made-up checkout: a justfile, the binary's usage, the registries, a page or two, and a
    history whose commit dates are set. Every date is in January 2026, outside any seven-day
    window these tests will ever run in."""

    def __init__(self, path):
        self.path = Path(path)
        self.path.mkdir(parents=True)
        self.git("init", "-q", "--initial-branch=master")
        self.git("config", "user.name", "Tester")
        self.git("config", "user.email", "tester@example.invalid")
        self.write("justfile", "set shell := [\"bash\", \"-c\"]\n\ntest *ARGS: test-scripts\n    cargo test\n\nbench *ARGS:\n    cargo bench\n\nalias t := test\n")
        self.write("crates/cavalry/src/main.rs", 'const USAGE: &str = "usage: cavalry <command>\n\n  run       start a driver\n  doctor    explain the driver\n  --version print the version\n";\n')
        self.write("config.toml", 'schema = 0\n\n[[key]]\nname = "dir"\n\n[[key]]\nname = "conductor.idle.strategy"\n')
        self.write("counters.toml", '[[counter]]\nname = "publication.position"\n')
        self.write("README.md", "# Cavalry\n\n## The ten-minute path\n\nWords.\n")
        self.write("docs/how-to/run-the-bench.md", "# Run the bench\n\n## Steps\n\nWords.\n")
        self.write("docs/decisions/0001-first.md", "# 0001. First\n\n- **Status:** Accepted\n")
        self.write("CHANGELOG.md", "# Changelog\n\n## Unreleased\n\n### Added\n\n- The first thing.\n")
        self.first = self.commit("Initial", "2026-01-01T10:00:00Z")

    def git(self, *args, env=None):
        full = {**os.environ, "GIT_TERMINAL_PROMPT": "0", **(env or {})}
        return subprocess.run(["git", *args], cwd=self.path, check=True, capture_output=True, text=True, env=full).stdout.strip()

    def write(self, rel, text):
        p = self.path / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text, encoding="utf-8")

    def append(self, rel, text):
        with open(self.path / rel, "a", encoding="utf-8") as f:
            f.write(text)

    def commit(self, subject, when, body=""):
        self.git("add", "-A")
        args = ["commit", "-q", "--allow-empty", "-m", subject] + (["-m", body] if body else [])
        self.git(*args, env={"GIT_AUTHOR_DATE": when, "GIT_COMMITTER_DATE": when})
        return self.git("rev-parse", "HEAD")

    def history(self):
        """c1 (Initial), c2 on master, cb on a branch merged with a merge commit, then c3."""
        self.append("CHANGELOG.md", "- The second thing.\n")
        self.write("docs/decisions/0002-second.md", "# 0002. Second\n\n- **Status:** Proposed\n")
        self.write("docs/how-to/second.md", "# Second\n")
        self.c2 = self.commit("Second thing", "2026-01-02T10:00:00Z", "The body of the second.")
        self.git("checkout", "-q", "-b", "b")
        self.append("CHANGELOG.md", "- The branch thing.\n")
        self.cb = self.commit("Branch thing", "2026-01-02T15:00:00Z")
        self.git("checkout", "-q", "master")
        self.git("merge", "-q", "--no-ff", "-m", "Merge b", "b", env={"GIT_AUTHOR_DATE": "2026-01-03T09:00:00Z", "GIT_COMMITTER_DATE": "2026-01-03T09:00:00Z"})
        self.append("CHANGELOG.md", "- The third thing.\n")
        self.write("docs/decisions/0002-second.md", "# 0002. Second\n\n- **Status:** Accepted\n")
        self.c3 = self.commit("Third thing", "2026-01-03T10:00:00Z")
        self.git("checkout", "-q", "-b", "dangling", self.first)
        self.dangling = self.commit("Dangling", "2026-01-04T10:00:00Z")
        self.git("checkout", "-q", "master")
        return self


class Run:
    def __init__(self, result, home, outputs):
        self.result, self.home = result, Path(home)
        self.out = result.stdout + result.stderr
        self.outputs = {}
        if Path(outputs).exists():
            for line in Path(outputs).read_text(encoding="utf-8").splitlines():
                name, _, value = line.partition("=")
                self.outputs[name] = value

    def read(self, rel):
        return (self.home / rel).read_text(encoding="utf-8")


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.gh = FakeGitHub()
        self.addCleanup(self.gh.close)
        self.repo = Repo(Path(self.tmp.name) / "checkout")
        self.email = Path(self.tmp.name) / "email"

    def run_script(self, *args, env=None, cwd=None):
        outputs = Path(self.tmp.name) / f"outputs-{len(os.listdir(self.tmp.name))}.txt"
        full = {"PATH": os.environ["PATH"], "HOME": self.tmp.name, "RELEASE_NOTES_DIR": str(self.email),
                "GITHUB_REPOSITORY": REPO, "GITHUB_API_URL": self.gh.url, "GITHUB_SERVER_URL": SERVER,
                "GITHUB_OUTPUT": str(outputs), "GH_TOKEN": FAKE_TOKEN}
        full.update(env or {})
        result = subprocess.run([sys.executable, "-I", str(SCRIPT), *args], cwd=cwd or self.repo.path,
                                capture_output=True, text=True, env=full)
        run = Run(result, self.email, outputs)
        self.assertNotIn(FAKE_TOKEN, run.out, "the output carries the job token")
        return run

    def passes(self, run, *wanted):
        self.assertEqual(run.result.returncode, 0, run.out)
        for w in wanted:
            self.assertIn(w, run.out)

    def fails(self, run, wanted):
        self.assertEqual(run.result.returncode, 1, run.out)
        self.assertIn(wanted, run.out)


# ---- window ------------------------------------------------------------------------------

class Window(Base):
    def setUp(self):
        super().setUp()
        self.repo.history()

    def check_from_c2(self, run):
        self.passes(run)
        self.assertEqual(run.outputs["count"], "3")
        self.assertEqual((run.outputs["first"], run.outputs["last"]), (self.repo.c2[:7], self.repo.c3[:7]))
        self.assertEqual((run.outputs["first_date"], run.outputs["last_date"]), ("2026-01-02", "2026-01-03"))
        self.assertEqual(run.outputs["title"], "What's new in Cavalry, January 2 to 3, 2026")
        self.assertEqual(run.outputs["subject"], "Cavalry: what's new, January 2 to 3, 2026")
        self.assertEqual(run.read("window.txt"), f"{self.repo.c2}\n{self.repo.cb}\n{self.repo.c3}\n")
        self.assertEqual(run.read("title.txt"), "What's new in Cavalry, January 2 to 3, 2026\n")
        commits = run.read("sources/commits.md")
        for want in (f"## {self.repo.c2[:7]} Second thing", "The body of the second.", "## {} Branch thing".format(self.repo.cb[:7]), "Third thing"):
            self.assertIn(want, commits)
        self.assertNotIn("Merge b", commits)
        self.assertNotIn("Initial", commits)
        changelog = run.read("sources/changelog.md")
        for want in ("- The second thing.", "- The branch thing.", "- The third thing."):
            self.assertIn(want, changelog)
        self.assertNotIn("The first thing", changelog)
        decisions = run.read("sources/decisions.md")
        self.assertIn(f"- Added: 0002. Second; Status: Accepted. {REPO_URL}/blob/master/docs/decisions/0002-second.md", decisions)
        self.assertNotIn("0001", decisions)
        docs = run.read("sources/docs.md")
        self.assertIn("- A\tdocs/how-to/second.md", docs)
        self.assertNotIn("CHANGELOG", docs)

    def test_from_a_commit_is_inclusive_and_asks_the_api_nothing(self):
        for asked in (self.repo.c2, self.repo.c2[:7]):
            run = self.run_script("window", env={"RELEASE_NOTES_FROM": asked})
            self.check_from_c2(run)
            self.assertIn(f"the window starts with {self.repo.c2[:7]}, as asked", run.out)
        self.assertEqual(self.gh.seen, [])

    def test_from_a_date(self):
        run = self.run_script("window", env={"RELEASE_NOTES_FROM": "2026-01-02"})
        self.check_from_c2(run)
        self.assertIn("the window starts on 2026-01-02, as asked", run.out)

    def test_from_the_root_commit_covers_everything(self):
        run = self.run_script("window", env={"RELEASE_NOTES_FROM": self.repo.first})
        self.passes(run)
        self.assertEqual(run.outputs["count"], "4")
        self.assertIn("- The first thing.", run.read("sources/changelog.md"))
        self.assertIn("- Added: 0001. First; Status: Accepted.", run.read("sources/decisions.md"))

    def test_from_refuses_what_is_not_a_commit_on_this_history(self):
        self.fails(self.run_script("window", env={"RELEASE_NOTES_FROM": "nosuch"}), "nosuch is not a commit of this checkout")
        self.fails(self.run_script("window", env={"RELEASE_NOTES_FROM": "2026-13-01"}), "2026-13-01 is not a date")
        self.fails(self.run_script("window", env={"RELEASE_NOTES_FROM": "--output=x"}), "is neither a commit nor a date")
        self.fails(self.run_script("window", env={"RELEASE_NOTES_FROM": self.repo.dangling}), "is not on the history of")

    def test_follows_the_last_email(self):
        """The last email is the newest successful run whose SEND STEP succeeded. A newer run that
        only previewed (the step skipped) does not move the mark, and neither does this run."""
        self.gh.routes[f"/repos/{REPO}/actions/workflows/{rn.WORKFLOW_FILE}/runs"] = {"workflow_runs": [
            run_data(30, "2026-01-03T09:30:00Z", self.repo.c3),
            run_data(10, "2026-01-02T12:00:00Z", self.repo.c2),
            run_data(20, "2026-01-02T16:00:00Z", self.repo.cb)]}
        self.gh.routes[f"/repos/{REPO}/actions/runs/30/jobs"] = jobs_data("success")
        self.gh.routes[f"/repos/{REPO}/actions/runs/20/jobs"] = jobs_data("skipped")
        self.gh.routes[f"/repos/{REPO}/actions/runs/10/jobs"] = jobs_data("success")
        run = self.run_script("window", env={"GITHUB_RUN_ID": "30", "GITHUB_SHA": self.repo.c3})
        self.passes(run, f"the last email was sent by the run created at 2026-01-02T12:00:00Z, from {self.repo.c2[:7]}")
        self.assertEqual(run.outputs["count"], "2")
        self.assertEqual((run.outputs["first"], run.outputs["last"]), (self.repo.cb[:7], self.repo.c3[:7]))
        self.assertNotIn("The second thing", run.read("sources/changelog.md"))
        self.assertIn("- Changed: 0002. Second; Status: Accepted, was Proposed.", run.read("sources/decisions.md"))
        self.assertFalse(self.gh.asked(f"/repos/{REPO}/actions/runs/30/jobs"), "the run read its own jobs")
        self.assertTrue(any("status=success" in path for path, _ in self.gh.seen), self.gh.seen)

    def test_refuses_a_last_email_off_this_history(self):
        self.gh.routes[f"/repos/{REPO}/actions/workflows/{rn.WORKFLOW_FILE}/runs"] = {"workflow_runs": [run_data(10, "2026-01-04T12:00:00Z", self.repo.dangling)]}
        self.gh.routes[f"/repos/{REPO}/actions/runs/10/jobs"] = jobs_data("success")
        self.fails(self.run_script("window"), "which is not on the history of")

    def test_seven_days_without_an_earlier_email(self):
        now = datetime.datetime.now(datetime.timezone.utc)
        self.repo.append("CHANGELOG.md", "- Ten days ago.\n")
        self.repo.commit("Ten days ago", rn.stamp(now - datetime.timedelta(days=10)))
        self.repo.append("CHANGELOG.md", "- Three days ago.\n")
        recent = self.repo.commit("Three days ago", rn.stamp(now - datetime.timedelta(days=3)))
        self.gh.routes[f"/repos/{REPO}/actions/workflows/{rn.WORKFLOW_FILE}/runs"] = {"workflow_runs": []}
        run = self.run_script("window")
        self.passes(run, "no earlier email was found: the window is the last seven days")
        self.assertEqual((run.outputs["count"], run.outputs["first"]), ("1", recent[:7]))
        changelog = run.read("sources/changelog.md")
        self.assertIn("- Three days ago.", changelog)
        self.assertNotIn("Ten days ago", changelog)
        # With a run id, the seven days count back from the run's own start, not from the clock.
        self.gh.routes[f"/repos/{REPO}/actions/runs/40"] = run_data(40, rn.stamp(now - datetime.timedelta(days=8)), recent)
        run = self.run_script("window", env={"GITHUB_RUN_ID": "40"})
        self.passes(run)
        self.assertEqual(run.outputs["count"], "2")
        self.assertIn("- Ten days ago.", run.read("sources/changelog.md"))

    def test_nothing_to_send(self):
        self.gh.routes[f"/repos/{REPO}/actions/workflows/{rn.WORKFLOW_FILE}/runs"] = {"workflow_runs": []}
        run = self.run_script("window")
        self.passes(run, "::notice::release-notes: no commit landed since the last email; nothing to send")
        self.assertEqual(run.outputs["count"], "0")
        self.assertNotIn("subject", run.outputs)
        self.assertEqual(run.read("window.txt"), "")
        self.assertFalse((self.email / "title.txt").exists())

    def test_fails_when_the_api_refuses(self):
        run = self.run_script("window", env={"GITHUB_REPOSITORY": "example-owner/another-repo"})
        self.fails(run, f"::error::release-notes: GET /repos/example-owner/another-repo/actions/workflows/{rn.WORKFLOW_FILE}/runs answered HTTP 404: Not Found")

    def test_the_token_is_a_header_on_every_request(self):
        self.gh.routes[f"/repos/{REPO}/actions/workflows/{rn.WORKFLOW_FILE}/runs"] = {"workflow_runs": []}
        self.passes(self.run_script("window"))
        self.assertTrue(self.gh.seen)
        for path, headers in self.gh.seen:
            self.assertEqual(headers.get("Authorization"), f"Bearer {FAKE_TOKEN}", path)

    def test_refuses_a_plain_http_api_off_loopback(self):
        self.fails(self.run_script("window", env={"GITHUB_API_URL": "http://api.example"}), "GITHUB_API_URL must use https")

    def test_refuses_without_a_repository_or_a_directory(self):
        run = self.run_script("window", env={"GITHUB_REPOSITORY": ""})
        self.fails(run, "GITHUB_REPOSITORY is not set")
        run = self.run_script("window", env={"RELEASE_NOTES_DIR": "", "RELEASE_NOTES_FROM": self.repo.c2})
        self.fails(run, "RELEASE_NOTES_DIR is not set")


class Span(unittest.TestCase):
    def test_the_dates_in_words(self):
        d = datetime.date
        self.assertEqual(rn.span(d(2026, 10, 16), d(2026, 10, 16)), "October 16, 2026")
        self.assertEqual(rn.span(d(2026, 10, 10), d(2026, 10, 16)), "October 10 to 16, 2026")
        self.assertEqual(rn.span(d(2026, 9, 29), d(2026, 10, 6)), "September 29 to October 6, 2026")
        self.assertEqual(rn.span(d(2025, 12, 29), d(2026, 1, 4)), "December 29, 2025 to January 4, 2026")


# ---- adopt -------------------------------------------------------------------------------

class Adopt(Base):
    def setUp(self):
        super().setUp()
        self.repo.history()
        self.passes(self.run_script("window", env={"RELEASE_NOTES_FROM": self.repo.c2}))
        self.draft = Path(self.tmp.name) / "draft"
        self.draft.mkdir()
        (self.draft / "notes.md").write_text("# A draft\n", encoding="utf-8")
        (self.draft / "review.md").write_text("APPROVED\n", encoding="utf-8")
        (self.draft / "window.txt").write_text((self.email / "window.txt").read_text(encoding="utf-8"), encoding="utf-8")
        self.gh.routes[f"/repos/{REPO}"] = {"default_branch": "master"}
        self.gh.routes[f"/repos/{REPO}/actions/runs/777"] = run_data(777, "2026-01-03T11:00:00Z", self.repo.c3)
        self.env = {"RELEASE_NOTES_DRAFT_RUN": "777", "RELEASE_NOTES_DRAFT_DIR": str(self.draft)}

    def adopt(self, **more):
        return self.run_script("adopt", env={**self.env, **more})

    def test_adopts_the_approved_draft_of_the_same_window(self):
        run = self.adopt()
        self.passes(run, "the draft of run 777 is adopted: approved there, 3 commit(s), the same as this email covers")
        self.assertEqual(run.read("notes.md"), "# A draft\n")
        self.assertEqual(run.read("review.md"), "APPROVED\n")

    def test_refuses_a_run_that_is_not_an_approved_run_of_this_workflow_on_master(self):
        self.fails(self.adopt(RELEASE_NOTES_DRAFT_RUN="77x"), "digits only")
        self.fails(self.adopt(GITHUB_RUN_ID="777"), "a run cannot adopt its own draft")
        self.fails(self.adopt(RELEASE_NOTES_DRAFT_DIR=""), "RELEASE_NOTES_DRAFT_DIR is not set")
        self.gh.routes[f"/repos/{REPO}/actions/runs/777"] = run_data(777, "x", self.repo.c3, path=".github/workflows/ci.yml")
        self.fails(self.adopt(), f"run 777 is not a run of {rn.WORKFLOW_FILE}")
        self.gh.routes[f"/repos/{REPO}/actions/runs/777"] = run_data(777, "x", self.repo.c3, conclusion="failure")
        self.fails(self.adopt(), "run 777 did not succeed (failure): its gate approved no draft")
        self.gh.routes[f"/repos/{REPO}/actions/runs/777"] = run_data(777, "x", self.repo.c3, head_branch="feature")
        self.fails(self.adopt(), "run 777 ran on feature, not on the default branch")

    def test_refuses_a_draft_that_was_not_approved_or_covers_another_window(self):
        (self.draft / "review.md").write_text("CHANGES_REQUESTED\nA finding.\n", encoding="utf-8")
        self.fails(self.adopt(), "the review of run 777 does not say APPROVED")
        (self.draft / "review.md").write_text("APPROVED\n", encoding="utf-8")
        with open(self.draft / "window.txt", "a", encoding="utf-8") as f:
            f.write(f"{self.repo.first}\n")
        self.fails(self.adopt(), "the draft of run 777 covers 4 commit(s) and this email covers 3, or not the same ones")
        (self.draft / "window.txt").unlink()
        self.fails(self.adopt(), "run 777 left no window")
        (self.draft / "notes.md").write_text("", encoding="utf-8")
        self.fails(self.adopt(), "run 777 left no draft")
        self.assertFalse((self.email / "notes.md").exists())


# ---- recipients --------------------------------------------------------------------------

class Recipients(Base):
    def test_masks_judges_lowercases_and_counts_once(self):
        listing = ("Ann <ann@example.com>, bob@example.com; carol@example.com\n"
                   "# a comment with dave@example.com\n"
                   "BOB@example.com erin@users.noreply.github.com ok@example.org\nok@example.org\n")
        run = self.run_script("recipients", env={"RELEASE_NOTES_RECIPIENTS": listing})
        self.passes(run, "the list has 8 entries; 3 will receive the email", "1 repeat an address above", "4 were refused (above)")
        self.assertEqual(run.read("recipients.txt"), "carol@example.com\nbob@example.com\nok@example.org\n")
        self.assertEqual(stat.S_IMODE((self.email / "recipients.txt").stat().st_mode), 0o600)
        self.assertEqual((run.outputs["count"], run.outputs["bcc"]), ("3", "carol@example.com,bob@example.com,ok@example.org"))
        for line in run.out.splitlines():
            if "@" in line:
                self.assertTrue(line.startswith("::add-mask::"), f"an address outside a mask: {line}")
        masks = [line for line in run.out.splitlines() if line.startswith("::add-mask::")]
        for want in ("::add-mask::<ann@example.com>", "::add-mask::bob@example.com;", "::add-mask::BOB@example.com",
                     "::add-mask::bob@example.com", "::add-mask::erin@users.noreply.github.com"):
            self.assertIn(want, masks)
        self.assertNotIn("dave", run.out)
        self.assertEqual(run.out.count("::warning::release-notes: entry"), 4)

    def test_an_empty_list_sends_to_nobody(self):
        run = self.run_script("recipients", env={"RELEASE_NOTES_RECIPIENTS": "  # nobody yet\n"})
        self.passes(run, "::notice::release-notes: the list of recipients is empty")
        self.assertEqual((run.outputs["count"], run.outputs["bcc"]), ("0", ""))


# ---- finish ------------------------------------------------------------------------------

class Finish(Base):
    def test_puts_the_footer_under_the_draft_and_renders(self):
        self.email.mkdir()
        (self.email / "window.txt").write_text("abc\n", encoding="utf-8")
        (self.email / "notes.md").write_text(SAMPLE, encoding="utf-8")
        run = self.run_script("finish")
        self.passes(run, "the email is", "lines, footer included; the HTML is")
        text = run.read("email.md")
        self.assertTrue(text.startswith(SAMPLE.rstrip("\n") + "\n\n---\n\n"))
        self.assertIn(f"[CHANGELOG.md]({REPO_URL}/blob/master/CHANGELOG.md)", text)
        self.assertIn(f"on the list for the release notes of [{REPO}]({REPO_URL}). To stop these emails, reply to this one and say so.", text)
        page = run.read("email.html")
        self.assertIn("reply to this one and say so.", page)
        self.assertIn("<h1 ", page)

    def test_refuses_without_a_draft(self):
        self.email.mkdir()
        (self.email / "window.txt").write_text("abc\n", encoding="utf-8")
        self.fails(self.run_script("finish"), "there is no draft at")


# ---- lint --------------------------------------------------------------------------------

CLEAN = f"""# The title

Cavalry is a transport. **This email covers two commits.**

## What's new

### Run the driver

`cavalry run` starts it and `cavalry doctor` explains it; `just test` runs the tests. `CAVALRY_DIR` moves the directory, `dir` is the key, and `publication.position` is a counter. The spec is `docs/how-to/run-the-bench.md`.

```in a terminal
target/release/cavalry run &
just bench
```

[Step by step →]({REPO_URL}/blob/master/docs/how-to/run-the-bench.md#steps)

## Decided

- ADR 0001: the first decision. [Read the decision →]({REPO_URL}/blob/master/docs/decisions/0001-first.md)

## Try it

[The ten-minute path]({REPO_URL}#the-ten-minute-path) and [the releases]({REPO_URL}/releases).
"""

DIRTY = f"""# Another title

Words with a TODO and the word Unreleased.

## What's new

### Many wrongs

`just nosuch` and `cavalry replay` and `CAVALRY_NOPE` and `foo.bar` and `docs/missing.md` and ADR 0099.
See [outside](https://example.com/x), [missing]({REPO_URL}/blob/master/docs/nope.md) and
[no heading]({REPO_URL}/blob/master/README.md#nope). Commit abc1234 fixed card 93 and #12 for
ann@example.com with ghp_abcdefghijklmnopqrstuvwxyz0123456789.

| a table |
"""


class Lint(Base):
    def lint(self, text, title="The title", cwd=None):
        path = Path(self.tmp.name) / "notes.md"
        path.write_text(text, encoding="utf-8")
        return self.run_script("lint", str(path), env={"RELEASE_NOTES_TITLE": title}, cwd=cwd)

    def test_a_clean_draft(self):
        run = self.lint(CLEAN)
        self.passes(run, "release-notes-lint: clean", "ok: the first line is the title", "ok: just test is a recipe", "ok: just bench is a recipe",
                    "ok: cavalry run is a command", "ok: cavalry doctor is a command", "ok: CAVALRY_DIR is a key's variable",
                    "ok: `publication.position` is a counter or a key",
                    "ok: `docs/how-to/run-the-bench.md` exists in the checkout", "ok: ADR 0001 exists",
                    f"ok: the link {REPO_URL}#the-ten-minute-path resolves", f"ok: the link {REPO_URL}/releases resolves",
                    f"ok: the link {REPO_URL}/blob/master/docs/how-to/run-the-bench.md#steps resolves", "ok: the draft renders as the email")
        self.assertNotIn("FAIL", run.out)

    def test_every_finding(self):
        run = self.lint(DIRTY)
        self.assertEqual(run.result.returncode, 1)
        fails = [line for line in run.out.splitlines() if line.startswith("FAIL: ")]
        for want in ("the draft carries a placeholder", "the draft says Unreleased",
                     "the first line is '# Another title'; it must be exactly `# The title`",
                     "just nosuch is not a recipe of the justfile (have: bench test)",
                     "cavalry replay is not a command of the binary (have: doctor run)",
                     "CAVALRY_NOPE is not the environment variable of a key in config.toml",
                     "`foo.bar` is neither a counter in counters.toml nor a key in config.toml",
                     "`docs/missing.md` is not a file of the checkout", "ADR 0099 has no file under docs/decisions/",
                     f"the link https://example.com/x is outside {REPO_URL}",
                     f"the link {REPO_URL}/blob/master/docs/nope.md names docs/nope.md, which is not in the checkout",
                     f"the link {REPO_URL}/blob/master/README.md#nope names a heading README.md does not have",
                     "the draft names a commit (above)", "the draft names a card, an issue or a pull request (above)",
                     "the draft carries an address (above)", "the draft carries something secret-shaped (above)",
                     "the draft does not render as the email (above)"):
            self.assertTrue(any(want in f for f in fails), f"no FAIL line says {want!r}:\n{run.out}")
        self.assertIn("  line 14: a table is not supported", run.out)
        self.assertIn(f"release-notes-lint: {len(fails)} finding(s)", run.out)

    def test_bounded_words(self):
        run = self.lint("# The title\n\n" + " ".join(["word"] * (rn.MAX_WORDS + 1)) + "\n")
        self.fails(run, f"the brief allows 450 and this lint refuses more than {rn.MAX_WORDS}")

    def test_skips_the_title_when_none_is_given(self):
        run = self.lint(CLEAN, title="")
        self.passes(run, "skip: RELEASE_NOTES_TITLE is unset")

    def test_a_missing_file(self):
        run = self.run_script("lint", str(Path(self.tmp.name) / "nope.md"))
        self.assertEqual(run.result.returncode, 1)
        self.assertIn("does not exist", run.out)

    def test_the_sample_is_clean_against_this_checkout(self):
        """The sample names recipes, commands, keys, ADRs and pages of THIS repository, so a
        rename there is a change here: the writer reads the sample as a model."""
        title = SAMPLE.split("\n", 1)[0][2:]
        path = Path(self.tmp.name) / "sample.md"
        path.write_text(SAMPLE, encoding="utf-8")
        run = self.run_script("lint", str(path), cwd=ROOT, env={"RELEASE_NOTES_TITLE": title, "GITHUB_REPOSITORY": "appshapes/cavalry",
                                                                 "GITHUB_SERVER_URL": "https://github.com"})
        self.passes(run, "release-notes-lint: clean")


class Readers(unittest.TestCase):
    """The lint reads the truth from this checkout's own files; the parsers must keep reading them."""

    def test_the_justfile(self):
        recipes = rn.justfile_recipes(ROOT)
        self.assertTrue({"test", "test-scripts", "lint", "docs", "lint-docs", "bench", "quickstart"} <= recipes, recipes)
        self.assertNotIn("set", recipes)

    def test_the_binarys_usage(self):
        self.assertTrue({"run", "doctor"} <= rn.cavalry_commands(ROOT), rn.cavalry_commands(ROOT))

    def test_the_registries(self):
        self.assertIn("dir", rn.toml_names(ROOT / "config.toml"))
        self.assertTrue(rn.toml_names(ROOT / "counters.toml"))

    def test_the_readme_headings(self):
        self.assertIn("thetenminutepath", rn.headings(ROOT / "README.md"))


# ---- the joins ---------------------------------------------------------------------------

class Workflow(unittest.TestCase):
    """What ties the workflow file to this script, the brief and the sample. A rename in one
    place fails here rather than in a Monday run."""

    @classmethod
    def setUpClass(cls):
        cls.text = (WORKFLOW / rn.WORKFLOW_FILE).read_text(encoding="utf-8")
        cls.brief = BRIEF.read_text(encoding="utf-8")

    def test_the_send_step_is_named_as_the_script_expects(self):
        self.assertEqual(self.text.count(f"- name: {rn.SEND_STEP}\n"), 1)

    def test_the_schedule_is_weekly(self):
        m = re.search(r'cron: "([^"]+)"', self.text)
        self.assertIsNotNone(m)
        fields = m.group(1).split()
        self.assertEqual(len(fields), 5)
        self.assertRegex(fields[4], r"^[0-6]$")
        self.assertNotEqual(fields[0], "0", "off the hour, on GitHub's own advice")

    def test_every_action_is_pinned_and_every_job_bounded(self):
        uses = re.findall(r"^\s+uses: (.*)$", self.text, re.M)
        self.assertTrue(uses)
        for use in uses:
            self.assertRegex(use, r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+@[0-9a-f]{40} # v\S+$", use)
        self.assertIn("timeout-minutes:", self.text)

    def test_the_credentials_reach_only_the_steps_that_need_them(self):
        self.assertEqual(self.text.count("secrets.RELEASE_NOTES_RECIPIENTS"), 1)
        self.assertEqual(self.text.count("secrets.SMTP_PASSWORD"), 3, "the configured check and the two mail steps")
        self.assertEqual(self.text.count("secrets.SMTP_USERNAME"), 3)
        self.assertEqual(self.text.count("secrets.CLAUDE_CODE_OAUTH_TOKEN"), 4, "the writer, the reviewer, the fixer, the second reviewer")
        self.assertNotIn("GH_ACTIONS_TOKEN", self.text)
        self.assertIn("contents: read", self.text)
        self.assertNotIn("contents: write", self.text)

    def test_the_agents_read_the_brief_and_cannot_write_the_checkout(self):
        self.assertTrue(BRIEF.is_file())
        self.assertGreaterEqual(self.text.count("`.claude/agents/release-notes-email.md`"), 5, "the header and every agent prompt name the brief")
        self.assertEqual(self.text.count('"Write",'), 4)
        self.assertEqual(self.text.count('"Edit",'), 4)
        self.assertIn("RELEASE_NOTES_DIR: /tmp/cavalry-email", self.text)
        self.assertIn("/tmp/cavalry-email/sources/changelog.md", self.brief)
        self.assertIn("/tmp/cavalry-email/lint.txt", self.brief)
        self.assertIn("scripts/release-notes-email-sample.md", self.brief)

    def test_the_workflow_runs_this_script_and_the_renderer(self):
        self.assertIn("python3 scripts/release-notes.py window", self.text)
        self.assertIn("python3 scripts/release-notes.py adopt", self.text)
        self.assertIn("python3 scripts/release-notes.py lint", self.text)
        self.assertIn("python3 scripts/release-notes.py recipients", self.text)
        self.assertIn("python3 scripts/release-notes.py finish", self.text)
        self.assertTrue(rn.RENDERER.is_file())

    def test_the_sample_follows_the_brief(self):
        for heading in ("## What's new", "## Decided", "## Before you update", "## Try it"):
            self.assertIn(heading, SAMPLE)
            self.assertIn(heading, self.brief)


if __name__ == "__main__":
    unittest.main()
