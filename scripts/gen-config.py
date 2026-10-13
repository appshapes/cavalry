#!/usr/bin/env python3
"""gen-config: the configuration gate CLAUDE.md names for invariant 5 (ADR 0020).

config.toml at the repository root is the single source of every configuration key.
This script checks it, checks that the code has what each key promises, and writes the
two files generated from it:

  docs/reference/config.md                         the reference page: how a key is set,
                                                   one row and one section per key
  crates/cavalry-control/src/config/generated.rs   one `KeyDef` constant per key and the
                                                   `KEYS` table

Two modes:

  gen-config.py            check and write both files; `just gen-config`
  gen-config.py --check    check and fail when either file differs from what would be
                           written; `just lint-config`, what CI runs

Checks on config.toml:

  schema           `schema = 0`, the only schema this script knows
  keys             every [[key]] has exactly name, type, default, flag, term, doc,
                   why_not_default and doctor, plus values when type is "enum"
  name             lowercase, dotted (`conductor.idle.strategy`) or one segment (`dir`),
                   unique; its environment variable, CAVALRY_ plus the name upper-cased
                   with dots as underscores, and its Rust constant name are unique
  type             "path", or "enum" with `values`: distinct non-empty strings, and the
                   default is one of them
  default          a non-empty string: the value, or for a path the rule in words
  flag             "" or `--name`, unique
  term             a non-empty string; `just lint-docs` checks it is a glossary row
  doc              a non-empty string
  why_not_default  a non-empty string (CLAUDE.md, invariant 5: the written reason)
  doctor           `a-name` that crates/cavalry-control/src/doctor.rs uses as a check
                   (`check: "<doctor>"`) and names in its tests

Checks on the code, so that a registered key is a real one:

  flag             a key with a flag is parsed by crates/cavalry/src/main.rs (the flag's
                   text appears there)
  read             a key's constant is used by crates/cavalry-control/src/config/mod.rs,
                   the module that reads the environment

Python 3.9 standard library only, like the other scripts (ADR 0003): `tomllib` when the
interpreter has it (3.11 and later), else a parser for the flat subset of TOML that
config.toml uses: `key = value` with basic strings, integers and arrays of basic
strings, `[[key]]` headers, blank lines and comments. The Rust output is written in
rustfmt's shape and run through rustfmt when it is installed.
"""

from __future__ import annotations

import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "config.toml"
PAGE = ROOT / "docs" / "reference" / "config.md"
RUST = ROOT / "crates" / "cavalry-control" / "src" / "config" / "generated.rs"
DOCTOR = ROOT / "crates" / "cavalry-control" / "src" / "doctor.rs"
READER = ROOT / "crates" / "cavalry-control" / "src" / "config" / "mod.rs"
BINARY = ROOT / "crates" / "cavalry" / "src" / "main.rs"
ADR = "../decisions/0020-configure-by-environment-variable.md"

