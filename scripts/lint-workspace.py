#!/usr/bin/env python3
"""Workspace lint for Cavalry. `just lint-workspace` runs it; any finding exits 1.

It enforces the parts of CLAUDE.md that rustc and clippy cannot see on their own.
Every crate's Cargo.toml declares `[package.metadata.cavalry] data_plane = true|false`.
"Data plane" is the CLAUDE.md definition: code a message passes through between
`try_claim` on the publisher and the fragment handler on the subscriber. This script
reads `cargo metadata` and runs seven checks:

  1. flags       every crate declares data_plane; a data-plane crate also declares
                 allowed-dependencies.
  2. deps        a data-plane crate's normal and build dependencies are workspace
                 members or listed in allowed-dependencies (CLAUDE.md, "Working rules
                 for agents": the dependency allowlist). Dev-dependencies are exempt:
                 test code is not on the hot path.
  3. planes      a data-plane crate never depends on a non-data-plane crate, so a
                 control-plane dependency cannot reach the hot path through a
                 workspace crate.
  4. async       no `async` and no `.await` under a data-plane crate's src/
                 (CLAUDE.md, invariant 2; .claude/rules/data-plane.md). Comments and
                 string literals are ignored.
  5. lints       every crate except cavalry-shm and cavalry-ffi inherits the workspace
                 lints, which deny unsafe_code; those two carry their own [lints.rust]
                 table (CLAUDE.md, "Unsafe").
  6. clippy      the root clippy.toml bans locks on the data plane. Clippy reads the
                 nearest clippy.toml per crate and does not merge, so exactly the
                 non-data-plane crates override it with a clippy.toml of their own.
                 This check keeps the overrides and the flags in agreement.
  7. re-exports  crates/cavalry/src/lib.rs holds only `pub use` items, attributes and
                 docs: the crate never grows logic of its own (ADR 0002).

Python 3.9 standard library only, so it runs on a laptop and on both CI runners
without anything to install.
"""

import json
import re
import subprocess
import sys
from pathlib import Path

UNSAFE_CRATES = {"cavalry-shm", "cavalry-ffi"}
REEXPORT_ONLY_CRATE = "cavalry"
CLIPPY_FILES = ("clippy.toml", ".clippy.toml")

findings = []


def find(path, line, message):
    where = f"{path}:{line}" if line else str(path)
    findings.append(f"{where}: {message}")


def blank(text):
    """Replace every character except newlines with a space, so offsets keep their line."""
    return re.sub(r"[^\n]", " ", text)


def strip_rust(src):
    """Blank out comments, string literals and char literals; line numbers are preserved."""
    out = []
    i, n = 0, len(src)
    while i < n:
        if src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            out.append(blank(src[i:j]))
            i = j
        elif src.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif src.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            out.append(blank(src[i:j]))
            i = j
        else:
            raw = re.match(r'b?r(#*)"', src[i : i + 12])
            boundary = i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")
            if raw and boundary:
                hashes = raw.group(1)
                j = src.find('"' + hashes, i + raw.end())
                j = n if j < 0 else j + 1 + len(hashes)
                out.append(blank(src[i:j]))
                i = j
            elif src[i] == '"' or (src.startswith('b"', i) and boundary):
                j = i + (2 if src[i] == "b" else 1)
                while j < n and src[j] != '"':
                    j += 2 if src[j] == "\\" else 1
                j = min(j + 1, n)
                out.append(blank(src[i:j]))
                i = j
            else:
                ch = re.match(r"'(\\.[^']*|[^\\'])'", src[i : i + 16]) if src[i] == "'" else None
                if ch:
                    out.append(blank(ch.group(0)))
                    i += ch.end()
                else:
                    out.append(src[i])
                    i += 1
    return "".join(out)


def strip_attributes(src):
    """Blank out `#[...]` and `#![...]` attributes, brackets balanced."""
    out = []
    i, n = 0, len(src)
    while i < n:
        if src.startswith("#[", i) or src.startswith("#![", i):
            j = src.index("[", i)
            depth = 0
            while j < n:
                if src[j] == "[":
                    depth += 1
                elif src[j] == "]":
                    depth -= 1
                    if depth == 0:
                        j += 1
                        break
                j += 1
            out.append(blank(src[i:j]))
            i = j
        else:
            out.append(src[i])
            i += 1
    return "".join(out)


def line_of(text, offset):
    return text.count("\n", 0, offset) + 1


