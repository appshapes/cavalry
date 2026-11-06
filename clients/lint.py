#!/usr/bin/env python3
"""The check `just lint` runs over the generated clients (clients/README.md; ADR 0046, the
boundary's second part): the per-message functions a generator writes, the wrapper of each
of the ABI's seven per-message functions (`crates/cavalry-ffi/README.md`: the claim, the
reserved value, the commit and abort, the offer, the poll and the header's values) and the
trampoline that calls the user's handler, hold no lock, no `await` and no raise, in the
language's forms, and so does every function of the package they call. For Python those
forms are `threading`'s locks and anything else of `threading`, `async def`, `await`,
`raise`, and building one of the package's own exceptions. For JavaScript, read from
`clients/node/index.js` without a parser, by a tokenizer that drops comments and strings
and matches braces, they are `async` on the function, `await`, `Promise`, `.then(`,
`throw`, building one of the package's own `Error` classes, and `Atomics.wait`, the one
blocking wait the language has. A call reaches the package's own functions by name: a
module function, a class's `__init__` or `constructor`, a method of the same class
through `self` or `this`, and any other method by its name in any class of the package.
The one raise allowed is in `poll`, after the last line that calls the ABI, where the
user's handler's exception is raised once the poll has returned
(clients/python/README.md; clients/node/README.md); a raise between two of its calls,
such as the refused poll from inside a handler and the poll's own call, is not after the
poll.

It fails on generated code under `clients/` in a language it has no rules for, and on a
package in which it finds none of the per-message functions, so that a client cannot slip
past it by renaming them. `lint-test.py` holds each rule to a generated function carrying
the form it forbids. Python 3.9 standard library only (ADR 0003).

    lint.py            check every client under clients/
"""

from __future__ import annotations

import ast
import os
import re
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
    # The Node client calls the ABI through its addon (clients/node/README.md): the addon's
    # `poll` is the one `cavalry_subscription_poll` call, and the header's fields are read
    # through its `headerField`, so the getters are the wrappers of `cavalry_header_values`.
    "node": {
        "file": "index.js",
        "functions": [
            "Publication.tryClaim",
            "Publication.setReservedValue",
            "Publication.commit",
            "Publication.abort",
            "Publication.offer",
            "Subscription.poll",
            "Header.frameLength",
            "Header.version",
            "Header.flags",
            "Header.type",
            "Header.termOffset",
            "Header.sessionId",
            "Header.streamId",
            "Header.termId",
            "Header.reservedValue",
        ],
        "raise_after": ("Subscription.poll", "addon.poll"),
    },
}
HELP = "see clients/README.md, \"What every client must pass\""
WHY = {
    "await": "no `await` on the per-message path",
    "threading": "no lock on the per-message path",
    "raise": "an Offer code is returned as a value and the handler's exception is raised by poll after the ABI "
             "returns, nothing else on the per-message path raises",
    "promise": "no promise on the per-message path: the call returns when the ABI returns",
    "wait": "no blocking wait on the per-message path",
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
        if name == allowed_in:  # the last line that calls the ABI: a raise between two of its calls is not after the poll
            for sub in ast.walk(node):
                if isinstance(sub, ast.Attribute) and sub.attr == abi_call:
                    abi_line = sub.lineno if abi_line is None else max(abi_line, sub.lineno)
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


# ------------------------------------------------------------------ JavaScript

JS_FORMS = (
    # (kind, pattern over the code with comments and strings blanked, what the finding says)
    ("await", re.compile(r"(?<![\w$.])await(?![\w$])"), "awaits"),
    ("promise", re.compile(r"(?<![\w$.])Promise(?![\w$])"), "uses `Promise`"),
    ("promise", re.compile(r"\.then\s*\("), "calls `.then`"),
    ("wait", re.compile(r"(?<![\w$.])Atomics\s*\.\s*wait(?:Async)?(?![\w$])"), "calls `Atomics.wait`"),
    ("raise", re.compile(r"(?<![\w$.])throw(?![\w$])"), "throws"),
)
# Names that are not the module's own functions when they stand before a `(`, and the
# objects whose methods are not the module's: the addon, the runtime's globals and modules.
JS_WORDS = {"if", "for", "while", "switch", "return", "catch", "function", "typeof", "new", "void", "delete",
            "in", "of", "else", "do", "try", "finally", "throw", "await", "yield", "class", "extends", "super",
            "this", "const", "let", "var", "get", "set", "static", "async"}
JS_FOREIGN = {"addon", "super", "Math", "Number", "BigInt", "Symbol", "Object", "Array", "Buffer", "path", "process",
              "console", "JSON", "Reflect", "String", "Error"}


def blank(text: str) -> str:
    """The JavaScript source with every comment, string and template literal replaced by
    spaces, newlines kept, so that positions and line numbers are the source's. A regular
    expression literal is left as it is: the generated code has none, and one holding a
    form's text would show as a finding, which is the safe side."""
    out = []
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        two = text[i:i + 2]
        if two == "//":
            j = text.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i))
            i = j
        elif two == "/*":
            j = text.find("*/", i + 2)
            j = n if j < 0 else j + 2
            out.append(re.sub(r"[^\n]", " ", text[i:j]))
            i = j
        elif c in "'\"`":
            j = i + 1
            while j < n and text[j] != c:
                j += 2 if text[j] == "\\" else 1
            j = min(j + 1, n)
            out.append(c + re.sub(r"[^\n]", " ", text[i + 1:j - 1]) + (c if j - i > 1 else ""))
            i = j
        else:
            out.append(c)
            i += 1
    return "".join(out)


