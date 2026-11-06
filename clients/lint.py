#!/usr/bin/env python3
"""The check `just lint` runs over the generated clients (clients/README.md; ADR 0046, the
boundary's second part): the per-message functions a generator writes, the wrapper of each
of the ABI's seven per-message functions (`crates/cavalry-ffi/README.md`: the claim, the
reserved value, the commit and abort, the offer, the poll and the header's values) and the
trampoline that calls the user's handler, hold no lock, no `await` and no raise, in the
language's forms, and so does every function of the package they call. For Python those
forms are `threading`'s locks and anything else of `threading`, `async def`, `await`,
`raise`, and building one of the package's own exceptions. A call reaches the package's
own functions by name: a module function, a class's `__init__`, a method of the same class
through `self`, and any other method by its name in any class of the package. The one
raise allowed is in `poll`, after the line that calls the ABI, where the user's handler's
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
            "Publication.set_reserved_value",
            "Publication.commit",
            "Publication.abort",
            "Publication.offer",
            "Subscription.poll",
            "Subscription._on_fragment",
            "Header.values",
            "Header.session_id",
            "Header.stream_id",
            "Header.term_id",
            "Header.term_offset",
            "Header.frame_length",
            "Header.flags",
            "Header.type",
            "Header.version",
            "Header.reserved_value",
        ],
        "raise_after": ("Subscription.poll", "cavalry_subscription_poll"),
    },
}
HELP = "see clients/README.md, \"What every client must pass\""
WHY = {
    "await": "no `await` on the per-message path",
    "threading": "no lock on the per-message path",
    "raise": "an Offer code is returned as a value and the handler's exception is raised by poll after the ABI "
             "returns, nothing else on the per-message path raises",
}


class Package:
    """The module's own functions, classes and exception classes, and what each function
    reaches: the first await, `threading` name and raise found in its body or, through its
    calls, in the module's functions it calls."""

    def __init__(self, tree: ast.Module):
        self.functions: dict[str, ast.AST] = {}
        self.classes: dict[str, dict[str, ast.AST]] = {}
        bases: dict[str, list[str]] = {}
        for node in tree.body:
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                self.functions[node.name] = node
            elif isinstance(node, ast.ClassDef):
                self.classes[node.name] = {item.name: item for item in node.body
                                           if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef))}
                bases[node.name] = [b.id for b in node.bases if isinstance(b, ast.Name)]
        self.exceptions: set[str] = set()
        while True:
            more = {name for name, of in bases.items() if name not in self.exceptions
                    and any(b in ("Exception", "BaseException") or b in self.exceptions for b in of)}
            if not more:
                break
            self.exceptions |= more
        self.reached: dict[str, dict[str, str]] = {}

    def node(self, label: str) -> ast.AST:
        if "." in label:
            cls, name = label.split(".", 1)
            return self.classes[cls][name]
        return self.functions[label]

    def callees(self, cls: str | None, call: ast.Call) -> list[str]:
        """The package's own functions a call can reach, as labels."""
        func = call.func
        if isinstance(func, ast.Name):
            if func.id in self.functions:
                return [func.id]
            if func.id in self.classes and "__init__" in self.classes[func.id]:
                return [f"{func.id}.__init__"]
            return []
        if not isinstance(func, ast.Attribute) or func.attr.startswith("__"):
            return []
        if isinstance(func.value, ast.Name) and func.value.id == "self" and cls and func.attr in self.classes[cls]:
            return [f"{cls}.{func.attr}"]
        return [f"{c}.{func.attr}" for c, methods in self.classes.items() if func.attr in methods]

    def forms(self, label: str, node: ast.AST, raise_after: int | None):
        """Each form in `node`'s own body, as (kind, line, text), and each call to the
        package's own functions, as ("call", line, callee). A raise or an exception built
        on a line after `raise_after` is allowed and not yielded."""
        cls = label.split(".", 1)[0] if "." in label else None
        for sub in ast.walk(node):
            allowed = raise_after is not None and getattr(sub, "lineno", 0) > raise_after
            if isinstance(sub, ast.Await):
                yield "await", sub.lineno, f"awaits at line {sub.lineno}"
            elif _names_threading(sub):
                yield "threading", sub.lineno, f"uses `threading` at line {sub.lineno}"
            elif isinstance(sub, ast.Raise) and not allowed:
                yield "raise", sub.lineno, f"raises at line {sub.lineno}"
            elif isinstance(sub, ast.Call):
                if isinstance(sub.func, ast.Name) and sub.func.id in self.exceptions and not allowed:
                    yield "raise", sub.lineno, f"builds {sub.func.id} at line {sub.lineno}"
                for callee in self.callees(cls, sub):
                    if callee != label:
                        yield ("call-allowed" if allowed else "call"), sub.lineno, callee

    def reach(self, label: str) -> dict[str, str]:
        """{kind: how `label` reaches it}, for the first await, `threading` name and raise
        in its body or in the package's functions it calls; memoized, and a function in a
        cycle reads as what was found before the cycle."""
        if label in self.reached:
            return self.reached[label]
        found: dict[str, str] = {}
        self.reached[label] = found
        for kind, line, what in self.forms(label, self.node(label), None):
            if kind in WHY:
                found.setdefault(kind, f"{label} {what}")
                continue
            for k, how in self.reach(what).items():
                found.setdefault(k, f"{label} calls {what} at line {line}, and {how}")
        return found


def python_findings(language: str, path: str, rules: dict) -> list[str]:
    with open(path, encoding="utf-8") as f:
        tree = ast.parse(f.read(), filename=path)
    package = Package(tree)
    found = {f"{cls}.{name}": node for cls, methods in package.classes.items() for name, node in methods.items()}
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
        for kind, line, what in package.forms(name, node, abi_line):
            if kind in WHY:
                findings.append(f"{where} {what}: {WHY[kind]}; {HELP}")
                continue
            for k, how in package.reach(what).items():
                if kind == "call-allowed" and k == "raise":
                    continue  # a helper poll calls after the ABI has returned may raise the handler's exception
                findings.append(f"{where} calls {what} at line {line}, and {how}: {WHY[k]}; {HELP}")
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
    print(f"lint-clients: {', '.join(languages)}: the per-message functions, and the package's functions they call, "
          f"hold no lock, no await and no raise")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
