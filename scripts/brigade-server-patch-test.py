#!/usr/bin/env python3
"""Tests of brigade-server-patch.py, which points a checkout of Brigade's server at this
tree of Cavalry (ADR 0142; work item 362), and of the workflow that runs it each night,
.github/workflows/brigade-server.yml.

The script's own steps are run against a made-up checkout with a fake `cargo` first on
PATH. The fake answers each call from a scenario and keeps a log of them, with the
config file it found at each call: so there is no network, no toolchain and no build,
and what is held is the order of the steps, the config file's text, what the script
prints and when, and what it refuses. That cargo reads such a file, and that Brigade's
server builds with it, is what the nightly job itself shows, and no test here.

The workflow's file is read as text, since no YAML parser is in the standard library:
its triggers, its token, its cache mode, its runner, its actions and its `run` blocks
are each held to what they are, so that an edit which loosens one fails a test and is
then made knowingly. The script of its last step is run under bash, as the runner runs
it, with a fake `make`.

Standard library only; `just test-scripts` runs it.

    python3 scripts/brigade-server-patch-test.py
"""
import importlib.util
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "brigade-server-patch.py"
WORKFLOW = ROOT / ".github" / "workflows" / "brigade-server.yml"
CI = ROOT / ".github" / "workflows" / "ci.yml"
URL = "https://github.com/appshapes/cavalry"
PIN = "rev=620a2e29e04751c7a1f930132666f85b3dd58e7a"
GIT = f"git+{URL}?{PIN}"
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
NO_COMMIT = "- Brigade's server: no commit, since git does not read it as a checkout"
CAVALRY = r"- Cavalry: [0-9a-f]{40}(, with changes that no commit holds)?"

_spec = importlib.util.spec_from_file_location("brigade_server_patch", SCRIPT)
patcher = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(patcher)

FAKE_CARGO = r'''#!/usr/bin/env python3
import json, os, sys
scenario = json.load(open(os.environ["FAKE_CARGO"]))
args = sys.argv[1:]
config = os.path.join(".cargo", "config.toml")
with open(os.environ["FAKE_CARGO_LOG"], "a") as log:
    log.write(json.dumps({"args": args, "cwd": os.getcwd(),
                          "config": open(config).read() if os.path.exists(config) else None}) + "\n")
if args[0] == scenario.get("fails"):
    sys.stderr.write("error: the fake cargo was told to fail here\n")
    sys.exit(101)
if args[0] == "fetch":
    if "--locked" in args:
        sys.exit("the resolve that rewrites the lock file cannot pass --locked")
    with open("Cargo.lock", "w") as lock:
        lock.write(scenario["lock_after"])
elif args[0] == "metadata" and "--manifest-path" in args:
    print(json.dumps(scenario["tree"]))
elif args[0] == "metadata" and "--no-deps" in args:
    print(json.dumps(scenario["server"]))
elif args[0] == "metadata" and "--locked" in args:
    print(json.dumps(scenario["graph"]))
else:
    sys.exit("the fake cargo does not know this call: " + " ".join(args))
'''


def package(name, manifest, source=None, version="0.1.0", dependencies=()):
    return {"name": name, "version": version, "id": f"{name}@{version}#{source or manifest}", "source": source,
            "manifest_path": str(manifest), "dependencies": list(dependencies)}


def dependency(name, source=GIT, path=None, kind=None, rename=None):
    return {"name": name, "source": source, "path": path, "kind": kind, "rename": rename, "optional": True}


def workspace(*packages):
    return {"packages": list(packages), "workspace_members": [p["id"] for p in packages]}


def lock(*crates):
    return "version = 4\n\n" + "\n".join(
        f'[[package]]\nname = "{name}"\nversion = "{version}"\nsource = "{CRATES_IO}"\n' for name, version in crates)