def block_end(code: str, start: int) -> int:
    """The index after the `}` that closes the `{` at `start`, in blanked code."""
    depth = 0
    for i in range(start, len(code)):
        if code[i] == "{":
            depth += 1
        elif code[i] == "}":
            depth -= 1
            if depth == 0:
                return i + 1
    return len(code)


def line_of(code: str, index: int) -> int:
    return code.count("\n", 0, index) + 1


class Module:
    """The JavaScript module's own functions, as `Class.method` for a method, a getter, a
    setter or a constructor and the bare name for a module function or an arrow function
    bound to a `const`, each with its line, whether it is `async`, its body and where the
    body starts; the module's own `Error` classes; and what each function reaches, as
    `Package.reach` does for Python."""

    METHOD = re.compile(r"(?<![\w$.])(?:(static)\s+)?(?:(async)\s+)?(?:(get|set)\s+)?([A-Za-z_$][\w$]*)\s*\(")
    FUNCTION = re.compile(r"(?<![\w$.])(?:(async)\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)\s*\(")
    ARROW = re.compile(r"(?<![\w$.])(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*(async\s+)?"
                       r"(?:\([^()]*\)|[A-Za-z_$][\w$]*)\s*=>\s*\{")
    CLASS = re.compile(r"(?<![\w$.])class\s+([A-Za-z_$][\w$]*)(?:\s+extends\s+([A-Za-z_$][\w$.]*))?\s*\{")

    def __init__(self, text: str):
        self.code = blank(text)
        self.functions: dict[str, tuple[int, bool, str, int]] = {}  # label: (line, async, body, body's start)
        self.classes: dict[str, set[str]] = {}
        extends: dict[str, str] = {}
        code = self.code
        spans = []
        for m in self.CLASS.finditer(code):
            name, base = m.group(1), m.group(2)
            body_start = m.end() - 1
            body_end = block_end(code, body_start)
            spans.append((m.start(), body_end))
            self.classes[name] = set()
            if base:
                extends[name] = base
            i = body_start + 1
            while i < body_end:
                mm = self.METHOD.search(code, i, body_end)
                if mm is None:
                    break
                params_end = code.find(")", mm.end())
                brace = code.find("{", params_end)
                if params_end < 0 or brace < 0 or brace >= body_end or code[params_end + 1:brace].strip():
                    i = mm.end()
                    continue
                end = block_end(code, brace)
                method = mm.group(4)
                self.classes[name].add(method)
                self.functions[f"{name}.{method}"] = (line_of(code, mm.start()), mm.group(2) is not None,
                                                      code[brace:end], brace)
                i = end
        for m in self.FUNCTION.finditer(code):
            if any(a <= m.start() < b for a, b in spans):
                continue
            brace = code.find("{", m.end())
            self.functions[m.group(2)] = (line_of(code, m.start()), m.group(1) is not None,
                                          code[brace:block_end(code, brace)], brace)
        for m in self.ARROW.finditer(code):
            if any(a <= m.start() < b for a, b in spans):
                continue
            brace = m.end() - 1
            self.functions[m.group(1)] = (line_of(code, m.start()), m.group(2) is not None,
                                          code[brace:block_end(code, brace)], brace)
        self.exceptions: set[str] = set()
        while True:
            more = {name for name, base in extends.items() if name not in self.exceptions
                    and (base in ("Error", "TypeError", "RangeError") or base in self.exceptions)}
            if not more:
                break
            self.exceptions |= more
        self.reached: dict[str, dict[str, str]] = {}

    def callees(self, cls: str | None, text: str) -> list[tuple[int, str]]:
        """(offset, label) for each call in `text` that reaches one of the module's own
        functions: `name(` a module function, `new Name(` a class's constructor, `this.name(`
        the same class's method, and `x.name(` any class's method of that name."""
        out = []
        for m in re.finditer(r"(?<![\w$])(?:(new)\s+)?(?:([A-Za-z_$][\w$]*)\s*\.\s*)?([A-Za-z_$][\w$]*)\s*\(", text):
            is_new, owner, name = m.group(1), m.group(2), m.group(3)
            if is_new:
                if name in self.classes and "constructor" in self.classes[name]:
                    out.append((m.start(), f"{name}.constructor"))
            elif owner is None:
                if name in self.functions and name not in JS_WORDS:
                    out.append((m.start(), name))
            elif owner == "this":
                if cls and name in self.classes.get(cls, ()):
                    out.append((m.start(), f"{cls}.{name}"))
            elif owner not in JS_FOREIGN:
                for c, methods in self.classes.items():
                    if name in methods:
                        out.append((m.start(), f"{c}.{name}"))
        return out

    def forms(self, label: str, raise_after: int | None):
        """Each form in the function's own body as (kind, line, text), and each call to the
        module's own functions as ("call" or "call-allowed", line, callee); a throw or an
        exception built on a line after `raise_after` is allowed and not yielded."""
        line, is_async, body, start = self.functions[label]
        cls = label.split(".", 1)[0] if "." in label else None
        base = line_of(self.code, start) - 1
        if is_async:
            yield "async", line, "is `async`"
        for kind, pattern, what in JS_FORMS:
            for m in pattern.finditer(body):
                at = base + body.count("\n", 0, m.start()) + 1
                if kind == "raise" and raise_after is not None and at > raise_after:
                    continue
                yield kind, at, f"{what} at line {at}"
        for m in re.finditer(r"(?<![\w$])new\s+([A-Za-z_$][\w$]*)\s*\(", body):
            if m.group(1) in self.exceptions:
                at = base + body.count("\n", 0, m.start()) + 1
                if raise_after is not None and at > raise_after:
                    continue
                yield "raise", at, f"builds {m.group(1)} at line {at}"
        for offset, callee in self.callees(cls, body):
            if callee == label:
                continue
            at = base + body.count("\n", 0, offset) + 1
            allowed = raise_after is not None and at > raise_after
            yield ("call-allowed" if allowed else "call"), at, callee

    def reach(self, label: str) -> dict[str, str]:
        """{kind: how `label` reaches it}, for the first form of each kind in its body or in
        the module's functions it calls; memoized, as `Package.reach`."""
        if label in self.reached:
            return self.reached[label]
        found: dict[str, str] = {}
        self.reached[label] = found
        for kind, line, what in self.forms(label, None):
            if kind in WHY:
                found.setdefault(kind, f"{label} {what}")
            elif kind != "async":
                for k, how in self.reach(what).items():
                    found.setdefault(k, f"{label} calls {what} at line {line}, and {how}")
        return found


