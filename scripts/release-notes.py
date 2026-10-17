#!/usr/bin/env python3
"""release-notes: the deterministic steps of the weekly release-notes email (ADR 0026).

    release-notes.py window            which commits the email covers, and the writer's sources
    release-notes.py adopt             the approved draft of an earlier run, in place of a new one
    release-notes.py lint <notes.md>   the deterministic half of the gate: nothing named that does not exist
    release-notes.py recipients        the addresses the email goes to
    release-notes.py finish            the approved draft with the footer under it, as text and as HTML

`.github/workflows/send-release-notes.yml` runs these. The email's words are a writer agent's,
gated by `lint` and by a reviewer agent; WHICH commits it covers, WHO receives it and WHAT it
says about why they received it are decided here, by code that release-notes-test.py tests.

  window      The commits on this branch after the last email and up to the commit this run
              checked out, merge commits left out. "The last email" is the newest successful
              run of the workflow in which the step named SEND_STEP concluded `success`: a
              preview, a run with nothing to send and a run that failed all leave the mark where
              it was, so the next run covers what they did not. Without one, the last seven
              days. RELEASE_NOTES_FROM, the workflow's `from` input, overrides both and is
              INCLUSIVE: a commit means the email starts with it, a date YYYY-MM-DD means it
              starts with that day's first commit. It writes window.txt (one commit a line,
              oldest first), title.txt (the email's first line, which the writer copies) and,
              under sources/, commits.md (every commit's message, the review record),
              changelog.md (the lines the window added to CHANGELOG.md, the authority on what
              changed for a user), decisions.md (the ADRs the window added or changed, with their
              status) and docs.md (the documentation pages the window touched).
  adopt       Takes the draft an earlier run wrote and its gate approved, so that what was
              previewed is what is sent: every run writes a new draft, and a gate can approve
              one and refuse the next (Brigade's first send was refused after a preview of the
              same releases had been approved and read). It refuses unless the run is a
              successful run of this workflow on the default branch, a run only succeeds when
              its gate approved, its review says APPROVED, and its window is this run's window,
              commit for commit: a commit that landed since belongs in the email, and the old
              draft does not have it. The workflow downloads that run's artifact into
              RELEASE_NOTES_DRAFT_DIR first; this copies notes.md and review.md from it. The
              lint runs on the adopted draft as on any other.
  lint        A model composed the draft, and a model names things that do not exist: Brigade's
              first release notes named a skill that was written later. This refuses, with one
              `FAIL:` line per finding for the writer's fix cycle: a first line that is not the
              title; a `just` recipe the justfile lacks; a `cavalry` command the binary's usage
              lacks; a `CAVALRY_` variable that is no key of config.toml; a backticked dotted
              name that is neither a counter nor a key; a backticked path the checkout lacks;
              an ADR number with no file; a link outside this repository, or into a path or
              heading that does not exist; a commit hash, a card or issue number, an address,
              a placeholder, the word Unreleased, anything secret-shaped, more than MAX_WORDS
              words; and a draft the renderer refuses, with its line numbers.
  recipients  The addresses of RELEASE_NOTES_RECIPIENTS, a list the maintainers keep by hand.
              Each entry is held to one plain address, lower-cased and counted once. It writes
              recipients.txt (0600) and the `bcc` output, and registers every address as a mask
              BEFORE anything else can print it: this repository is public and so are its run
              logs. It makes no request.
  finish      Writes email.md: notes.md, then a footer that says where every change is, why the
              reader received this and how to stop it, by answering the email, which is why the
              sender's address is an inbox someone reads. The footer is this script's and not
              the agent's so that no draft can leave it out. Then renders email.html from it with
              release-notes-email.py. The mail step sends email.html as the HTML part and
              email.md as the text part.

Environment:
  RELEASE_NOTES_DIR          the working directory, outside the checkout (the workflow: /tmp/cavalry-email)
  GITHUB_REPOSITORY          owner/name (Actions sets it)
  GITHUB_API_URL             Actions sets it; https://api.github.com when unset
  GITHUB_SERVER_URL          Actions sets it; https://github.com when unset
  GITHUB_RUN_ID              this run (Actions sets it); unset, a window with no earlier email counts back from now
  GITHUB_SHA                 the commit the checkout is at (Actions sets it); HEAD when unset
  GITHUB_OUTPUT              where step outputs go, when set
  GH_TOKEN                   window, adopt: the job token. It is read from the environment, sent as a
                             header and never put on argv, since a runner's `ps` and the Actions debug
                             log both see argv; it is removed from the environment before any child
                             process starts
  RELEASE_NOTES_FROM         window: the first commit to cover, or a date, optional
  RELEASE_NOTES_TITLE        lint: the title the window gave, which the draft's first line must be
  RELEASE_NOTES_DRAFT_RUN    adopt: the id of the earlier run (the workflow's `draft_run` input)
  RELEASE_NOTES_DRAFT_DIR    adopt: where the workflow downloaded that run's artifact
  RELEASE_NOTES_RECIPIENTS   recipients: the list, addresses separated by commas, blanks or line ends,
                             `#` starts a comment. A SECRET of the workflow, never a variable and never
                             a file of the repository: a variable is printed unmasked, and the
                             repository is public

The checkout is the working directory. Every line goes to stdout, the annotations included, so
that GitHub renders them on the run page. No address is ever printed except on its own
`::add-mask::` line, which the runner consumes. Standard library only, Python 3.9 (ADR 0003).
"""
from __future__ import annotations