SCHEMA = 0
KEYS = ("name", "type", "default", "flag", "term", "doc", "why_not_default", "doctor")
TYPES = ("path", "enum")
NAME = re.compile(r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$")
FLAG = re.compile(r"^--[a-z][a-z0-9-]*$")
CHECK = re.compile(r"^[a-z][a-z0-9-]*$")
ENV_PREFIX = "CAVALRY_"


class Fail(Exception):
    """A finding that stops the run; the message says which entry and what to fix."""


# ---- reading config.toml ----------------------------------------------------------

def load(text: str) -> dict:
    try:
        import tomllib  # Python 3.11+
    except ImportError:
        return parse_flat(text)
    try:
        return tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        raise Fail(f"config.toml: {e}") from None


def parse_flat(text: str) -> dict:
    """The subset config.toml uses: top-level `key = value`, `[[key]]` tables, integers,
    basic strings with \\\" \\\\ \\n \\t escapes, arrays of basic strings on one line,
    comments and blank lines."""
    doc: dict = {"key": []}
    table: dict | None = None
    for number, raw in enumerate(text.splitlines(), 1):
        line = strip_comment(raw).strip()
        if not line:
            continue
        if line == "[[key]]":
            table = {}
            doc["key"].append(table)
            continue
        m = re.match(r"^([A-Za-z0-9_-]+)\s*=\s*(.+)$", line)
        if not m:
            raise Fail(f"config.toml:{number}: not `key = value` or `[[key]]`; this parser "
                       "reads the flat subset the file uses (scripts/gen-config.py)")
        key, value = m.group(1), parse_value(m.group(2), number)
        target = doc if table is None else table
        if key in target:
            raise Fail(f"config.toml:{number}: `{key}` is set twice in the same table")
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
    if text.startswith("[") and text.endswith("]"):
        items = []
        for part in split_array(text[1:-1], number):
            value = parse_value(part, number)
            if not isinstance(value, str):
                raise Fail(f"config.toml:{number}: arrays hold strings only")
            items.append(value)
        return items
    if len(text) >= 2 and text[0] == '"' and text[-1] == '"':
        return parse_string(text[1:-1], number)
    raise Fail(f"config.toml:{number}: `{text}` is not an integer, a \"string\" or an array")


def split_array(body: str, number: int) -> list[str]:
    parts: list[str] = []
    current: list[str] = []
    quoted = False
    escaped = False
    for ch in body:
        if escaped:
            escaped = False
            current.append(ch)
        elif ch == "\\" and quoted:
            escaped = True
            current.append(ch)
        elif ch == '"':
            quoted = not quoted
            current.append(ch)
        elif ch == "," and not quoted:
            parts.append("".join(current))
            current = []
        else:
            current.append(ch)
    if quoted:
        raise Fail(f"config.toml:{number}: an array's string is not closed")
    tail = "".join(current)
    if tail.strip():
        parts.append(tail)
    return [p.strip() for p in parts if p.strip()]


def parse_string(body: str, number: int) -> str:
    out = []
    i = 0
    while i < len(body):
        ch = body[i]
        if ch == "\\":
            i += 1
            if i >= len(body):
                raise Fail(f"config.toml:{number}: a string ends in a backslash")
            escape = {'"': '"', "\\": "\\", "n": "\n", "t": "\t"}.get(body[i])
            if escape is None:
                raise Fail(f"config.toml:{number}: unknown escape \\{body[i]}")
            out.append(escape)
        elif ch == '"':
            raise Fail(f"config.toml:{number}: a quote inside a string must be \\\"")
        else:
            out.append(ch)
        i += 1
    return "".join(out)


# ---- checking ---------------------------------------------------------------------

def env_name(name: str) -> str:
    return ENV_PREFIX + name.upper().replace(".", "_")


def const_name(name: str) -> str:
    return name.upper().replace(".", "_")


def anchor(name: str) -> str:
    """mdBook's and GitHub's heading id for `## <name>`: lowercase, non-alphanumerics
    other than `-` and `_` dropped."""
    return re.sub(r"[^a-z0-9_-]", "", name.lower())


def check(doc: dict) -> list[dict]:
    if doc.get("schema") != SCHEMA:
        raise Fail(f"config.toml: schema = {doc.get('schema')!r}; this script knows schema "
                   f"{SCHEMA} (ADR 0020)")
    for key in doc:
        if key not in ("schema", "key"):
            raise Fail(f"config.toml: top-level key `{key}` is not `schema` or `[[key]]`")
    keys = doc.get("key") or []
    if not isinstance(keys, list) or not keys:
        raise Fail("config.toml: no [[key]] entries")
    names: set[str] = set()
    envs: dict[str, str] = {}
    consts: dict[str, str] = {}
    flags: dict[str, str] = {}
    for n, k in enumerate(keys, 1):
        where = f"config.toml: [[key]] {n} ({k.get('name', 'unnamed')!r})"
        expected = KEYS + (("values",) if k.get("type") == "enum" else ())
        missing = [f for f in expected if f not in k]
        extra = [f for f in k if f not in expected]
        if missing or extra:
            raise Fail(f"{where}: every entry has exactly {', '.join(KEYS)}, plus values when "
                       "type is \"enum\""
                       + (f"; missing {missing}" if missing else "")
                       + (f"; unknown {extra}" if extra else ""))
        name = k["name"]
        if not isinstance(name, str) or not NAME.match(name):
            raise Fail(f"{where}: name must be lowercase and dotted, like "
                       f"`conductor.idle.strategy`, or one segment, like `dir`; got {name!r}")
        if name in names:
            raise Fail(f"{where}: name {name!r} is used twice")
        names.add(name)
        env = env_name(name)
        if env in envs:
            raise Fail(f"{where}: environment variable {env} is also {envs[env]!r}'s")
        envs[env] = name
        const = const_name(name)
        if const in consts:
            raise Fail(f"{where}: constant {const} is also {consts[const]!r}'s; rename one")
        consts[const] = name
        if k["type"] not in TYPES:
            raise Fail(f"{where}: type must be one of {list(TYPES)}; got {k['type']!r}")
        for field in ("default", "term", "doc", "why_not_default"):
            if not isinstance(k[field], str) or not k[field].strip():
                raise Fail(f"{where}: {field} must be a non-empty string"
                           + (" (CLAUDE.md, invariant 5: the written reason why the default "
                              "cannot be right)" if field == "why_not_default" else ""))
        if k["type"] == "enum":
            values = k["values"]
            if (not isinstance(values, list) or not values
                    or any(not isinstance(v, str) or not v.strip() for v in values)):
                raise Fail(f"{where}: values must be a non-empty array of non-empty strings")
            if len(set(values)) != len(values):
                raise Fail(f"{where}: values has a duplicate: {values}")
            if k["default"] not in values:
                raise Fail(f"{where}: default {k['default']!r} is not one of values {values}")
        flag = k["flag"]
        if not isinstance(flag, str) or (flag and not FLAG.match(flag)):
            raise Fail(f"{where}: flag must be \"\" or `--name`; got {flag!r}")
        if flag:
            if flag in flags:
                raise Fail(f"{where}: flag {flag} is also {flags[flag]!r}'s")
            flags[flag] = name
        doctor = k["doctor"]
        if not isinstance(doctor, str) or not CHECK.match(doctor):
            raise Fail(f"{where}: doctor must name a check, like `driver-directory-backing`; "
                       f"got {doctor!r} (CLAUDE.md, invariant 5: a doctor check that fires "
                       "when the key is set wrong)")
    return keys


def check_code(keys: list[dict]) -> None:
    """A registered key is a real one: its doctor check exists and is tested, its flag is
    parsed, and the module that reads the environment uses its constant."""
    doctor = DOCTOR.read_text(encoding="utf-8") if DOCTOR.exists() else ""
    tests = doctor.split("mod tests", 1)[1] if "mod tests" in doctor else ""
    reader = READER.read_text(encoding="utf-8") if READER.exists() else ""
    binary = BINARY.read_text(encoding="utf-8") if BINARY.exists() else ""
    for k in keys:
        where = f"config.toml: key `{k['name']}`"
        if f'check: "{k["doctor"]}"' not in doctor:
            raise Fail(f"{where}: doctor check `{k['doctor']}` is not a finding in "
                       f"{DOCTOR.relative_to(ROOT).as_posix()} (no `check: \"{k['doctor']}\"`); "
                       "write the check that fires when the key is set wrong")
        if f'"{k["doctor"]}"' not in tests:
            raise Fail(f"{where}: doctor check `{k['doctor']}` has no test naming it in "
                       f"{DOCTOR.relative_to(ROOT).as_posix()} (CLAUDE.md, gate 4: each known "
                       "pathology has a doctor test)")
        if k["flag"] and f'"{k["flag"]}"' not in binary:
            raise Fail(f"{where}: flag {k['flag']} is not parsed by "
                       f"{BINARY.relative_to(ROOT).as_posix()}; add it there or set flag = \"\"")
        if const_name(k["name"]) not in reader:
            raise Fail(f"{where}: {READER.relative_to(ROOT).as_posix()} does not use "
                       f"{const_name(k['name'])}; the module that reads the environment reads "
                       "every key")


# ---- rendering --------------------------------------------------------------------

def md_cell(text: str) -> str:
    """Prose in a table cell or a list item: pipes and newlines out, and angle brackets
    escaped so that mdBook does not read `<user>` as an HTML tag."""
    text = text.replace("|", "\\|").replace("\n", " ").strip()
    return text.replace("<", "&lt;").replace(">", "&gt;")


def type_text(k: dict) -> str:
    if k["type"] == "enum":
        return "one of " + ", ".join(f"`{v}`" for v in k["values"])
    return k["type"]


def default_text(k: dict) -> str:
    """The default in code, so that `<user>` stays literal."""
    return f"`{k['default']}`"


def render_page(keys: list[dict]) -> str:
    lines = [
        "<!-- Generated from config.toml by scripts/gen-config.py. Do not edit this page: "
        "change config.toml and run `just gen-config`. -->",
        "# Configuration",
        "",
        "Every configuration key Cavalry has, from `config.toml`, the single source "
        f"([ADR 0020]({ADR})). Zero configuration is the primary supported configuration "
        "(`CLAUDE.md`, invariant 5): `cavalry run` with nothing set works on a laptop and on "
        "a cloud VM, and every key below says why its default cannot always be right and "
        "names the `doctor` check that fires when it is set wrong. This page is generated by "
        "`scripts/gen-config.py`: edit `config.toml`, never this page, and `just lint-config` "
        "fails when the two differ.",
        "",
        "## How a key is set",
        "",
        "A key is read from these, and the first that is set wins:",
        "",
        "1. Its flag on the `cavalry` binary, when the key has one. A flag exists for a key an "
        "operator types interactively; a key set once in a unit file or a container's "
        "environment has none.",
        "2. Its environment variable, which every key has: `CAVALRY_`, then the key's name "
        "upper-cased with dots as underscores, so `conductor.idle.strategy` is "
        "`CAVALRY_CONDUCTOR_IDLE_STRATEGY`. An empty value is an unset one.",
        "3. Its default.",
        "",
        "There is no configuration file to write. `config.toml` in the repository is the "
        "registry this page and the code are generated from, not a file an operator edits. A "
        "value a key does not take stops the command before anything starts, with a message "
        "that names the key, the value, the values it takes and this page. A secret, when "
        "the first one exists, has no flag and no default, is read from its environment "
        "variable or from the file named by the variable with the suffix `_FILE`, and "
        "`doctor` reports it as set or unset, never its value.",
        "",
        "In Aeron, the keys are system properties in the Java driver, `aeron.dir`, and "
        "environment variables in the C driver, `AERON_DIR` ([glossary](../glossary.md)).",
        "",
        "## The keys",
        "",
        "| Key | Type | Default | Environment variable | Flag | Doctor check |",
        "| --- | --- | --- | --- | --- | --- |",
    ]
    for k in keys:
        flag = f"`{k['flag']}`" if k["flag"] else "none"
        lines.append(f"| [`{k['name']}`](#{anchor(k['name'])}) | {type_text(k)} | "
                     f"{default_text(k)} | `{env_name(k['name'])}` | {flag} | "
                     f"`{k['doctor']}` |")
    for k in keys:
        flag = f"`{k['flag']}` on `cavalry run` and `cavalry doctor`" if k["flag"] else "none"
        lines += [
            "",
            f"## {k['name']}",
            "",
            f"- **Type:** {type_text(k)}",
            f"- **Default:** {default_text(k)}",
            f"- **Environment variable:** `{env_name(k['name'])}`",
            f"- **Flag:** {flag}",
            f"- **Doctor check:** `{k['doctor']}`",
            f"- **Glossary:** {k['term']}",
            "",
            md_cell(k["doc"]),
            "",
            f"**Why the default cannot always be right.** {md_cell(k['why_not_default'])}",
        ]
    return "\n".join(lines) + "\n"


def rust_str(text: str) -> str:
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n") + '"'


def render_rust(keys: list[dict]) -> str:
    out = [
        "//! Every configuration key in `config.toml`, as constants: one [`KeyDef`] per key and",
        "//! the [`KEYS`] table. Generated by `scripts/gen-config.py`; do not edit. Change",
        "//! `config.toml` and run `just gen-config`; `just lint-config` fails while this file",
        "//! is stale (ADR 0020).",
        "",
        "use super::{KeyDef, Kind};",
        "",
    ]
    for k in keys:
        out.append(f"/// `{k['name']}`, {type_text(k).replace('`', '')}: {k['doc'].strip()}")
        out.append(f"pub const {const_name(k['name'])}: KeyDef = KeyDef {{")
        out.append(f"    name: {rust_str(k['name'])},")
        out.append(f"    env: {rust_str(env_name(k['name']))},")
        out.append(f"    flag: {('Some(' + rust_str(k['flag']) + ')') if k['flag'] else 'None'},")
        out.append(f"    kind: Kind::{k['type'].capitalize()},")
        values = ", ".join(rust_str(v) for v in k.get("values", []))
        out.append(f"    values: &[{values}],")
        out.append(f"    default: {rust_str(k['default'])},")
        out.append(f"    term: {rust_str(k['term'])},")
        out.append(f"    doc: {rust_str(k['doc'])},")
        out.append(f"    why_not_default: {rust_str(k['why_not_default'])},")
        out.append(f"    doctor: {rust_str(k['doctor'])},")
        out.append(f"    anchor: {rust_str('docs/reference/config.md#' + anchor(k['name']))},")
        out.append("};")
        out.append("")
    out.append("/// Every key of `config.toml`, in the file's order.")
    refs = ", ".join("&" + const_name(k["name"]) for k in keys)
    out.append(f"pub const KEYS: &[&KeyDef] = &[{refs}];")
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
        sys.exit("usage: gen-config.py [--check]")
    try:
        keys = check(load(SOURCE.read_text(encoding="utf-8")))
        outputs = {PAGE: render_page(keys), RUST: render_rust(keys)}
    except Fail as e:
        print(f"{'lint-config' if check_only else 'gen-config'}: {e}", file=sys.stderr)
        return 1
    stale = [p for p, text in outputs.items()
             if not p.exists() or p.read_text(encoding="utf-8") != text]
    if check_only:
        for p in stale:
            print(f"lint-config: {p.relative_to(ROOT).as_posix()} is not what config.toml "
                  "generates; run `just gen-config` and commit the result", file=sys.stderr)
        try:
            check_code(keys)
        except Fail as e:
            print(f"lint-config: {e}", file=sys.stderr)
            return 1
        if stale:
            return 1
        print(f"lint-config: {len(keys)} keys, each with why_not_default, a tested doctor "
              "check and a reference section, 2 generated files current, ok")
        return 0
    # The files are written before the code is checked, so that a new key's constant exists
    # for the code that the check then asks for.
    for p in stale:
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(outputs[p], encoding="utf-8")
        print(f"gen-config: wrote {p.relative_to(ROOT).as_posix()}")
    if not stale:
        print("gen-config: both files already current")
    print(f"gen-config: {len(keys)} keys")
    try:
        check_code(keys)
    except Fail as e:
        print(f"gen-config: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
