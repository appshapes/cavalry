#!/usr/bin/env python3
"""The check `just lint` runs over the generated clients (clients/README.md; ADR 0046, the
boundary's second part): the per-message functions a generator writes, the offer, the
claim, the commit and abort, the poll and the trampoline that calls the user's handler,
hold no lock, no `await` and no raise, in the language's forms. For Python those forms are
`threading`'s locks and anything else of `threading`, `async def`, `await` and `raise`. The
one raise allowed is in `poll`, after the line that calls the ABI, where the user's handler's
exception is raised once the poll has returned (clients/python/README.md).

It fails on generated code under `clients/` in a language it has no rules for, and on a
package in which it finds none of the per-message functions, so that a client cannot slip
past it by renaming them. `lint-test.py` holds each rule to a generated function carrying
the form it forbids. Python 3.9 standard library only (ADR 0003).

    lint.py            check every client under clients/
"""

from __future__ import annotations

import ast
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CLIENTS = os.path.join(ROOT, "clients")

# Per language: the generated file that holds the per-message functions, their names as
# `Class.method`, and the function whose raise after the ABI's call is allowed with the
# name of that ABI call.
RULES = {
    "python": {
        "file": os.path.join("cavalry", "__init__.py"),
        "functions": [
            "Publication.try_claim",
            "Publication.offer",
            "Publication.commit",
            "Publication.abort",
            "Subscription.poll",
            "Subscription._on_fragment",
        ],
        "raise_after": ("Subscription.poll", "cavalry_subscription_poll"),
    },
}
HELP = "see clients/README.md, \"What every client must pass\""


def python_findings(language: str, path: str, rules: dict) -> list[str]:
    with open(path, encoding="utf-8") as f:
        tree = ast.parse(f.read(), filename=path)
    found: dict[str, ast.AST] = {}
    for node in tree.body:
        if isinstance(node, ast.ClassDef):
            for item in node.body:
                if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    found[f"{node.name}.{item.name}"] = item
    findings = []
    present = [name for name in rules["functions"] if name in found]
    if not present:
        return [f"{language}: none of the per-message functions {', '.join(rules['functions'])} is in {path}; "
                f"the generator renamed them or the rules are stale; {HELP}"]
    for name in rules["functions"]:
        if name not in found:
            findings.append(f"{language}: the per-message function {name} is not in {path}; {HELP}")
    allowed_in, abi_call = rules["raise_after"]
    for name in present:
        node = found[name]
        where = f"{language}: {name} ({os.path.relpath(path, ROOT)}:{node.lineno})"
        if isinstance(node, ast.AsyncFunctionDef):
            findings.append(f"{where} is `async def`: a per-message function runs on the caller's thread and returns "
                            f"when the ABI returns, with no coroutine around it; {HELP}")
        abi_line = None
        if name == allowed_in:
            for sub in ast.walk(node):
                if isinstance(sub, ast.Attribute) and sub.attr == abi_call:
                    abi_line = sub.lineno if abi_line is None else min(abi_line, sub.lineno)
        for sub in ast.walk(node):
            if isinstance(sub, ast.Await):
                findings.append(f"{where} awaits at line {sub.lineno}: no `await` on the per-message path; {HELP}")
            elif _names_threading(sub):
                findings.append(f"{where} uses `threading` at line {sub.lineno}: no lock on the per-message path; {HELP}")
            elif isinstance(sub, ast.Raise):
                if name == allowed_in and abi_line is not None and sub.lineno > abi_line:
                    continue  # the handler's exception, raised once the ABI has returned
                findings.append(f"{where} raises at line {sub.lineno}: an Offer code is returned as a value and the "
                                f"handler's exception is raised by poll after the ABI returns, nothing else raises; {HELP}")
    return findings


LOCKS = ("Lock", "RLock", "Condition", "Semaphore", "BoundedSemaphore")


def _names_threading(node: ast.AST) -> bool:
    """A `threading` name, reported once at the name itself, or a lock class reached some
    other way, such as `from threading import Lock`."""
    if isinstance(node, ast.Name):
        return node.id == "threading" or node.id in LOCKS
    if isinstance(node, ast.Attribute):
        return node.attr in LOCKS and not (isinstance(node.value, ast.Name) and node.value.id == "threading")
    return False


CHECKERS = {"python": python_findings}


def findings(clients: str = CLIENTS) -> list[str]:
    out = []
    for entry in sorted(os.listdir(clients)):
        path = os.path.join(clients, entry)
        if not os.path.isdir(path) or entry.startswith(".") or entry == "__pycache__":
            continue
        if entry not in RULES:
            out.append(f"{entry}: generated code under clients/{entry} in a language this check has no rules for; add "
                       f"its per-message functions and forms to RULES in clients/lint.py; {HELP}")
            continue
        rules = RULES[entry]
        file = os.path.join(path, rules["file"])
        if not os.path.exists(file):
            out.append(f"{entry}: {os.path.relpath(file, ROOT)} is missing; run `python3 clients/generate.py {entry}`")
            continue
        out.extend(CHECKERS[entry](entry, file, rules))
    return out


def main(argv: list[str]) -> int:
    found = findings()
    for line in found:
        print(f"lint-clients: {line}", file=sys.stderr)
    if found:
        print(f"lint-clients: {len(found)} finding(s)", file=sys.stderr)
        return 1
    languages = [e for e in sorted(os.listdir(CLIENTS)) if e in RULES]
    print(f"lint-clients: {', '.join(languages)}: the per-message functions hold no lock, no await and no raise")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
