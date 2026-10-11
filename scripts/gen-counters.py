#!/usr/bin/env python3
"""gen-counters: the counters gate CLAUDE.md names for invariant 4 (ADR 0012).

counters.toml at the repository root is the single source of every counter. This script
checks it and writes the two files generated from it:

  docs/reference/counters.md                     the reference page, one row per counter
  crates/cavalry-shm/src/counters/generated.rs   one `CounterId` constant per counter and
                                                 the `COUNTERS` table

Two modes:

  gen-counters.py            check counters.toml and write both files; `just gen-counters`
  gen-counters.py --check    check counters.toml and fail when either file differs from
                             what would be written; `just lint-counters`, what CI runs

Checks on counters.toml:

  schema   `schema = 0`, the only schema this script knows
  keys     every [[counter]] has exactly id, name, kind, scope, doc and doctor
  id       an integer from 0 to 2^31 - 1 (the record's type id is an i32), unique
  name     lowercase, dotted (`layout.what_moved`), unique, at most 124 bytes (a static
           counter's label is its name), and its Rust constant name is unique
  kind     "counter" or "gauge"
  scope    "static" or "instance"
  doc      a non-empty string
  doctor   a string, empty when there is nothing for an operator to do

Python 3.9 standard library only, like the other scripts (ADR 0003): `tomllib` when the
interpreter has it (3.11 and later), else a parser for the flat subset of TOML that
counters.toml uses: `key = value` with integers and basic strings, `[[counter]]`
headers, blank lines and comments. The Rust output is written in rustfmt's shape and
run through rustfmt when it is installed, so `cargo fmt --check` passes either way.
"""

from __future__ import annotations

import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "counters.toml"
PAGE = ROOT / "docs" / "reference" / "counters.md"
RUST = ROOT / "crates" / "cavalry-shm" / "src" / "counters" / "generated.rs"