import datetime
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
RENDERER = HERE / "release-notes-email.py"

# The name of the workflow's step that sends to the recipients, and the workflow's file name.
# `window` finds the last email by this step's conclusion, so the two are joined to the
# workflow by release-notes-test.py: renaming the step there without renaming it here would
# make every run believe no email was ever sent.
SEND_STEP = "Send to the recipients"
WORKFLOW_FILE = "send-release-notes.yml"
DEFAULT_WINDOW = datetime.timedelta(days=7)    # the schedule is weekly
EMPTY_TREE = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"    # git's empty tree: the diff base before the first commit
MAX_WORDS = 600                                 # the brief allows 450; this is the bound a runaway draft meets
MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August",
          "September", "October", "November", "December"]
# A backticked token with a slash is a path of the checkout when it starts with one of these.
TOP_DIRS = {"docs", "spec", "crates", "scripts", "bench", "conformance", "clients", ".github", ".claude"}
MODES = ("window", "adopt", "lint", "recipients", "finish")


def say(msg):
    print(f"release-notes: {msg}", flush=True)


def note(msg):
    print(f"::notice::release-notes: {msg}", flush=True)


def warn(msg):
    print(f"::warning::release-notes: {msg}", flush=True)


def die(msg):
    print(f"::error::release-notes: {msg}", flush=True)
    sys.exit(1)


def output(name, value):
    """A step output, when the script runs as a workflow step."""
    path = os.environ.get("GITHUB_OUTPUT")
    if path:
        with open(path, "a", encoding="utf-8") as f:
            f.write(f"{name}={value}\n")


class Context:
    """What every mode shares: the directories, the repository and the API."""

    def __init__(self, token):
        self.dir = Path(os.environ["RELEASE_NOTES_DIR"]) if os.environ.get("RELEASE_NOTES_DIR") else None
        self.repo = os.environ.get("GITHUB_REPOSITORY", "")
        self.api = os.environ.get("GITHUB_API_URL", "https://api.github.com").rstrip("/")
        self.server = os.environ.get("GITHUB_SERVER_URL", "https://github.com").rstrip("/")
        self.run_id = os.environ.get("GITHUB_RUN_ID", "")
        self.sha = os.environ.get("GITHUB_SHA", "HEAD")
        self.root = Path.cwd()
        self._token = token
        if not self.repo:
            die("GITHUB_REPOSITORY is not set: the repository (owner/name) is required")
        if not re.fullmatch(r"[A-Za-z0-9._-]+/[A-Za-z0-9._-]+", self.repo):
            die(f"GITHUB_REPOSITORY is not an owner/name: {self.repo}")
        # https only: the job token rides on every request. Loopback is the exception the tests run on.
        if not (self.api.startswith("https://") or re.match(r"^http://(127\.0\.0\.1|localhost)(:\d+)?$", self.api)):
            die(f"GITHUB_API_URL must use https (http is allowed only for 127.0.0.1/localhost): {self.api}")

    @property
    def repo_url(self):
        return f"{self.server}/{self.repo}"

    def need_dir(self):
        if self.dir is None:
            die("RELEASE_NOTES_DIR is not set: the working directory is required")
        self.dir.mkdir(parents=True, exist_ok=True)

    def get(self, path):
        """GET <path and query> -> the parsed JSON, or the end of the run. GitHub's `message`
        is the only field quoted back: it carries no credential."""
        headers = {"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28"}
        if self._token:
            headers["Authorization"] = f"Bearer {self._token}"
        request = urllib.request.Request(self.api + path, headers=headers)
        for attempt in range(3):
            try:
                with urllib.request.urlopen(request, timeout=30) as response:
                    return json.loads(response.read().decode("utf-8"))
            except urllib.error.HTTPError as e:
                if e.code >= 500 and attempt < 2:
                    time.sleep(5)
                    continue
                try:
                    message = json.loads(e.read().decode("utf-8")).get("message", "")
                except (ValueError, OSError):
                    message = ""
                die(f"GET {path.split('?')[0]} answered HTTP {e.code}: {message}")
            except (urllib.error.URLError, OSError) as e:
                if attempt < 2:
                    time.sleep(5)
                    continue
                die(f"GET {path.split('?')[0]} failed: {getattr(e, 'reason', e)}")
        return None    # unreachable: every path above returns or dies


