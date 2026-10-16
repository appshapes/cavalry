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

Checks on the state machines of the data plane, the gate CLAUDE.md names for a state
transition with no counter (invariant 4; the shape is in counters.toml's header):

  machine     every [[state_machine]] has exactly name and states: a unique lowercase name
              and a non-empty array of unique lowercase states
  transition  every [[transition]] has exactly machine, from, to and counter: the machine
              exists, from and to are its states, the counter is a [[counter]] name, no
              transition repeats, and every machine has at least one
  counter     a transition's counter is a [[counter]] of kind "counter": invariant 4 asks
              for a count that only rises, and a gauge that moves back hides how often
              it moved
  moved       a transition's counter's constant (`COUNTERS_FREED`) is named in the code of
              some crate under crates/*/src, outside generated.rs and comments: a
              registered counter no code names is a transition with no counter. The
              crate may differ from the enum's: a layout crate holds the state, and the
              driver, which performs the transition, moves the counter
  enum        exactly one enum under crates/*/src carries the comment
              `// state machine: <name>` on the line before its definition (attributes
              and docs may sit between), and its variants, one per line, lower-cased
              with an underscore between words, are the machine's states
  fail closed every enum under crates/*/src whose name is or ends in State, Status or
              Phase carries `// state machine: <name>` or `// not a state machine: <why>`,
              so an enum nobody registered fails the gate instead of passing it

Python 3.9 standard library only, like the other scripts (ADR 0003): `tomllib` when the
interpreter has it (3.11 and later), else a parser for the flat subset of TOML that
counters.toml uses: `key = value` with integers, basic strings and arrays of them,
`[[counter]]`, `[[state_machine]]` and `[[transition]]` headers, blank lines and
comments. The Rust output is written in rustfmt's shape and run through rustfmt when it
is installed, so `cargo fmt --check` passes either way.
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
MACHINE_KEYS = ("name", "states")
TRANSITION_KEYS = ("machine", "from", "to", "counter")
TABLES = ("counter", "state_machine", "transition")
KINDS = ("counter", "gauge")
SCOPES = ("static", "instance")
MAX_ID = 2**31 - 1
MAX_LABEL_LENGTH = 124
NAME = re.compile(r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$")
STATE = re.compile(r"^[a-z][a-z0-9_]*$")
MARKER = re.compile(r"^\s*// state machine: ([a-z][a-z0-9_.]*)\s*$")
EXEMPT = re.compile(r"^\s*// not a state machine:(.*)$")
ENUM = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+(\w+)")
STATEFUL = re.compile(r"(State|Status|Phase)$")
VARIANT = re.compile(r"^\s*([A-Z]\w*)")
CRATES = ROOT / "crates"


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
    """The subset counters.toml uses: top-level `key = value`, the `[[counter]]`,
    `[[state_machine]]` and `[[transition]]` tables, integers, basic strings with \\\" \\\\
    \\n \\t escapes, one-line arrays of them, comments and blank lines."""
    doc: dict = {}
    table: dict | None = None
    for number, raw in enumerate(text.splitlines(), 1):
        line = strip_comment(raw).strip()
        if not line:
            continue
        header = re.fullmatch(r"\[\[([a-z_]+)\]\]", line)
        if header:
            table = {}
            doc.setdefault(header.group(1), []).append(table)
            continue
        m = re.match(r"^([A-Za-z0-9_-]+)\s*=\s*(.+)$", line)
        if not m:
            raise Fail(f"counters.toml:{number}: not `key = value` or a `[[table]]` header; this "
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
    if len(text) >= 2 and text[0] == "[" and text[-1] == "]":
        return [parse_value(item, number) for item in split_items(text[1:-1])]
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


def split_items(text: str) -> list[str]:
    """An array's items, split on the commas outside strings; a trailing comma is allowed."""
    items, start, quoted, escaped = [], 0, False, False
    for i, ch in enumerate(text):
        if escaped:
            escaped = False
        elif ch == "\\" and quoted:
            escaped = True
        elif ch == '"':
            quoted = not quoted
        elif ch == "," and not quoted:
            items.append(text[start:i])
            start = i + 1
    items.append(text[start:])
    return [item for item in items if item.strip()]


def fail(message: str):
    raise Fail(message)


# ---- checking ---------------------------------------------------------------------

def check(doc: dict) -> tuple[list[dict], list[dict]]:
    if doc.get("schema") != SCHEMA:
        raise Fail(f"counters.toml: schema = {doc.get('schema')!r}; this script knows "
                   f"schema {SCHEMA} (spec/counters-file.md, `counters.toml`)")
    for key in doc:
        if key not in ("schema", *TABLES):
            raise Fail(f"counters.toml: top-level key `{key}` is not `schema` or one of "
                       f"{['[[' + t + ']]' for t in TABLES]}")
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
    return counters, check_machines(doc, {c["name"]: c["kind"] for c in counters})


def const_name(name: str) -> str:
    return name.upper().replace(".", "_")


def check_machines(doc: dict, counters: dict[str, str]) -> list[dict]:
    """The [[state_machine]] and [[transition]] tables, checked against each other, the
    counters and the marked enums; returns each machine with its transitions attached."""
    machines: dict[str, dict] = {}
    for n, m in enumerate(doc.get("state_machine") or [], 1):
        where = f"counters.toml: [[state_machine]] {n} ({m.get('name', 'unnamed')!r})"
        if sorted(m) != sorted(MACHINE_KEYS):
            raise Fail(f"{where}: every entry has exactly name and states; got {sorted(m)}")
        name, states = m["name"], m["states"]
        if not isinstance(name, str) or not MARKER.match(f"// state machine: {name}"):
            raise Fail(f"{where}: name must be lowercase, like a counter's prefix; got {name!r}")
        if name in machines:
            raise Fail(f"{where}: name {name!r} is used twice")
        if (not isinstance(states, list) or not states or len(set(states)) != len(states)
                or any(not isinstance(s, str) or not STATE.match(s) for s in states)):
            raise Fail(f"{where}: states must be a non-empty array of unique lowercase names; "
                       f"got {states!r}")
        machines[name] = {"name": name, "states": states, "transitions": []}
    seen: set[tuple] = set()
    for n, t in enumerate(doc.get("transition") or [], 1):
        where = f"counters.toml: [[transition]] {n}"
        if sorted(t) != sorted(TRANSITION_KEYS):
            raise Fail(f"{where}: every entry has exactly machine, from, to and counter; "
                       f"got {sorted(t)}")
        wrong = [k for k in TRANSITION_KEYS if not isinstance(t[k], str)]
        if wrong:
            raise Fail(f"{where}: {wrong} must be strings, one name each; got "
                       f"{[t[k] for k in wrong]!r}")
        machine = machines.get(t["machine"])
        if machine is None:
            raise Fail(f"{where}: machine {t['machine']!r} has no [[state_machine]]")
        for end in ("from", "to"):
            if t[end] not in machine["states"]:
                raise Fail(f"{where}: {end} = {t[end]!r} is not a state of {t['machine']!r}, "
                           f"whose states are {machine['states']}")
        if t["counter"] not in counters:
            raise Fail(f"{where}: counter {t['counter']!r} is not a [[counter]] name; a "
                       "transition with no counter is what this gate refuses (CLAUDE.md, "
                       "invariant 4): add the counter, then name it here")
        if counters[t["counter"]] != "counter":
            raise Fail(f"{where}: counter {t['counter']!r} is a {counters[t['counter']]}; a "
                       "transition adds one to a [[counter]] of kind \"counter\", which only "
                       "rises, so its count of moves survives a move back (invariant 4)")
        key = tuple(t[k] for k in TRANSITION_KEYS)
        if key in seen:
            raise Fail(f"{where}: repeats an earlier transition")
        seen.add(key)
        machine["transitions"].append(t)
    for machine in machines.values():
        if not machine["transitions"]:
            raise Fail(f"counters.toml: state machine {machine['name']!r} has no [[transition]]")
    check_enums(machines)
    return list(machines.values())


def check_enums(machines: dict[str, dict]) -> None:
    """Every marked enum under crates/*/src has a table and the table's states; every
    table has exactly one marked enum; every enum whose name says it holds a state is
    marked, as a state machine or as not one."""
    found: dict[str, str] = {}
    crate_of: dict[str, str] = {}
    unmarked: list[str] = []
    for path in sorted(CRATES.glob("*/src/**/*.rs")):
        file = path.relative_to(ROOT).as_posix()
        crate = path.relative_to(CRATES).parts[0]
        lines = path.read_text(encoding="utf-8").splitlines()
        marked: set[int] = set()
        for i, line in enumerate(lines):
            m, x = MARKER.match(line), EXEMPT.match(line)
            if not (m or x):
                continue
            where = f"{file}:{i + 1}"
            if x and not x.group(1).strip():
                raise Fail(f"{where}: `// not a state machine:` needs its reason after the "
                           "colon: why no transition of this enum needs a counter")
            begin, variants = enum_at(lines, i + 1, where)
            marked.add(begin)
            if x:
                continue
            name = m.group(1)
            if name not in machines:
                raise Fail(f"{where}: `// state machine: {name}` has no [[state_machine]] "
                           "in counters.toml")
            if name in found:
                raise Fail(f"{where}: state machine {name!r} is also marked at {found[name]}; "
                           "one enum per machine")
            found[name] = where
            crate_of[name] = crate
            states = machines[name]["states"]
            if sorted(variants) != sorted(states):
                raise Fail(f"{where}: the enum's variants {sorted(variants)} are not the "
                           f"states of {name!r}, {sorted(states)}")
        for i, line in enumerate(lines):
            e = ENUM.match(line)
            if e and STATEFUL.search(e.group(1)) and i not in marked:
                unmarked.append(f"{file}:{i + 1} ({e.group(1)})")
    if unmarked:
        raise Fail(f"{', '.join(unmarked)}: an enum whose name ends in State, Status or Phase "
                   "carries, on the line before it, `// state machine: <name>` with a "
                   "[[state_machine]] in counters.toml whose every [[transition]] names a "
                   "counter, or `// not a state machine: <why>` (CLAUDE.md, invariant 4; "
                   "docs/reference/counters.md, State machines)")
    for name in machines:
        if name not in found:
            raise Fail(f"counters.toml: state machine {name!r} has no enum under crates/*/src "
                       f"marked `// state machine: {name}` on the line before it")
    code = crate_code()
    for name, machine in machines.items():
        check_moved(machine, found[name], code)


def crate_code() -> str:
    """Every crate's source outside generated.rs, with comments stripped, as one text."""
    code = []
    for path in sorted(CRATES.glob("*/src/**/*.rs")):
        if path == RUST:
            continue
        code += [line.split("//")[0] for line in path.read_text(encoding="utf-8").splitlines()]
    return "\n".join(code)


def check_moved(machine: dict, where: str, code: str) -> None:
    """Every transition's counter is named by code in some crate, outside generated.rs
    and outside comments: a counter declared and registered but never moved would let the
    gate pass a transition nothing counts. The crate may differ from the enum's: a layout
    crate holds the state word, and the driver that writes it moves the counter."""
    for t in machine["transitions"]:
        const = const_name(t["counter"])
        if not re.search(rf"\b{const}\b", code):
            raise Fail(f"counters.toml: [[transition]] {t['from']} -> {t['to']} of "
                       f"{machine['name']!r} (enum at {where}) names counter "
                       f"{t['counter']!r}, but no code under crates/*/src outside "
                       f"generated.rs names {const}: a counter nothing moves is a transition "
                       "with no counter (CLAUDE.md, invariant 4); move it where the "
                       "transition is written, then register it")


def enum_at(lines: list[str], start: int, where: str) -> tuple[int, list[str]]:
    """The line of the enum that begins at line `start`, after any attributes and docs,
    and its variants as lowercase names with an underscore between words (`WriteFailed`
    is `write_failed`). One variant per line; a tuple or struct variant's fields may span
    lines, and an attribute may span lines."""
    variants: list[str] = []
    begin, attribute, depth, opened = None, 0, 0, None
    for i in range(start, len(lines)):
        line, stripped = lines[i], lines[i].strip()
        if begin is None:
            if attribute == 0 and ENUM.match(line):
                begin = i
            elif attribute > 0 or stripped.startswith("#["):
                attribute = max(0, attribute + line.count("[") - line.count("]"))
                continue
            elif not stripped or stripped.startswith("//"):
                continue
            else:
                raise Fail(f"{where}: the line after the marker must begin an enum; "
                           "comments, docs and attributes may sit between")
        if opened is not None and depth == 1 and (m := VARIANT.match(line)):
            if re.search(r",\s*[A-Z]", re.sub(r"\([^)]*\)|\{[^}]*\}", "", line)):
                raise Fail(f"{where}: line {i + 1} holds more than one variant; write the "
                           "enum one variant per line, so that this check can read it")
            variants.append(re.sub(r"(?<!^)(?=[A-Z])", "_", m.group(1)).lower())
        if opened is None and "{" in line.split("//")[0]:
            opened = i
        code = line.split("//")[0]
        depth += code.count("{") + code.count("(") - code.count("}") - code.count(")")
        if opened is not None and depth == 0:
            if opened == i:
                raise Fail(f"{where}: the enum opens and closes on line {i + 1}; write it "
                           "one variant per line, so that this check can read it")
            return begin, variants
    raise Fail(f"{where}: the marked enum never closes")


# ---- rendering --------------------------------------------------------------------

def md_cell(text: str) -> str:
    return text.replace("|", "\\|").replace("\n", " ").strip()


def render_page(counters: list[dict], machines: list[dict]) -> str:
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
    lines += [
        "",
        "## State machines",
        "",
        "Every state machine on the data plane registers its states and transitions in "
        "`counters.toml` too, and `just lint-counters` fails a transition that names no "
        "counter, a counter that is not of kind `counter` in the table above, a counter "
        "whose constant no code in the enum's crate names, or an enum whose variants are "
        "not the machine's states (CLAUDE.md, invariant 4). The enum "
        "that holds the states carries the comment `// state machine: <name>` on the line "
        "before its definition. The check fails closed: every enum under `crates/*/src` "
        "whose name is or ends in `State`, `Status` or `Phase` carries that comment, or "
        "`// not a state machine: <why>` with the reason no transition of it needs a "
        "counter, and `just lint-counters` names the file and line of one that has "
        "neither. Each machine is listed here with the counter every transition adds one "
        "to.",
    ]
    if not machines:
        lines += ["", "None registered yet."]
    for machine in machines:
        lines += [
            "",
            f"### `{machine['name']}`",
            "",
            "States: " + ", ".join(f"`{s}`" for s in machine["states"]) + ".",
            "",
            "| From | To | Counter |",
            "| --- | --- | --- |",
        ]
        lines += [f"| `{t['from']}` | `{t['to']}` | `{t['counter']}` |"
                  for t in machine["transitions"]]
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
        counters, machines = check(load(SOURCE.read_text(encoding="utf-8")))
        outputs = {PAGE: render_page(counters, machines), RUST: render_rust(counters)}
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
        print(f"lint-counters: {len(counters)} counters, {len(machines)} state machines, "
              "2 generated files current, ok")
        return 0
    for p in stale:
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(outputs[p], encoding="utf-8")
        print(f"gen-counters: wrote {p.relative_to(ROOT).as_posix()}")
    if not stale:
        print("gen-counters: both files already current")
    print(f"gen-counters: {len(counters)} counters, {len(machines)} state machines")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