SCHEMA = 0
KEYS = ("id", "name", "kind", "scope", "doc", "doctor")
KINDS = ("counter", "gauge")
SCOPES = ("static", "instance")
MAX_ID = 2**31 - 1
MAX_LABEL_LENGTH = 124
NAME = re.compile(r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$")


class Fail(Exception):
    """A finding that stops the run; the message says which entry and what to fix."""


# ---- reading counters.toml --------------------------------------------------------

def load(text: str) -> dict:
    try:
        import tomllib  # Python 3.11+
    except ImportError:
        return parse_flat(text)
    try:
        return tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        raise Fail(f"counters.toml: {e}") from None


def parse_flat(text: str) -> dict:
    """The subset counters.toml uses: top-level `key = value`, `[[counter]]` tables,
    integers, basic strings with \\\" \\\\ \\n \\t escapes, comments and blank lines."""
    doc: dict = {"counter": []}
    table: dict | None = None
    for number, raw in enumerate(text.splitlines(), 1):
        line = strip_comment(raw).strip()
        if not line:
            continue
        if line == "[[counter]]":
            table = {}
            doc["counter"].append(table)
            continue
        m = re.match(r"^([A-Za-z0-9_-]+)\s*=\s*(.+)$", line)
        if not m:
            raise Fail(f"counters.toml:{number}: not `key = value` or `[[counter]]`; this "
                       "parser reads the flat subset the file uses (scripts/gen-counters.py)")
        key, value = m.group(1), parse_value(m.group(2), number)
        target = doc if table is None else table
        if key in target:
            raise Fail(f"counters.toml:{number}: `{key}` is set twice in the same table")
        target[key] = value
    return doc


def strip_comment(line: str) -> str:
    quoted = False
    escaped = False
    for i, ch in enumerate(line):
        if escaped:
            escaped = False
        elif ch == "\\" and quoted:
            escaped = True
        elif ch == '"':
            quoted = not quoted
        elif ch == "#" and not quoted:
            return line[:i]
    return line


def parse_value(text: str, number: int):
    text = text.strip()
    if re.fullmatch(r"[+-]?\d+", text):
        return int(text)
    if len(text) >= 2 and text[0] == '"' and text[-1] == '"':
        body = text[1:-1]
        out = []
        i = 0
        while i < len(body):
            ch = body[i]
            if ch == "\\":
                i += 1
                if i >= len(body):
                    raise Fail(f"counters.toml:{number}: a string ends in a backslash")
                out.append({'"': '"', "\\": "\\", "n": "\n", "t": "\t"}.get(body[i], None)
                           or fail(f"counters.toml:{number}: unknown escape \\{body[i]}"))
            elif ch == '"':
                raise Fail(f"counters.toml:{number}: a quote inside a string must be \\\"")
            else:
                out.append(ch)
            i += 1
        return "".join(out)
    raise Fail(f"counters.toml:{number}: `{text}` is not an integer or a \"string\"")


def fail(message: str):
    raise Fail(message)


# ---- checking ---------------------------------------------------------------------

def check(doc: dict) -> list[dict]:
    if doc.get("schema") != SCHEMA:
        raise Fail(f"counters.toml: schema = {doc.get('schema')!r}; this script knows "
                   f"schema {SCHEMA} (spec/counters-file.md, `counters.toml`)")
    for key in doc:
        if key not in ("schema", "counter"):
            raise Fail(f"counters.toml: top-level key `{key}` is not `schema` or `[[counter]]`")
    counters = doc.get("counter") or []
    if not isinstance(counters, list) or not counters:
        raise Fail("counters.toml: no [[counter]] entries")
    ids: dict[int, str] = {}
    names: set[str] = set()
    consts: dict[str, str] = {}
    for n, c in enumerate(counters, 1):
        where = f"counters.toml: [[counter]] {n} ({c.get('name', 'unnamed')!r})"
        missing = [k for k in KEYS if k not in c]
        extra = [k for k in c if k not in KEYS]
        if missing or extra:
            raise Fail(f"{where}: every entry has exactly id, name, kind, scope, doc and doctor"
                       + (f"; missing {missing}" if missing else "")
                       + (f"; unknown {extra}" if extra else ""))
        cid, name, kind, scope, doc_, doctor = (c[k] for k in KEYS)
        if not isinstance(cid, int) or isinstance(cid, bool) or not 0 <= cid <= MAX_ID:
            raise Fail(f"{where}: id must be an integer from 0 to {MAX_ID}, the record's "
                       "i32 type id; got {cid!r}")
        if cid in ids:
            raise Fail(f"{where}: id {cid} is already {ids[cid]!r}; ids are never reused")
        ids[cid] = name
        if not isinstance(name, str) or not NAME.match(name):
            raise Fail(f"{where}: name must be lowercase and dotted, the layout or component "
                       f"first, like `counters.allocated`; got {name!r}")
        if len(name.encode()) > MAX_LABEL_LENGTH:
            raise Fail(f"{where}: name is {len(name.encode())} bytes; a label holds "
                       f"{MAX_LABEL_LENGTH} (spec/counters-file.md, Metadata record)")
        if name in names:
            raise Fail(f"{where}: name {name!r} is used twice")
        names.add(name)
        const = const_name(name)
        if const in consts:
            raise Fail(f"{where}: constant {const} is also {consts[const]!r}'s; rename one")
        consts[const] = name
        if kind not in KINDS:
            raise Fail(f"{where}: kind must be one of {list(KINDS)}; got {kind!r}")
        if scope not in SCOPES:
            raise Fail(f"{where}: scope must be one of {list(SCOPES)}; got {scope!r}")
        if not isinstance(doc_, str) or not doc_.strip():
            raise Fail(f"{where}: doc must be one sentence, what a change in the value means")
        if not isinstance(doctor, str):
            raise Fail(f"{where}: doctor must be a string: what an operator does, or \"\"")
    return counters


def const_name(name: str) -> str:
    return name.upper().replace(".", "_")


# ---- rendering --------------------------------------------------------------------

def md_cell(text: str) -> str:
    return text.replace("|", "\\|").replace("\n", " ").strip()


def render_page(counters: list[dict]) -> str:
    lines = [
        "<!-- Generated from counters.toml by scripts/gen-counters.py. Do not edit this page: "
        "change counters.toml and run `just gen-counters`. -->",
        "# Counters",
        "",
        "Every counter Cavalry has, from `counters.toml`, the single source "
        "([ADR 0012](../decisions/0012-lay-out-the-counters-file.md)). The driver publishes "
        "each one in the [counters file](counters-file.md). A static counter is allocated "
        "when the file is created, in the record whose index is its id, and its label is "
        "its name. An instance counter is allocated per key when its instance is created, "
        "and its label is the name, a space, and the key's text as the doc column defines "
        "it. A `counter` never decreases while allocated; a `gauge` moves both ways. This "
        "page is generated by `scripts/gen-counters.py`: edit `counters.toml`, never this "
        "page, and `just lint-counters` fails when the two differ.",
        "",
        "| Id | Name | Kind | Scope | What a change means | What to do |",
        "| --- | --- | --- | --- | --- | --- |",
    ]
    for c in counters:
        lines.append(f"| {c['id']} | `{c['name']}` | {c['kind']} | {c['scope']} | "
                     f"{md_cell(c['doc'])} | {md_cell(c['doctor'])} |")
    return "\n".join(lines) + "\n"


def rust_str(text: str) -> str:
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n") + '"'


def render_rust(counters: list[dict]) -> str:
    out = [
        "//! Every counter in `counters.toml`, as constants: one [`CounterId`] per entry and the",
        "//! [`COUNTERS`] table. Generated by `scripts/gen-counters.py`; do not edit. Change",
        "//! `counters.toml` and run `just gen-counters`; `just lint-counters` fails while this",
        "//! file is stale (ADR 0012).",
        "",
        "use super::{CounterDef, CounterId, Kind, Scope};",
        "",
    ]
    for c in counters:
        out.append(f"/// `{c['name']}`, a {c['scope']} {c['kind']}: {c['doc'].strip()}")
        out.append(f"pub const {const_name(c['name'])}: CounterId = CounterId({c['id']});")
        out.append("")
    out.append("/// Every entry of `counters.toml`, in the file's order.")
    out.append("pub const COUNTERS: &[CounterDef] = &[")
    for c in counters:
        out.append("    CounterDef {")
        out.append(f"        id: {const_name(c['name'])},")
        out.append(f"        name: {rust_str(c['name'])},")
        out.append(f"        kind: Kind::{c['kind'].capitalize()},")
        out.append(f"        scope: Scope::{c['scope'].capitalize()},")
        out.append(f"        doc: {rust_str(c['doc'])},")
        out.append(f"        doctor: {rust_str(c['doctor'])},")
        out.append("    },")
    out.append("];")
    return rustfmt("\n".join(out) + "\n")


def rustfmt(source: str) -> str:
    """The source through rustfmt with the repository's settings, or unchanged when rustfmt
    is not installed; the text is already in rustfmt's shape, and `cargo fmt --check`
    guards it in `just lint`."""
    if shutil.which("rustfmt") is None:
        return source
    config = [p for p in (ROOT / "rustfmt.toml", ROOT / ".rustfmt.toml") if p.exists()]
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "generated.rs"
        path.write_text(source, encoding="utf-8")
        command = ["rustfmt", "--edition", "2024"]
        if config:
            command += ["--config-path", str(config[0])]
        p = subprocess.run([*command, str(path)], capture_output=True, text=True)
        if p.returncode != 0:
            raise Fail(f"rustfmt failed on the generated Rust: {p.stderr.strip()}")
        return path.read_text(encoding="utf-8")


# ---- main -------------------------------------------------------------------------

def main(argv: list[str]) -> int:
    check_only = argv[1:] == ["--check"]
    if argv[1:] and not check_only:
        sys.exit("usage: gen-counters.py [--check]")
    try:
        counters = check(load(SOURCE.read_text(encoding="utf-8")))
        outputs = {PAGE: render_page(counters), RUST: render_rust(counters)}
    except Fail as e:
        print(f"gen-counters: {e}", file=sys.stderr)
        return 1
    stale = [p for p, text in outputs.items()
             if not p.exists() or p.read_text(encoding="utf-8") != text]
    if check_only:
        for p in stale:
            print(f"lint-counters: {p.relative_to(ROOT).as_posix()} is not what counters.toml "
                  "generates; run `just gen-counters` and commit the result", file=sys.stderr)
        if stale:
            return 1
        print(f"lint-counters: {len(counters)} counters, 2 generated files current, ok")
        return 0
    for p in stale:
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(outputs[p], encoding="utf-8")
        print(f"gen-counters: wrote {p.relative_to(ROOT).as_posix()}")
    if not stale:
        print("gen-counters: both files already current")
    print(f"gen-counters: {len(counters)} counters")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