def node_findings(language: str, path: str, rules: dict) -> list[str]:
    with open(path, encoding="utf-8") as f:
        module = Module(f.read())
    findings = []
    present = [name for name in rules["functions"] if name in module.functions]
    if not present:
        return [f"{language}: none of the per-message functions {', '.join(rules['functions'])} is in {path}; "
                f"the generator renamed them or the rules are stale; {HELP}"]
    for name in rules["functions"]:
        if name not in module.functions:
            findings.append(f"{language}: the per-message function {name} is not in {path}; {HELP}")
    allowed_in, abi_call = rules["raise_after"]
    for name in present:
        line, _, body, start = module.functions[name]
        where = f"{language}: {name} ({os.path.relpath(path, ROOT)}:{line})"
        abi_line = None
        if name == allowed_in:  # the last line that calls the ABI through the addon
            base = line_of(module.code, start) - 1
            for m in re.finditer(re.escape(abi_call) + r"\s*\(", body):
                at = base + body.count("\n", 0, m.start()) + 1
                abi_line = at if abi_line is None else max(abi_line, at)
        for kind, at, what in module.forms(name, abi_line):
            if kind == "async":
                findings.append(f"{where} {what}: a per-message function runs on the caller's thread and returns when "
                                f"the ABI returns, with no promise around it; {HELP}")
            elif kind in WHY:
                findings.append(f"{where} {what}: {WHY[kind]}; {HELP}")
            else:
                for k, how in module.reach(what).items():
                    if kind == "call-allowed" and k == "raise":
                        continue
                    findings.append(f"{where} calls {what} at line {at}, and {how}: {WHY[k]}; {HELP}")
    return findings


CHECKERS = {"python": python_findings, "node": node_findings}


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
          f"hold no lock, no await, no promise and no raise")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