def executable(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


class Made:
    """A made-up checkout of Brigade beside a made-up tree of Cavalry, and what the fake
    cargo answers about them. A test changes the scenario before it runs the script."""

    def __init__(self, test: unittest.TestCase):
        self.dir = Path(os.path.realpath(tempfile.mkdtemp(prefix="brigade-server-patch-")))
        test.addCleanup(lambda: subprocess.run(["rm", "-rf", str(self.dir)], check=False))
        self.checkout, self.server = self.dir / "brigade", self.dir / "brigade" / "server"
        self.server.mkdir(parents=True)
        (self.server / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")
        (self.server / "Cargo.lock").write_text(lock(("libc", "0.2.190"), ("nix", "0.31.3")), encoding="utf-8")
        self.tree = {name: self.dir / "cavalry" / "crates" / name / "Cargo.toml"
                     for name in ("cavalry", "cavalry-log", "cavalry-shm", "cavalry-bench")}
        self.mine = self.server / "crates" / "brigade-log" / "Cargo.toml"
        self.scenario = {
            "tree": workspace(*(package(name, manifest) for name, manifest in self.tree.items())),
            "server": workspace(package("brigade-log", self.mine, dependencies=[
                dependency("cavalry"), dependency("cavalry-log"), dependency("nix", CRATES_IO),
                dependency("brigade-core", None, str(self.server / "crates" / "brigade-core"))])),
            "graph": {"packages": [package("brigade-log", self.mine), package("nix", "/registry/nix", CRATES_IO, "0.32.0")]
                      + [package(name, self.tree[name]) for name in ("cavalry", "cavalry-log", "cavalry-shm")]},
            "lock_after": lock(("libc", "0.2.191"), ("nix", "0.31.3"), ("nix", "0.32.0"), ("cavalry", "0.1.0")),
        }
        bin_dir = self.dir / "bin"
        bin_dir.mkdir()
        executable(bin_dir / "cargo", FAKE_CARGO)
        self.path = f"{bin_dir}{os.pathsep}{os.environ['PATH']}"
        self.config = self.server / ".cargo" / "config.toml"

    def names(self, *dependencies):
        """Have the server's one package name these, and nothing else."""
        self.scenario["server"]["packages"][0]["dependencies"] = list(dependencies)

    def git(self, where: Path, *args: str) -> str:
        return subprocess.run(("git", "-C", str(where), "-c", "user.email=test@example.invalid", "-c", "user.name=test")
                              + args, check=True, capture_output=True, text=True).stdout.strip()

    def run(self, *args: str):
        scenario, log = self.dir / "scenario.json", self.dir / "calls.jsonl"
        scenario.write_text(json.dumps(self.scenario), encoding="utf-8")
        log.write_text("", encoding="utf-8")
        done = subprocess.run([sys.executable, "-I", str(SCRIPT)] + list(args or (str(self.checkout),)),
                              capture_output=True, text=True, cwd=self.dir,
                              env={"PATH": self.path, "FAKE_CARGO": str(scenario), "FAKE_CARGO_LOG": str(log)})
        calls = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]
        return done.returncode, done.stdout, done.stderr, calls


class Run(unittest.TestCase):
    def test_the_server_is_pointed_at_the_tree_in_four_cargo_calls_under_server(self):
        made = Made(self)
        code, out, err, calls = made.run()
        self.assertEqual(code, 0, err)
        self.assertEqual([c["args"][:2] for c in calls],
                         [["metadata", "--no-deps"], ["metadata", "--no-deps"], ["fetch"], ["metadata", "--locked"]])
        # Under server/, where rustup reads the checkout's toolchain file and cargo its config.
        self.assertEqual({c["cwd"] for c in calls}, {str(made.server)})
        self.assertIn(str(ROOT / "Cargo.toml"), calls[0]["args"])
        # Nothing is written until both workspaces are read, and the resolve finds the patch.
        self.assertEqual([c["config"] is not None for c in calls], [False, False, True, True])
        self.assertEqual(calls[2]["config"], made.config.read_text(encoding="utf-8"))

    def test_the_config_file_patches_each_crate_the_server_names_and_no_other(self):
        made = Made(self)
        made.run()
        text = made.config.read_text(encoding="utf-8")
        self.assertTrue(text.startswith(patcher.MARK + "\n"))
        self.assertIn(f'[patch."{URL}"]', text)
        self.assertIn(f'cavalry = {{ path = "{made.tree["cavalry"].parent}" }}', text)
        self.assertIn(f'cavalry-log = {{ path = "{made.tree["cavalry-log"].parent}" }}', text)
        # cavalry-shm follows from the two by their own path dependencies; the bench is no one's.
        self.assertNotIn("cavalry-shm", text)
        self.assertNotIn("cavalry-bench", text)
        self.assertNotIn("nix", text)

    def test_the_summary_names_both_commits_the_pin_and_what_the_lock_file_gained_and_lost(self):
        made = Made(self)
        code, out, err, _ = made.run()
        lines = out.splitlines()
        self.assertEqual(lines[0], NO_COMMIT)
        self.assertRegex(lines[1], f"^{CAVALRY}$")
        self.assertEqual(lines[2], f"- In place of what the server pins: `{PIN}`")
        self.assertEqual(lines[3], "- Taken from this tree: cavalry, cavalry-log, cavalry-shm")
        # Cavalry's own crates are left out of both lists: they are the line above.
        self.assertEqual(lines[4], "- Crates the lock file gained: libc 0.2.191, nix 0.32.0")
        self.assertEqual(lines[5], "- Crates the lock file lost: libc 0.2.190")
        self.assertEqual(len(lines), 6)

    def test_a_run_that_fails_has_still_printed_both_commits(self):
        # A red night's log names what was built, whichever step of the script failed.
        for fails, ran, printed in (("metadata", 1, 2), ("fetch", 3, 3)):
            made = Made(self)
            made.scenario["fails"] = fails
            code, out, err, calls = made.run()
            lines = out.splitlines()
            self.assertEqual((code, len(calls), len(lines)), (1, ran, printed), err)
            self.assertEqual(lines[0], NO_COMMIT)
            self.assertRegex(lines[1], f"^{CAVALRY}$")
            self.assertIn("the fake cargo was told to fail here", err)
            self.assertIn("cargo's own message is above this line", err)

    def test_the_commit_of_a_checkout_is_its_own_and_never_the_repository_above_it(self):
        made = Made(self)
        made.git(made.dir, "init", "-q")
        made.git(made.dir, "commit", "-q", "--allow-empty", "-m", "the repository above")
        self.assertEqual(made.run()[1].splitlines()[0], NO_COMMIT)
        made.git(made.checkout, "init", "-q")
        made.git(made.checkout, "commit", "-q", "--allow-empty", "-m", "the checkout")
        self.assertEqual(made.run()[1].splitlines()[0], "- Brigade's server: " + made.git(made.checkout, "rev-parse", "HEAD"))

    def test_a_config_file_the_script_did_not_write_is_refused_and_left_as_it_was(self):
        for name in ("config.toml", "config"):
            made = Made(self)
            theirs = made.server / ".cargo" / name
            theirs.parent.mkdir()
            theirs.write_text("[build]\njobs = 2\n", encoding="utf-8")
            code, out, err, calls = made.run()
            # Both commits are said first: a night that ends here still names what it built.
            self.assertEqual((code, len(out.splitlines()), calls), (1, 2, []), err)
            self.assertIn("this script did not write it", err)
            self.assertEqual(theirs.read_text(encoding="utf-8"), "[build]\njobs = 2\n")

    def test_a_second_run_writes_over_its_own_config_file(self):
        made = Made(self)
        made.run()
        made.tree["cavalry-log"] = made.dir / "cavalry" / "moved" / "cavalry-log" / "Cargo.toml"
        for part in ("tree", "graph"):
            for held in made.scenario[part]["packages"]:
                if held["name"] == "cavalry-log":
                    held["manifest_path"] = str(made.tree["cavalry-log"])
        code, _, err, _ = made.run()
        self.assertEqual(code, 0, err)
        self.assertIn(str(made.tree["cavalry-log"].parent), made.config.read_text(encoding="utf-8"))

    def test_a_crate_of_the_trees_still_taken_from_the_pin_after_the_patch_fails_the_run_by_name(self):
        made = Made(self)
        made.scenario["graph"]["packages"].append(package("cavalry-raft", "/git/cavalry-raft/Cargo.toml", GIT + "#620a2e2"))
        made.scenario["tree"] = workspace(*made.scenario["tree"]["packages"],
                                          package("cavalry-raft", made.dir / "cavalry/crates/cavalry-raft/Cargo.toml"))
        code, out, err, _ = made.run()
        self.assertEqual(code, 1)
        self.assertNotIn("Taken from this tree", out)
        self.assertIn(f'cavalry-raft is taken from "{GIT}#620a2e2"', err)

    def test_a_crate_with_a_source_is_not_the_trees_whatever_its_manifests_path(self):
        made = Made(self)
        made.scenario["graph"]["packages"][2]["source"] = CRATES_IO
        code, _, err, _ = made.run()
        self.assertEqual(code, 1)
        self.assertIn(f'cavalry is taken from "{CRATES_IO}"', err)

    def test_a_crate_the_tree_does_not_have_still_taken_from_the_patched_url_fails_the_run(self):
        # A crate renamed or removed here since the pin, which another crate at the pin still asks for.
        for source in (GIT + "#620a2e2", f"git+{URL}.git?{PIN}#620a2e2", f"git+{URL.upper()}/#620a2e2"):
            made = Made(self)
            made.scenario["graph"]["packages"].append(package("cavalry-old", "/git/cavalry-old/Cargo.toml", source))
            code, out, err, _ = made.run()
            self.assertEqual(code, 1, source)
            self.assertNotIn("Taken from this tree", out)
            self.assertIn('"cavalry-old" is taken from Cavalry\'s repository', err)

    def test_a_crate_the_server_names_from_cavalrys_repository_that_the_tree_lacks_is_refused(self):
        for url in (URL, URL + ".git", URL + "/"):
            made = Made(self)
            made.names(dependency("cavalry"), dependency("cavalry-log"), dependency("cavalry-old", f"git+{url}?{PIN}"))
            code, out, err, calls = made.run()
            self.assertEqual((code, len(calls), len(out.splitlines())), (1, 2, 2), err)
            self.assertIn('"brigade-log" takes "cavalry-old" from Cavalry\'s repository, and this tree has no crate', err)
            self.assertIn("docs/how-to/read-the-nightly-run-of-brigades-server.md", err)
            self.assertFalse(made.config.exists())

    def test_a_crate_of_another_repository_by_git_is_none_of_cavalrys(self):
        made = Made(self)
        made.names(dependency("cavalry"), dependency("cavalry-log"),
                   dependency("another", "git+https://github.com/someone/another?rev=1"))
        made.scenario["graph"]["packages"].append(
            package("another", "/git/another/Cargo.toml", "git+https://github.com/someone/another?rev=1#1"))
        self.assertEqual(made.run()[0], 0)

    def test_a_crate_of_the_trees_name_taken_from_another_copy_fails_the_run(self):
        made = Made(self)
        made.scenario["graph"]["packages"][2]["manifest_path"] = "/another/copy/cavalry/Cargo.toml"
        code, _, err, _ = made.run()
        self.assertEqual(code, 1)
        self.assertIn('cavalry is taken from "/another/copy/cavalry/Cargo.toml"', err)

    def test_a_crate_the_server_names_that_the_graph_lacks_fails_the_run(self):
        made = Made(self)
        made.scenario["graph"]["packages"] = [p for p in made.scenario["graph"]["packages"] if p["name"] != "cavalry-log"]
        code, _, err, _ = made.run()
        self.assertEqual(code, 1)
        self.assertIn("cavalry-log is not in the graph", err)

    def test_a_server_that_names_no_crate_of_the_trees_is_refused_before_anything_is_written(self):
        made = Made(self)
        made.names(dependency("nix", CRATES_IO))
        code, out, err, calls = made.run()
        self.assertEqual((code, len(calls), len(out.splitlines())), (1, 2, 2), err)
        self.assertIn("nothing to build against it", err)
        self.assertFalse(made.config.exists())

    def test_a_crate_taken_from_a_registry_or_by_a_path_is_refused_with_what_to_change(self):
        for source, path, where in ((CRATES_IO, None, f'"{CRATES_IO}"'), (None, "/elsewhere/cavalry", 'the path "/elsewhere/cavalry"')):
            made = Made(self)
            made.scenario["server"]["packages"][0]["dependencies"][0] = dependency("cavalry", source, path)
            code, out, err, _ = made.run()
            self.assertEqual((code, len(out.splitlines())), (1, 2), err)
            self.assertIn(f'"brigade-log" takes cavalry from {where}, not by git', err)
            self.assertIn("scripts/brigade-server-patch.py", err)
            self.assertFalse(made.config.exists())

    def test_a_dependency_for_tests_or_for_a_build_script_and_a_renamed_one_are_patched_as_any_other(self):
        made = Made(self)
        made.names(dependency("cavalry", kind="dev", rename="driver"), dependency("cavalry-log", kind="build"))
        code, _, err, _ = made.run()
        self.assertEqual(code, 0, err)
        text = made.config.read_text(encoding="utf-8")
        self.assertIn("\ncavalry = {", text)
        self.assertIn("\ncavalry-log = {", text)
        self.assertNotIn("driver", text)

    def test_a_checkout_with_no_lock_file_gains_every_crate_the_resolve_writes(self):
        made = Made(self)
        (made.server / "Cargo.lock").unlink()
        code, out, err, _ = made.run()
        self.assertEqual(code, 0, err)
        self.assertEqual(out.splitlines()[4:], ["- Crates the lock file gained: libc 0.2.191, nix 0.31.3, nix 0.32.0"])

    def test_a_checkout_and_a_tree_reached_through_links_are_the_ones_they_lead_to(self):
        made = Made(self)
        for manifest in made.tree.values():
            manifest.parent.mkdir(parents=True)
            manifest.write_text("[package]\n", encoding="utf-8")
        (made.dir / "linked").symlink_to(made.dir / "cavalry")
        (made.dir / "to-brigade").symlink_to(made.checkout)
        for held in made.scenario["graph"]["packages"]:
            held["manifest_path"] = held["manifest_path"].replace(str(made.dir / "cavalry"), str(made.dir / "linked"))
        code, _, err, calls = made.run(str(made.dir / "to-brigade"))
        self.assertEqual(code, 0, err)
        self.assertEqual({c["cwd"] for c in calls}, {str(made.server)})

    def test_a_pin_or_a_url_that_would_start_a_line_of_its_own_is_refused_and_not_printed(self):
        # The summary goes to a workflow's log, where a line that begins `::` is a command.
        for source in (f"git+{URL}?rev=abc\n::error::made up", f"git+{URL}\n::warning::x?{PIN}", f"git+{URL}?rev=a b",
                       f"git+{URL}?rev=abc\n"):
            made = Made(self)
            made.scenario["server"]["packages"][0]["dependencies"][0] = dependency("cavalry", source)
            code, out, err, calls = made.run()
            self.assertEqual((code, len(calls), len(out.splitlines())), (1, 2, 2), err)
            self.assertIn("a source this script does not read", err)
            self.assertNotRegex(out + err, r"(?m)^::")
            self.assertFalse(made.config.exists())

    def test_what_a_refusal_quotes_from_the_checkout_starts_no_line_of_its_own(self):
        made = Made(self)
        made.scenario["server"]["packages"][0]["name"] = "brigade-log\n::error::made up"
        made.names(dependency("cavalry", None, "/a/path\n::error::made up"))
        code, out, err, _ = made.run()
        self.assertEqual(code, 1)
        self.assertNotRegex(out + err, r"(?m)^::")
        self.assertEqual(len(err.splitlines()), 1)

    def test_a_path_that_is_no_checkout_of_brigade_and_a_missing_argument_are_told_apart(self):
        made = Made(self)
        code, out, err, calls = made.run(str(made.dir / "bin"))
        self.assertEqual((code, out, calls), (1, "", []))
        self.assertIn("holds no Cargo.toml", err)
        for args in ((), ("--help",), ("one", "two")):
            done = subprocess.run([sys.executable, "-I", str(SCRIPT)] + list(args), capture_output=True, text=True)
            self.assertEqual((done.returncode, done.stdout), (2, ""))
            self.assertIn("usage:", done.stderr)


class Parts(unittest.TestCase):
    def test_a_path_with_a_quote_a_backslash_and_a_space_is_one_toml_string(self):
        ours = {"cavalry": Path('/a "b"\\c d/cavalry/Cargo.toml')}
        text = patcher.patch({"cavalry": (URL, PIN)}, ours)
        self.assertIn('cavalry = { path = "/a \\"b\\"\\\\c d/cavalry" }', text)

    def test_two_git_urls_get_a_table_each(self):
        ours = {name: Path(f"/tree/{name}/Cargo.toml") for name in ("cavalry", "cavalry-log")}
        text = patcher.patch({"cavalry": (URL, PIN), "cavalry-log": (URL + ".git", "")}, ours)
        self.assertEqual(text.count("[patch."), 2)
        self.assertLess(text.index(f'[patch."{URL}"]'), text.index("cavalry = "))
        self.assertLess(text.index(f'[patch."{URL}.git"]'), text.index("cavalry-log = "))

    def test_one_crate_taken_from_two_places_is_refused(self):
        ours = {"cavalry": Path("/tree/cavalry/Cargo.toml")}
        server = workspace(package("a", "/s/a/Cargo.toml", dependencies=[dependency("cavalry")]),
                           package("b", "/s/b/Cargo.toml", dependencies=[dependency("cavalry", f"git+{URL}?branch=master")]))
        with self.assertRaisesRegex(patcher.Refused, "from two places"):
            patcher.taken(server, ours)

    def test_a_dependency_of_a_crate_outside_the_servers_workspace_is_not_read(self):
        ours = {"cavalry": Path("/tree/cavalry/Cargo.toml")}
        server = workspace(package("a", "/s/a/Cargo.toml", dependencies=[dependency("cavalry")]))
        server["packages"].append(package("stranger", "/x/Cargo.toml", CRATES_IO, dependencies=[dependency("cavalry", CRATES_IO)]))
        self.assertEqual(patcher.taken(server, ours), {"cavalry": (URL, PIN)})

    def test_a_lock_file_is_read_as_names_and_versions_made_of_a_crates_characters(self):
        self.assertEqual(patcher.locked(lock(("nix", "0.31.3"), ("nix", "0.32.0"))), {("nix", "0.31.3"), ("nix", "0.32.0")})
        self.assertEqual(patcher.locked(""), set())
        # What is not a crate's name or version is not read, so it is never printed: an
        # escape, a line of its own, and a name that a Markdown page would make a link.
        odd = lock(("a\\u001b[31m", "1.0.0"), ("b", "1 ::x"), ("www.example.com", "1.0.0"), ("d", "www.example.com"))
        self.assertEqual(patcher.locked(odd + lock(("c", "1.0.0-rc.1+build"))), {("c", "1.0.0-rc.1+build")})

    def test_a_url_is_the_same_with_a_slash_or_dot_git_after_it_and_in_any_case(self):
        self.assertEqual({patcher.same(u) for u in (URL, URL + "/", URL + ".git", URL.upper(), URL + ".GIT/")}, {URL})
        self.assertNotEqual(patcher.same(URL), patcher.same(URL + "-two"))

    def test_a_source_that_is_not_gits_has_no_git_url(self):
        self.assertEqual(patcher.git_url(GIT + "#620a2e2"), URL)
        self.assertEqual(patcher.git_url(f"git+{URL}#620a2e2"), URL)
        for source in (CRATES_IO, None, "", f"path+file:///x?{URL}"):
            self.assertIsNone(patcher.git_url(source))


def code_of(path: Path) -> str:
    """A workflow's lines that are not comments: a comment says what the job does not
    do, in the very words a test looks for."""
    return "\n".join(line for line in path.read_text(encoding="utf-8").splitlines() if not line.lstrip().startswith("#")) + "\n"


def block(code: str, key: str) -> str:
    """A top-level key of a workflow with everything under it, as it stands."""
    return re.search(rf"^{key}:.*\n(?:[ \t]+.*\n|\n)*", code, re.M).group(0).rstrip("\n") + "\n"


def step_script(code: str, name: str) -> str:
    """The `run: |` block of the step called `name`, with its indentation taken off."""
    lines = code.split("\n")
    at = lines.index(f"      - name: {name}")
    run = next(i for i in range(at + 1, len(lines)) if lines[i].strip().startswith(("run:", "- ")))
    if lines[run].strip() != "run: |":
        raise AssertionError(f"the step {name!r} has no `run: |` block")
    indent = len(lines[run]) - len(lines[run].lstrip()) + 2
    body = []
    for line in lines[run + 1:]:
        if line.strip() and len(line) - len(line.lstrip()) < indent:
            break
        body.append(line[indent:])
    return "\n".join(body).rstrip("\n") + "\n"


class Workflow(unittest.TestCase):
    """What the nightly job's file holds, read as text by the shape this repository's
    workflows keep. Each test names one thing the job is given or not given."""

    @classmethod
    def setUpClass(cls):
        cls.code = code_of(WORKFLOW)
        cls.jobs = cls.code.split("\njobs:\n", 1)[1]

    def test_it_runs_each_night_and_by_hand_and_for_nothing_else(self):
        self.assertEqual(block(self.code, "on"), "on:\n  schedule:\n    - cron: '43 6 * * *'\n  workflow_dispatch:\n")
        self.assertEqual(re.findall(r"^([\w-]+):", self.code, re.M),
                         ["name", "on", "permissions", "cache-mode", "concurrency", "jobs"])

    def test_no_job_carries_the_name_of_a_check_that_ci_reports(self):
        names = re.findall(r"^  ([\w-]+):\n", self.jobs, re.M) + re.findall(r"^    name: (.+)$", self.jobs, re.M)
        self.assertEqual(names, ["brigade-server", "brigade server"])
        ci = code_of(CI).split("\njobs:\n", 1)[1]
        taken = re.findall(r"^  ([\w-]+):\n", ci, re.M) + ["gates (linux)", "gates (macos)"]
        for name in names:
            self.assertNotIn(name, taken)
            self.assertNotIn("gates", name)

    def test_it_holds_a_token_that_reads_and_no_secret(self):
        self.assertEqual(block(self.code, "permissions"), "permissions:\n  contents: read\n")
        self.assertEqual(self.code.count("permissions"), 1)
        for word in ("secrets", "token", "TOKEN", "env:"):
            self.assertNotIn(word, self.code)
        # One expression in the whole file, and it reads the ref for the concurrency group.
        self.assertEqual(re.findall(r"\$\{\{(.*?)\}\}", self.code), [" github.ref "])

    def test_no_step_can_restore_or_save_a_cache_and_the_runner_keeps_none(self):
        self.assertEqual(block(self.code, "cache-mode"), "cache-mode: none\n")
        self.assertEqual(self.code.count("cache-mode"), 1)
        # A runner of GitHub's own: a Blacksmith runner shares a Go build cache between a repository's jobs.
        self.assertEqual(re.findall(r"runs-on: (.+)", self.code), ["ubuntu-24.04"])

    def test_its_actions_are_two_checkouts_and_go_pinned_to_the_commits_ci_pins(self):
        uses = re.findall(r"uses: (\S+)@(\S+)(.*)", self.code)
        self.assertEqual([action for action, _, _ in uses], ["actions/checkout", "actions/checkout", "actions/setup-go"])
        ci = dict(re.findall(r"uses: (\S+)@([0-9a-f]{40}) # v", code_of(CI)))
        for action, commit, rest in uses:
            self.assertRegex(commit, r"^[0-9a-f]{40}$")
            self.assertRegex(rest, r"^ # v\d")
            self.assertEqual(commit, ci[action])

    def test_each_checkout_leaves_no_credential_and_brigades_is_its_master(self):
        steps = re.split(r"\n      - ", self.jobs)[1:]
        # What follows each `uses:` line. The pins themselves are the test before this one's.
        given = [step.split("\n", 1)[1] for step in steps[:3]]
        self.assertEqual(given[0], "        with:\n          path: cavalry\n          persist-credentials: false")
        self.assertEqual(given[1], "        with:\n          repository: appshapes/brigade\n          ref: master\n"
                                   "          path: brigade\n          persist-credentials: false")
        self.assertEqual(given[2], "        with:\n          go-version-file: brigade/go.mod\n          cache: false")
        self.assertEqual(len(steps), 6)

    def test_no_step_is_skipped_and_no_failure_is_passed_over(self):
        for word in ("continue-on-error", "if:", "|| true\n", "always()"):
            self.assertNotIn(word, self.code.replace("\"$RUNNER_TEMP/server-test.log\" || true)", ""))
        self.assertEqual(step_script(self.code, "Rust toolchain"),
                         "rustup --version\nrustup toolchain install || rustup show\ncargo --version\n")
        self.assertEqual(step_script(self.code, "Point Brigade's server at this tree"),
                         "echo \"### Brigade's server against this tree\" >> \"$GITHUB_STEP_SUMMARY\"\n"
                         "python3 cavalry/scripts/brigade-server-patch.py brigade | tee -a \"$GITHUB_STEP_SUMMARY\"\n")
        self.assertTrue(step_script(self.code, "Brigade's tests of its server").startswith(
            "make server-test 2>&1 | tee \"$RUNNER_TEMP/server-test.log\"\n"))
        # `tee` would hide a failure from a shell without pipefail, which `shell: bash` sets.
        steps = re.split(r"\n      - ", self.jobs)[1:]
        for step in steps[4:]:
            self.assertIn("\n        shell: bash\n", step)
        self.assertIn("\n        working-directory: brigade/server\n", steps[3])
        self.assertIn("\n        working-directory: brigade\n", steps[5])

    def test_a_test_that_does_not_return_ends_as_a_failed_step_before_the_job_is_cancelled(self):
        bounds = [int(n) for n in re.findall(r"timeout-minutes: (\d+)", self.jobs)]
        self.assertEqual(len(bounds), 2)
        job, step = bounds
        self.assertLess(step, job)
        self.assertIn(f"\n        timeout-minutes: {step}\n", re.split(r"\n      - ", self.jobs)[6])


class Step(unittest.TestCase):
    """The script of the workflow's last step, run under bash as the runner runs a step
    with `shell: bash`, with a fake `make` that prints a log and ends as it is told."""

    def run_step(self, log: str, ends: int = 0, grep: str = ""):
        directory = Path(tempfile.mkdtemp(prefix="brigade-server-step-"))
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(directory)], check=False))
        if grep:
            executable(directory / "grep", grep)
        executable(directory / "make", "#!/bin/sh\n[ \"$1\" = server-test ] || exit 64\ncat \"$FAKE_MAKE_LOG\"\n"
                                       "echo 'a line on stderr' >&2\nexit \"$FAKE_MAKE_ENDS\"\n")
        (directory / "log").write_text(log, encoding="utf-8")
        (directory / "step.sh").write_text(step_script(code_of(WORKFLOW), "Brigade's tests of its server"), encoding="utf-8")
        summary = directory / "summary.md"
        done = subprocess.run(["bash", "--noprofile", "--norc", "-e", "-o", "pipefail", str(directory / "step.sh")],
                              capture_output=True, text=True, cwd=directory,
                              env={"PATH": f"{directory}{os.pathsep}{os.environ['PATH']}", "RUNNER_TEMP": str(directory),
                                   "GITHUB_STEP_SUMMARY": str(summary), "FAKE_MAKE_LOG": str(directory / "log"),
                                   "FAKE_MAKE_ENDS": str(ends)})
        return done.returncode, done.stdout, summary.read_text(encoding="utf-8") if summary.exists() else ""

    FEED = ("test a_team_stays_owned_across_a_kill_on_the_cavalry_feed ... ok\n"
            "test the_conformance_suite_is_green_on_the_cavalry_feed ... ok\n")

    def test_a_green_make_with_tests_on_the_feed_passes_and_says_how_many(self):
        code, out, summary = self.run_step("test on_the_file_log ... ok\n" + self.FEED + "ok  \tgithub.com/x\t1.0s\n")
        self.assertEqual(code, 0, out)
        self.assertEqual(summary, "- Tests named for the Cavalry feed that passed: 2\n")
        self.assertIn("test on_the_file_log ... ok\n", out)
        self.assertIn("a line on stderr\n", out)

    def test_a_make_that_fails_fails_the_step_though_tee_ended_well(self):
        code, out, summary = self.run_step(self.FEED + "test another ... FAILED\n", ends=2)
        self.assertEqual((code, summary), (2, ""))
        self.assertIn("test another ... FAILED\n", out)

    def test_a_count_that_is_no_number_fails_the_step(self):
        # A `grep` that could not read the log prints nothing: the step must not pass on that.
        code, out, summary = self.run_step(self.FEED, grep="#!/bin/sh\nexit 2\n")
        self.assertEqual(code, 1, out)
        self.assertRegex(out, r"(?m)^::error::make server-test passed, and no test named for the Cavalry feed ran\.")

    def test_a_green_make_whose_tests_on_the_feed_did_not_run_fails_the_step(self):
        for log in ("test on_the_file_log ... ok\n",
                    "test the_conformance_suite_is_green_on_the_cavalry_feed ... ignored, needs the Go side\n",
                    "test a_test_on_the_cavalry_feed ... FAILED\n",
                    "running the tests of the cavalry_feed ... ok, they say\n"):
            code, out, summary = self.run_step(log)
            self.assertEqual(code, 1, log)
            self.assertEqual(summary, "- Tests named for the Cavalry feed that passed: 0\n")
            self.assertRegex(out, r"(?m)^::error::make server-test passed, and no test named for the Cavalry feed ran\.")


if __name__ == "__main__":
    unittest.main()