def git(ctx, *args, check=True):
    p = subprocess.run(["git", *args], cwd=ctx.root, capture_output=True, text=True)
    if check and p.returncode != 0:
        die(f"git {' '.join(args)} failed: {p.stderr.strip()}")
    return p.stdout if p.returncode == 0 else None


def is_ancestor(ctx, older, newer):
    p = subprocess.run(["git", "merge-base", "--is-ancestor", older, newer], cwd=ctx.root, capture_output=True)
    return p.returncode == 0


def first_parent(ctx, sha):
    parents = git(ctx, "rev-list", "--parents", "-n", "1", sha).split()
    return parents[1] if len(parents) > 1 else EMPTY_TREE


def stamp(when):
    """A time in GitHub's own form, the one git's --since reads too."""
    return when.astimezone(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_stamp(text):
    return datetime.datetime.strptime(text, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc)


def span(first, last):
    """The dates an email covers, in words: "October 10 to 16, 2026"."""
    if first == last:
        return f"{MONTHS[first.month - 1]} {first.day}, {first.year}"
    if first.year != last.year:
        return f"{MONTHS[first.month - 1]} {first.day}, {first.year} to {MONTHS[last.month - 1]} {last.day}, {last.year}"
    if first.month != last.month:
        return f"{MONTHS[first.month - 1]} {first.day} to {MONTHS[last.month - 1]} {last.day}, {last.year}"
    return f"{MONTHS[first.month - 1]} {first.day} to {last.day}, {last.year}"


# ---- window ------------------------------------------------------------------------------

def last_send(ctx):
    """The head commit and creation time of the newest successful run whose send step
    succeeded, or None."""
    runs = ctx.get(f"/repos/{ctx.repo}/actions/workflows/{WORKFLOW_FILE}/runs?status=success&per_page=30")
    for run in sorted(runs.get("workflow_runs") or [], key=lambda r: r.get("created_at", ""), reverse=True):
        if str(run.get("id")) == ctx.run_id:
            continue
        jobs = ctx.get(f"/repos/{ctx.repo}/actions/runs/{run['id']}/jobs?per_page=100")
        for job in jobs.get("jobs") or []:
            for step in job.get("steps") or []:
                if step.get("name") == SEND_STEP and step.get("conclusion") == "success":
                    return run.get("head_sha", ""), run.get("created_at", "")
    return None


def window(ctx):
    ctx.need_dir()
    upper = git(ctx, "rev-parse", "--verify", f"{ctx.sha}^{{commit}}").strip()
    asked = os.environ.get("RELEASE_NOTES_FROM", "").strip()
    since = lower = None
    if asked:
        if re.fullmatch(r"\d{4}-\d{2}-\d{2}", asked):
            try:
                datetime.date.fromisoformat(asked)
            except ValueError:
                die(f"{asked} is not a date")
            since = f"{asked}T00:00:00Z"
            say(f"the window starts on {asked}, as asked")
        else:
            if asked.startswith("-") or not re.fullmatch(r"[A-Za-z0-9._/~^-]+", asked):
                die(f"{asked} is neither a commit nor a date (YYYY-MM-DD)")
            resolved = git(ctx, "rev-parse", "--verify", "--quiet", f"{asked}^{{commit}}", check=False)
            if not resolved:
                die(f"{asked} is not a commit of this checkout")
            start = resolved.strip()
            if not is_ancestor(ctx, start, upper):
                die(f"{asked} is not on the history of {upper[:7]}")
            lower = first_parent(ctx, start)
            say(f"the window starts with {start[:7]}, as asked")
    else:
        found = last_send(ctx)
        if found:
            head, created = found
            if not head or not is_ancestor(ctx, head, upper):
                die(f"the last email was sent from {head[:7] or '?'}, which is not on the history of {upper[:7]}; run with `from`")
            lower = head
            say(f"the last email was sent by the run created at {created}, from {head[:7]}")
        else:
            start = datetime.datetime.now(datetime.timezone.utc)
            if ctx.run_id:
                created = ctx.get(f"/repos/{ctx.repo}/actions/runs/{ctx.run_id}").get("created_at")
                if created:
                    start = parse_stamp(created)
            since = stamp(start - DEFAULT_WINDOW)
            say(f"no earlier email was found: the window is the last seven days, from {since}")

    rng = [f"{lower}..{upper}"] if lower and lower != EMPTY_TREE else [upper]
    filters = [f"--since={since}"] if since else []
    shas = git(ctx, "rev-list", "--reverse", "--no-merges", *filters, *rng).split()
    if since:
        oldest = git(ctx, "rev-list", "--reverse", *filters, *rng).split()
        lower = first_parent(ctx, oldest[0]) if oldest else upper

    sources = ctx.dir / "sources"
    shutil.rmtree(sources, ignore_errors=True)
    sources.mkdir()
    (ctx.dir / "window.txt").write_text("".join(f"{s}\n" for s in shas), encoding="utf-8")
    for stale in ("title.txt", "notes.md", "review.md", "lint.txt", "email.md", "email.html"):
        (ctx.dir / stale).unlink(missing_ok=True)
    output("count", len(shas))
    if not shas:
        say("no commit landed in the window: there is nothing to send")
        note("no commit landed since the last email; nothing to send")
        return

    first, last = shas[0], shas[-1]
    first_date = datetime.date.fromisoformat(git(ctx, "show", "-s", "--format=%cs", first).strip())
    last_date = datetime.date.fromisoformat(git(ctx, "show", "-s", "--format=%cs", last).strip())
    covered = span(first_date, last_date)
    title = f"What's new in Cavalry, {covered}"
    subject = f"Cavalry: what's new, {covered}"
    (ctx.dir / "title.txt").write_text(title + "\n", encoding="utf-8")

    log = git(ctx, "log", "--reverse", "--no-merges", "--date=short", "--format=## %h %s%n%n_%cd_%n%n%b", *filters, *rng)
    (sources / "commits.md").write_text(
        f"# The {len(shas)} commits this email covers, oldest first\n\nEach commit's message is the review record: "
        "what changed, which invariant, and the evidence.\n\n" + log, encoding="utf-8")

    added = [line[1:] for line in git(ctx, "diff", lower, upper, "--", "CHANGELOG.md").split("\n")
             if line.startswith("+") and not line.startswith("+++")]
    (sources / "changelog.md").write_text(
        "# The lines this window added to CHANGELOG.md\n\nThe changelog is written in the reader's words, one line per "
        "user-visible change, in the commit that made the change. It is the authority on what changed and why.\n\n"
        + ("\n".join(added) + "\n" if added else "No line was added to CHANGELOG.md in this window.\n"), encoding="utf-8")

    decisions = ["# The ADRs this window added or changed\n"]
    for entry in git(ctx, "diff", "--name-status", lower, upper, "--", "docs/decisions/").splitlines():
        parts = entry.split("\t")
        status, path = parts[0][:1], parts[-1]
        if status == "D" or not re.match(r"docs/decisions/\d{4}-", path):
            continue
        after = git(ctx, "show", f"{upper}:{path}", check=False) or ""
        title_line = after.split("\n", 1)[0].lstrip("# ").strip()
        status_after = adr_status(after)
        if status == "A":
            decisions.append(f"- Added: {title_line}; Status: {status_after}. {ctx.repo_url}/blob/master/{path}")
        else:
            before = git(ctx, "show", f"{lower}:{path}", check=False) or ""
            change = f", was {adr_status(before)}" if adr_status(before) != status_after else ""
            decisions.append(f"- Changed: {title_line}; Status: {status_after}{change}. {ctx.repo_url}/blob/master/{path}")
    if len(decisions) == 1:
        decisions.append("No ADR was added or changed in this window.")
    (sources / "decisions.md").write_text("\n".join(decisions) + "\n", encoding="utf-8")

    pages = git(ctx, "diff", "--name-status", lower, upper, "--", "docs/", "README.md", "crates/*/README.md", "spec/").splitlines()
    (sources / "docs.md").write_text(
        "# The documentation this window touched\n\nA = added, M = changed, D = removed, R = renamed. Link a page only "
        "as it is in the checkout today.\n\n" + ("".join(f"- {p}\n" for p in pages) or "No documentation page changed in this window.\n"),
        encoding="utf-8")

    output("first", first[:7])
    output("last", last[:7])
    output("first_date", first_date.isoformat())
    output("last_date", last_date.isoformat())
    output("title", title)
    output("subject", subject)
    say(f'{len(shas)} commit(s), {first[:7]} to {last[:7]}; the subject is "{subject}"')


def adr_status(text):
    m = re.search(r"^- \*\*Status:\*\* (.+?)\s*$", text, re.M)
    return m.group(1) if m else "unknown"


# ---- adopt -------------------------------------------------------------------------------

def adopt(ctx):
    ctx.need_dir()
    run = os.environ.get("RELEASE_NOTES_DRAFT_RUN", "")
    if not re.fullmatch(r"[0-9]+", run):
        die("RELEASE_NOTES_DRAFT_RUN is not the id of a run: digits only")
    if not os.environ.get("RELEASE_NOTES_DRAFT_DIR"):
        die("RELEASE_NOTES_DRAFT_DIR is not set: where that run's artifact was downloaded")
    from_dir = Path(os.environ["RELEASE_NOTES_DRAFT_DIR"])
    if run == ctx.run_id:
        die("a run cannot adopt its own draft")
    ours = ctx.dir / "window.txt"
    if not ours.exists():
        die(f"there is no window at {ours}: run window first")

    branch = ctx.get(f"/repos/{ctx.repo}").get("default_branch", "")
    data = ctx.get(f"/repos/{ctx.repo}/actions/runs/{run}")
    path, conclusion, head_branch = data.get("path", ""), data.get("conclusion", ""), data.get("head_branch", "")
    if not (path == f".github/workflows/{WORKFLOW_FILE}" or path.startswith(f".github/workflows/{WORKFLOW_FILE}@")):
        die(f"run {run} is not a run of {WORKFLOW_FILE}")
    if conclusion != "success":
        die(f"run {run} did not succeed ({conclusion}): its gate approved no draft")
    if not branch or head_branch != branch:
        die(f"run {run} ran on {head_branch}, not on the default branch")

    notes, review, theirs = from_dir / "notes.md", from_dir / "review.md", from_dir / "window.txt"
    if not notes.is_file() or notes.stat().st_size == 0:
        die(f"run {run} left no draft: notes.md is missing from its artifact, or empty")
    if not review.is_file():
        die(f"run {run} left no review")
    if review.read_text(encoding="utf-8").split("\n", 1)[0].strip() != "APPROVED":
        die(f"the review of run {run} does not say APPROVED")
    if not theirs.is_file():
        die(f"run {run} left no window")
    if theirs.read_text(encoding="utf-8") != ours.read_text(encoding="utf-8"):
        die(f"the draft of run {run} covers {count_lines(theirs)} commit(s) and this email covers {count_lines(ours)}, "
            "or not the same ones: write a new draft")
    shutil.copyfile(notes, ctx.dir / "notes.md")
    shutil.copyfile(review, ctx.dir / "review.md")
    say(f"the draft of run {run} is adopted: approved there, {count_lines(ours)} commit(s), the same as this email covers")


def count_lines(path):
    return len(path.read_text(encoding="utf-8").splitlines())


# ---- lint --------------------------------------------------------------------------------

def justfile_recipes(root):
    """The recipe names of the justfile: a name at column 0 followed by its parameters and a colon.
    `set x := ...` and `alias a := b` carry `:=` and are not recipes."""
    text = (root / "justfile").read_text(encoding="utf-8") if (root / "justfile").is_file() else ""
    return {m.group(1) for m in re.finditer(r"^([A-Za-z_][A-Za-z0-9_-]*)(?:[ \t]+[^:#\n]*?)?[ \t]*:(?!=)", text, re.M)}


def cavalry_commands(root):
    """The commands the `cavalry` binary lists in its usage text, read from the USAGE constant of
    crates/cavalry/src/main.rs: the text the binary prints, without building it. A command line is
    indented by two spaces and starts with the name, followed by its arguments or its description
    (`  run      start a driver`, `  record <stream id>  record ...`); flag lines start with `--`
    and continuation lines are indented further, so neither matches."""
    path = root / "crates" / "cavalry" / "src" / "main.rs"
    if not path.is_file():
        return set()
    m = re.search(r'const USAGE: &str = "(.*?)";', path.read_text(encoding="utf-8"), re.S)
    if not m:
        return set()
    return {c.group(1) for c in re.finditer(r"^ {2}([a-z][a-z-]*)(?= |$)", m.group(1), re.M)}


def toml_names(path):
    return set(re.findall(r'^name\s*=\s*"([^"]+)"', path.read_text(encoding="utf-8"), re.M)) if path.is_file() else set()


def headings(path):
    """The headings of a Markdown page, canonicalised the way GitHub slugs them, roughly."""
    text = re.sub(r"^```.*?^```[ \t]*$", "", path.read_text(encoding="utf-8"), flags=re.S | re.M)
    out = set()
    for line in text.splitlines():
        m = re.match(r"^#{1,6}\s+(.*?)\s*#*\s*$", line)
        if m:
            out.add(canon(re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", m.group(1))))
    return out


def canon(s):
    return re.sub(r"[^a-z0-9]", "", s.lower())


def split_fences(text):
    """The draft's lines in two piles: prose (fence marks left out) and the lines inside fences."""
    prose, code, mark = [], [], None
    for n, line in enumerate(text.split("\n"), 1):
        stripped = line.strip()
        if mark is None:
            m = re.match(r"^(`{3,}|~{3,})", stripped)
            if m:
                mark = m.group(1)
                continue
            prose.append((n, line))
        elif stripped and set(stripped) == {mark[0]} and len(stripped) >= len(mark):
            mark = None
        else:
            code.append((n, line))
    return prose, code


def lint(ctx, notes):
    path = Path(notes)
    if not path.is_file():
        print(f"FAIL: {notes} does not exist")
        sys.exit(1)
    text = path.read_text(encoding="utf-8")
    root = ctx.root
    fails = 0

    def fail(msg):
        nonlocal fails
        print(f"FAIL: {msg}")
        fails += 1

    def ok(msg):
        print(f"ok: {msg}")

    def lines_matching(pattern, source, flags=0):
        return [(n, line) for n, line in source if re.search(pattern, line, flags)]

    def show(hits):
        for n, line in hits:
            print(f"  line {n}: {line.strip()}")

    every = list(enumerate(text.split("\n"), 1))
    prose_lines, code_lines = split_fences(text)
    prose = "\n".join(line for _, line in prose_lines)

    # 1. Not empty, no placeholder, no "Unreleased".
    if text.strip():
        ok("the draft is not empty")
    else:
        fail("the draft is empty")
    hits = lines_matching(r"\b(TODO|TBD|FIXME|XXX)\b", every)
    if hits:
        show(hits)
        fail("the draft carries a placeholder (above)")
    else:
        ok("no placeholder")
    hits = lines_matching(r"unreleased", every, re.I)
    if hits:
        show(hits)
        fail("the draft says Unreleased (above)")
    else:
        ok("no Unreleased")

    # 2. The first line is the title the window gave.
    title = os.environ.get("RELEASE_NOTES_TITLE", "").strip()
    first = text.split("\n", 1)[0].strip()
    if not title:
        print("skip: RELEASE_NOTES_TITLE is unset, so the first line is not checked")
    elif first == f"# {title}":
        ok("the first line is the title")
    else:
        fail(f"the first line is {first!r}; it must be exactly `# {title}`")

    # 3. Every `just <recipe>` is a recipe of the justfile.
    recipes = justfile_recipes(root)
    named = set(re.findall(r"`just ([a-z][a-z0-9-]*)", prose))
    named |= {m.group(1) for _, line in code_lines for m in [re.match(r"^\s*just ([a-z][a-z0-9-]*)", line)] if m}
    for recipe in sorted(named):
        if recipe in recipes:
            ok(f"just {recipe} is a recipe")
        else:
            fail(f"just {recipe} is not a recipe of the justfile (have: {' '.join(sorted(recipes))})")

    # 4. Every `cavalry <command>` is a command the binary's usage lists.
    commands = cavalry_commands(root)
    named = set(re.findall(r"`cavalry ([a-z][a-z-]*)", prose))
    named |= {m.group(1) for _, line in code_lines for m in [re.match(r"^\s*(?:\S*/)?cavalry ([a-z][a-z-]*)", line)] if m}
    for command in sorted(named):
        if command in commands:
            ok(f"cavalry {command} is a command")
        elif not commands:
            fail(f"cavalry {command} cannot be checked: no USAGE constant was read from crates/cavalry/src/main.rs")
        else:
            fail(f"cavalry {command} is not a command of the binary (have: {' '.join(sorted(commands))})")

    # 5. Every CAVALRY_ variable is a key of config.toml.
    keys = toml_names(root / "config.toml")
    variables = {"CAVALRY_" + key.upper().replace(".", "_") for key in keys}
    for variable in sorted(set(re.findall(r"\bCAVALRY_[A-Z0-9_]+\b", text))):
        if variable in variables:
            ok(f"{variable} is a key's variable")
        else:
            fail(f"{variable} is not the environment variable of a key in config.toml (have: {' '.join(sorted(variables))})")

    # 6. Every backticked path exists, and every backticked dotted name is a counter or a key.
    counters = toml_names(root / "counters.toml")
    for token in sorted({t.strip() for t in re.findall(r"`([^`\n]+)`", prose)}):
        if " " in token or token.startswith(("/", "http://", "https://", "-")) or ".." in token:
            continue
        if ("/" in token or "." in token) and re.fullmatch(r"[A-Za-z0-9._/-]+", token) and (root / token).exists():
            ok(f"`{token}` exists in the checkout")
            continue
        if "/" in token and token.split("/")[0] in TOP_DIRS:
            fail(f"`{token}` is not a file of the checkout")
        elif re.fullmatch(r"[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)+", token):
            if token in counters or token in keys:
                ok(f"`{token}` is a counter or a key")
            else:
                fail(f"`{token}` is neither a counter in counters.toml nor a key in config.toml")

    # 7. Every ADR named has a file.
    for number in sorted({n.zfill(4) for n in re.findall(r"\bADR\s+(\d{1,4})\b", text)}):
        if list((root / "docs" / "decisions").glob(f"{number}-*.md")):
            ok(f"ADR {number} exists")
        else:
            fail(f"ADR {number} has no file under docs/decisions/")

    # 8. Every link stays in this repository and resolves to a path, and a heading, that exist.
    urls = set(re.findall(r"\]\(([^)\s]+)\)", prose))
    urls |= {u.rstrip(".,;:!?)") for u in re.findall(r"""https?://[^\s<>"'`)]+""", prose)}
    base = re.escape(ctx.repo_url)
    for url in sorted(urls):
        if not re.match(base + r"(?:$|[/#?])", url):
            fail(f"the link {url} is outside {ctx.repo_url}")
            continue
        m = re.match(base + r"/(?:blob|tree)/[^/]+/([^#?]+)(?:#(.*))?$", url)
        anchor, page = None, None
        if m:
            page = root / urllib.parse.unquote(m.group(1))
            anchor = m.group(2)
            if not page.exists():
                fail(f"the link {url} names {m.group(1)}, which is not in the checkout")
                continue
        else:
            m = re.match(base + r"/?#(.+)$", url)
            if m:
                page, anchor = root / "README.md", m.group(1)
        if anchor and page is not None and page.suffix == ".md" and page.is_file():
            if canon(urllib.parse.unquote(anchor)) not in headings(page):
                fail(f"the link {url} names a heading {page.relative_to(root)} does not have")
                continue
        ok(f"the link {url} resolves")

    # 9. Nothing that points into the history, the tracker or a person's inbox.
    hits = lines_matching(r"\b(?=[0-9a-f]*[a-f])(?=[0-9a-f]*[0-9])[0-9a-f]{7,40}\b", every)
    if hits:
        show(hits)
        fail("the draft names a commit (above)")
    else:
        ok("no commit hash")
    hits = lines_matching(r"(?i)(?<![A-Za-z])(card|ticket|issue|pull request)s? #?\d+|(?<![\w/&#])#\d+\b", every)
    if hits:
        show(hits)
        fail("the draft names a card, an issue or a pull request (above)")
    else:
        ok("no card, issue or pull request")
    hits = lines_matching(r"[A-Za-z0-9._%+-]+@([A-Za-z0-9-]+\.)+[A-Za-z]{2,}", every)
    if hits:
        show(hits)
        fail("the draft carries an address (above)")
    else:
        ok("no address")
    hits = lines_matching(r"ghp_[A-Za-z0-9]{20,}|github_pat_|sbp_[A-Za-z0-9]{20,}|sb_secret_|re_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9]{20,}|eyJ[A-Za-z0-9_-]{20,}\.eyJ|-----BEGIN ", every)
    if hits:
        show(hits)
        fail("the draft carries something secret-shaped (above)")
    else:
        ok("nothing secret-shaped")

    # 10. Bounded: the brief allows 450 words; past MAX_WORDS nothing is sent whatever the reviewer says.
    words = len(re.findall(r"\S+", "\n".join(line for n, line in prose_lines if n > 1)))
    if words > MAX_WORDS:
        fail(f"the draft is {words} words outside its fences; the brief allows 450 and this lint refuses more than {MAX_WORDS}")
    else:
        ok(f"{words} words outside the fences")

    # 11. The draft renders: the layout refuses what the shape forbids, with line numbers.
    result = subprocess.run([sys.executable, "-I", str(RENDERER), "--check", "--in", str(path)], capture_output=True, text=True)
    if result.returncode == 0 and not result.stdout.strip():
        ok("the draft renders as the email")
    else:
        for line in (result.stdout + result.stderr).splitlines():
            print(f"  {line}")
        fail("the draft does not render as the email (above)")

    if fails:
        print(f"release-notes-lint: {fails} finding(s)")
        sys.exit(1)
    print("release-notes-lint: clean")


# ---- recipients --------------------------------------------------------------------------

def sendable(address):
    """True for one plain address. The result becomes a comma-separated input of the mail step,
    so anything but one address, a display name, a second address glued on with a semicolon, is
    refused rather than cleaned. GitHub's own no-reply addresses accept no mail."""
    if ".." in address or address.lower().endswith("@users.noreply.github.com"):
        return False
    return re.fullmatch(r"[A-Za-z0-9._%+-]+@([A-Za-z0-9-]+\.)+[A-Za-z]{2,}", address) is not None


def recipients(ctx, addresses):
    ctx.need_dir()
    entries = [e for e in re.split(r"[,\s]+", re.sub(r"#[^\n]*", "", addresses)) if e]
    kept, refused, repeated = [], 0, 0
    for n, entry in enumerate(entries, 1):
        # Masked before it is judged: an entry that is refused is still somebody's address, or most of one.
        if "@" in entry:
            print(f"::add-mask::{entry}", flush=True)
        if not sendable(entry):
            refused += 1
            warn(f"entry {n} of the list is not one plain address; not used")
            continue
        address = entry.lower()
        print(f"::add-mask::{address}", flush=True)
        if address in kept:
            repeated += 1
            continue
        kept.append(address)
    path = ctx.dir / "recipients.txt"
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        f.write("".join(f"{a}\n" for a in kept))
    output("count", len(kept))
    output("bcc", ",".join(kept))
    say(f"the list has {len(entries)} entries; {len(kept)} will receive the email")
    if repeated:
        say(f"{repeated} repeat an address above and are counted once")
    if refused:
        say(f"{refused} were refused (above)")
    if not kept:
        note("the list of recipients is empty (the secret RELEASE_NOTES_RECIPIENTS); nothing to send")


# ---- finish ------------------------------------------------------------------------------

def finish(ctx):
    ctx.need_dir()
    notes = ctx.dir / "notes.md"
    if not notes.is_file() or notes.stat().st_size == 0:
        die(f"there is no draft at {notes}")
    if not (ctx.dir / "window.txt").is_file():
        die(f"there is no window at {ctx.dir / 'window.txt'}: run window first")
    if not RENDERER.is_file():
        die(f"the renderer {RENDERER} does not exist")
    footer = (
        f"Every change, in the reader's words: [CHANGELOG.md]({ctx.repo_url}/blob/master/CHANGELOG.md). "
        f"Every decision, with what lost: [docs/decisions]({ctx.repo_url}/tree/master/docs/decisions).\n\n"
        f"You are receiving this because you are on the list for the release notes of [{ctx.repo}]({ctx.repo_url}). "
        "To stop these emails, reply to this one and say so.\n")
    email = ctx.dir / "email.md"
    email.write_text(notes.read_text(encoding="utf-8").rstrip("\n") + "\n\n---\n\n" + footer, encoding="utf-8")
    today = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
    result = subprocess.run([sys.executable, "-I", str(RENDERER), "--in", str(email), "--out", str(ctx.dir / "email.html"), "--date", today],
                            capture_output=True, text=True)
    if result.returncode != 0:
        for line in (result.stdout + result.stderr).splitlines():
            print(f"  {line}")
        die("the email did not render as HTML (above); nothing is sent")
    size = (ctx.dir / "email.html").stat().st_size
    say(f"the email is {count_lines(email)} lines, footer included; the HTML is {size} bytes")


# ---- main --------------------------------------------------------------------------------

def main(argv):
    mode = argv[0] if argv else ""
    if mode not in MODES or (mode == "lint") != (len(argv) == 2) or (mode != "lint" and len(argv) != 1):
        print("usage: scripts/release-notes.py window|adopt|recipients|finish, or lint <notes.md>", file=sys.stderr)
        return 2
    # The two values nothing but this process may see leave the environment before any child starts.
    token = os.environ.pop("GH_TOKEN", "")
    addresses = os.environ.pop("RELEASE_NOTES_RECIPIENTS", "")
    os.umask(0o077)    # 0600 for recipients.txt and everything else this process creates
    ctx = Context(token)
    if mode == "window":
        window(ctx)
    elif mode == "adopt":
        adopt(ctx)
    elif mode == "lint":
        lint(ctx, argv[1])
    elif mode == "recipients":
        recipients(ctx, addresses)
    else:
        finish(ctx)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