def main():
    root = Path(__file__).resolve().parent.parent
    metadata = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"],
            cwd=root,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    packages = {p["name"]: p for p in metadata["packages"]}
    members = set(packages)

    def manifest(name):
        return Path(packages[name]["manifest_path"])

    def rel(path):
        return Path(path).resolve().relative_to(root)

    # 1. flags
    flags = {}
    for name, pkg in packages.items():
        cavalry = (pkg.get("metadata") or {}).get("cavalry") or {}
        data_plane = cavalry.get("data_plane")
        if not isinstance(data_plane, bool):
            find(
                rel(manifest(name)),
                None,
                "declare [package.metadata.cavalry] data_plane = true or false "
                "(CLAUDE.md, Data plane); every crate must say which plane it is on",
            )
            continue
        flags[name] = data_plane
        if data_plane and not isinstance(cavalry.get("allowed-dependencies"), list):
            find(
                rel(manifest(name)),
                None,
                "a data-plane crate must declare allowed-dependencies = [...] under "
                "[package.metadata.cavalry] (CLAUDE.md, Working rules for agents)",
            )

    # 2. deps and 3. planes
    for name, pkg in packages.items():
        if not flags.get(name):
            continue
        allowed = set(pkg["metadata"]["cavalry"].get("allowed-dependencies") or [])
        for dep in pkg["dependencies"]:
            if dep.get("kind") == "dev":
                continue
            dep_name = dep["name"]
            if dep_name in members:
                if flags.get(dep_name) is False:
                    find(
                        rel(manifest(name)),
                        None,
                        f"data-plane crate depends on non-data-plane crate `{dep_name}` "
                        "(CLAUDE.md, Data plane); move the code it needs to a data-plane "
                        "crate or take it off the hot path",
                    )
            elif dep_name not in allowed:
                find(
                    rel(manifest(name)),
                    None,
                    f"dependency `{dep_name}` is not in allowed-dependencies "
                    "(CLAUDE.md, Working rules for agents); ask before adding it, "
                    "justify it in the commit message, then list it",
                )

    # 4. async
    for name in sorted(n for n, dp in flags.items() if dp):
        src_dir = manifest(name).parent / "src"
        for path in sorted(src_dir.rglob("*.rs")):
            clean = strip_rust(path.read_text(encoding="utf-8"))
            for m in re.finditer(r"\basync\b|\.await\b", clean):
                find(
                    rel(path),
                    line_of(clean, m.start()),
                    f"`{m.group(0)}` on the data plane (CLAUDE.md, invariant 2; "
                    ".claude/rules/data-plane.md); the data plane is synchronous, "
                    "use a dedicated thread and an idle strategy",
                )

    # 5. lints
    inherit = re.compile(r"^\[lints\]\s*\nworkspace\s*=\s*true\s*$", re.M)
    own_table = re.compile(r"^\[lints\.rust\]\s*$", re.M)
    for name in packages:
        text = manifest(name).read_text(encoding="utf-8")
        inherits = inherit.search(text) is not None
        if name in UNSAFE_CRATES:
            if inherits or not own_table.search(text):
                find(
                    rel(manifest(name)),
                    None,
                    "cavalry-shm and cavalry-ffi carry their own [lints.rust] table "
                    "instead of [lints] workspace = true, because the workspace table "
                    "denies unsafe_code (CLAUDE.md, Unsafe)",
                )
        elif not inherits:
            find(
                rel(manifest(name)),
                None,
                "add [lints] workspace = true; unsafe_code is denied outside "
                "cavalry-shm and cavalry-ffi (CLAUDE.md, Unsafe)",
            )
    root_manifest = (root / "Cargo.toml").read_text(encoding="utf-8")
    section = re.search(r"^\[workspace\.lints\.rust\]\s*$(.*?)(?=^\[|\Z)", root_manifest, re.M | re.S)
    if not section or not re.search(r'^unsafe_code\s*=\s*"deny"', section.group(1), re.M):
        find(
            "Cargo.toml",
            None,
            '[workspace.lints.rust] must contain unsafe_code = "deny" (CLAUDE.md, Unsafe)',
        )

    # 6. clippy
    root_clippy = root / "clippy.toml"
    if not root_clippy.exists() or "std::sync::Mutex" not in root_clippy.read_text(encoding="utf-8"):
        find(
            "clippy.toml",
            None,
            "the root clippy.toml must list std::sync::Mutex under disallowed-types "
            "(CLAUDE.md, invariant 2: no lock on the data plane)",
        )
    for name, data_plane in sorted(flags.items()):
        crate_dir = manifest(name).parent
        overrides = [f for f in CLIPPY_FILES if (crate_dir / f).exists()]
        if data_plane and overrides:
            find(
                rel(crate_dir / overrides[0]),
                None,
                "a data-plane crate must not override the root clippy.toml; delete "
                "this file so the lock ban applies (CLAUDE.md, invariant 2)",
            )
        if not data_plane and not overrides:
            find(
                rel(manifest(name)),
                None,
                "a non-data-plane crate needs its own clippy.toml next to Cargo.toml "
                "to lift the root lock ban; copy crates/cavalry-control/clippy.toml",
            )

    # 7. re-exports
    lib = manifest(REEXPORT_ONLY_CRATE).parent / "src" / "lib.rs"
    clean = strip_attributes(strip_rust(lib.read_text(encoding="utf-8")))
    pub_use = re.compile(r"pub\s+use\s+[\w:{},\s*]+;")
    for item in re.finditer(r"[^;]+;|[^;]+$", clean):
        text = item.group(0).strip()
        if text and not pub_use.fullmatch(text):
            start = item.start() + (len(item.group(0)) - len(item.group(0).lstrip()))
            find(
                rel(lib),
                line_of(clean, start),
                "only `pub use` re-exports, attributes and docs belong in this file; "
                "the cavalry crate never grows logic of its own (ADR 0002). Put the "
                "code in the crate it belongs to and re-export it",
            )

    if findings:
        print("\n".join(findings))
        print(
            f"lint-workspace: {len(findings)} finding(s). Fix each one; ask before "
            "weakening a gate (CLAUDE.md, Working rules for agents).",
            file=sys.stderr,
        )
        return 1
    data_plane_count = sum(1 for dp in flags.values() if dp)
    print(f"lint-workspace: {len(packages)} crates, {data_plane_count} data plane, 7 checks, ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
