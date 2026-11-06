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
blocking wait the language has. The Node client's addon, `clients/node/src/addon.c`, is
generated client code in C on the same path, so its per-message functions, the trampoline,
the finalizer of each Buffer, the addon's own `poll`, `headerField`, `view` and `detach`,
and the wrappers of the claim, the reserved value, the commit, the abort and the offer, are
read by the same tokenizer for C's forms: a lock, `pthread_mutex_*`, `pthread_rwlock_*`,
C11's `mtx_*`, libuv's `uv_mutex_*` and `uv_rwlock_*`, and N-API's
`napi_*threadsafe_function*`; and a raise, `napi_throw*` and `napi_fatal_*`, except in the
argument readers, which throw a TypeError or a RangeError for an argument the ABI's
parameter cannot take, before the ABI is called. A call reaches the package's own functions by name: a
module function, a class's `__init__` or `constructor`, a method of the same class
through `self` or `this`, and any other method by its name in any class of the package.
The one raise allowed is in `poll`, after the last line that calls the ABI, where the
user's handler's exception is raised once the poll has returned
(clients/python/README.md; clients/node/README.md); a raise between two of its calls,
such as the refused poll from inside a handler and the poll's own call, is not after the
poll. For C#, read from the files of `clients/dotnet` by the same tokenizer, the forms are
`lock`, `Monitor`, `Mutex`, `Semaphore`, `SpinLock` and the other waits of
`System.Threading`, `async`, `await`, `Task`, `ValueTask`, `.Wait(`, `.Result`,
`Thread.Sleep`, a thread started, `throw`, building an exception, `new`, C#'s allocation,
except a `Span` or `ReadOnlySpan` made over bytes the ABI handed over, which is a struct on
the stack, and an interpolated string; the one exception built on the per-message path is
the `HandlerException` that `Poll` throws after its ABI call, and `Header.Ensure` is the
guard that throws for a header read outside its handler call, before the ABI is reached.

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
        # The addon is generated client code in C on the same path: the functions the
        # per-message functions above call and the trampoline the ABI calls, with the
        # finalizer Node calls for each of their Buffers. The argument readers may throw a
        # TypeError or a RangeError for an argument of the wrong type, before the ABI is
        # called, as `offer("hello")` does; nothing else on the path throws.
        "addon": {
            "file": os.path.join("src", "addon.c"),
            "functions": [
                "trampoline",
                "buffer_freed",
                "fn_poll",
                "fn_headerField",
                "fn_view",
                "fn_detach",
                "fn_cavalry_publication_try_claim",
                "fn_cavalry_buffer_claim_set_reserved_value",
                "fn_cavalry_buffer_claim_commit",
                "fn_cavalry_buffer_claim_abort",
                "fn_cavalry_publication_offer",
            ],
            "readers": ["read_args", "type_error", "is_nullish", "external_arg", "bytes_arg", "struct_arg",
                        "string_arg", "int64_arg", "int32_arg", "size_arg", "bool_arg"],
        },
    },
    # The .NET client calls the ABI through LibraryImport stubs (clients/dotnet/README.md):
    # the per-message members are in Publication.cs, Subscription.cs and Header.cs, the
    # trampoline the ABI reaches is Subscription.OnFragment, which calls Deliver, and Abi.cs
    # holds the helpers they call, so every file of the package is read. `Poll` builds the
    # HandlerException after its ABI call, the one exception built on the path, and
    # `Header.Ensure` is the guard that throws for a header read outside its handler call,
    # before the ABI is reached.
    "dotnet": {
        "file": "Publication.cs",
        "files": ["Abi.cs", "CavalryException.cs", "Client.cs", "FragmentHandler.cs", "HandlerException.cs", "Header.cs",
                  "Image.cs", "Publication.cs", "Recording.cs", "Replay.cs", "Subscription.cs"],
        "functions": [
            "Publication.TryClaim",
            "Publication.SetReservedValue",
            "Publication.Commit",
            "Publication.Abort",
            "Publication.Offer",
            "Publication.Claim",
            "Subscription.Poll",
            "Subscription.OnFragment",
            "Subscription.Deliver",
            "Header.FrameLength",
            "Header.Version",
            "Header.Flags",
            "Header.Type",
            "Header.TermOffset",
            "Header.SessionId",
            "Header.StreamId",
            "Header.TermId",
            "Header.ReservedValue",
        ],
        "raise_after": ("Subscription.Poll", "Abi.cavalry_subscription_poll"),
        "guards": ["Header.Ensure"],
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
    "thread": "no thread started on the per-message path: the call returns when the ABI returns",
    "alloc": "no allocation on the per-message path",
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


# ------------------------------------------------------------------ C, the Node addon

C_FORMS = (
    # (kind, pattern over the code with comments and strings blanked, what the finding says)
    ("threading", re.compile(r"(?<![\w$])pthread_mutex_\w+\s*\("), "calls `pthread_mutex_*`"),
    ("threading", re.compile(r"(?<![\w$])pthread_rwlock_\w+\s*\("), "calls `pthread_rwlock_*`"),
    ("threading", re.compile(r"(?<![\w$])mtx_\w+\s*\("), "calls C11's `mtx_*`"),
    ("threading", re.compile(r"(?<![\w$])uv_mutex_\w+\s*\("), "calls libuv's `uv_mutex_*`"),
    ("threading", re.compile(r"(?<![\w$])uv_rwlock_\w+\s*\("), "calls libuv's `uv_rwlock_*`"),
    ("threading", re.compile(r"(?<![\w$])napi_\w*threadsafe_function\w*\s*\("),
     "calls an N-API thread-safe function, `napi_*threadsafe_function*`"),
    ("raise", re.compile(r"(?<![\w$])napi_throw\w*\s*\("), "throws, `napi_throw*`"),
    ("raise", re.compile(r"(?<![\w$])napi_fatal_(?:error|exception)\s*\("), "calls `napi_fatal_*`"),
)
C_WORDS = {"if", "for", "while", "switch", "return", "sizeof", "defined", "_Alignof", "_Static_assert"}


class CModule:
    """The addon's C, read as Module reads JavaScript: its functions at file scope, each with
    its line, body and where the body starts, found by a definition at the start of a line,
    and what each reaches, through calls to the module's own functions by name. A raise in
    one of the argument readers, `readers`, is not one: a reader throws a TypeError or a
    RangeError for an argument of the wrong type, before the ABI is called."""

    FUNCTION = re.compile(r"^(?:static\s+)?(?:inline\s+)?[A-Za-z_][\w \t*]*?[\s*]([A-Za-z_]\w*)\s*\(([^;{}]*?)\)\s*\{",
                          re.M)

    def __init__(self, text: str, readers):
        self.code = blank(text)
        self.functions: dict[str, tuple[int, str, int]] = {}  # name: (line, body, body's start)
        for m in self.FUNCTION.finditer(self.code):
            brace = m.end() - 1
            self.functions[m.group(1)] = (line_of(self.code, m.start()), self.code[brace:block_end(self.code, brace)],
                                          brace)
        self.readers = set(readers)
        self.reached: dict[str, dict[str, str]] = {}

    def forms(self, label: str):
        """Each form in the function's own body as (kind, line, text), a reader's raise
        left out, and each call to the module's own functions as ("call", line, callee)."""
        _, body, start = self.functions[label]
        base = line_of(self.code, start) - 1
        for kind, pattern, what in C_FORMS:
            if kind == "raise" and label in self.readers:
                continue
            for m in pattern.finditer(body):
                at = base + body.count("\n", 0, m.start()) + 1
                yield kind, at, f"{what} at line {at}"
        for m in re.finditer(r"(?<![\w$])([A-Za-z_]\w*)\s*\(", body):
            name = m.group(1)
            if name in self.functions and name != label and name not in C_WORDS:
                yield "call", base + body.count("\n", 0, m.start()) + 1, name

    def reach(self, label: str) -> dict[str, str]:
        """{kind: how `label` reaches it}, for the first form of each kind in its body or in
        the module's functions it calls; memoized, as `Package.reach`."""
        if label in self.reached:
            return self.reached[label]
        found: dict[str, str] = {}
        self.reached[label] = found
        for kind, line, what in self.forms(label):
            if kind in WHY:
                found.setdefault(kind, f"{label} {what}")
                continue
            for k, how in self.reach(what).items():
                found.setdefault(k, f"{label} calls {what} at line {line}, and {how}")
        return found


def c_findings(language: str, path: str, rules: dict) -> list[str]:
    """The addon's per-message functions, and the module's functions they call, against
    C_FORMS."""
    with open(path, encoding="utf-8") as f:
        module = CModule(f.read(), rules["readers"])
    present = [name for name in rules["functions"] if name in module.functions]
    if not present:
        return [f"{language}: none of the addon's per-message functions {', '.join(rules['functions'])} is in {path}; "
                f"the generator renamed them or the rules are stale; {HELP}"]
    findings = [f"{language}: the addon's per-message function {name} is not in {path}; {HELP}"
                for name in rules["functions"] if name not in module.functions]
    for name in present:
        where = f"{language}: {name} ({os.path.relpath(path, ROOT)}:{module.functions[name][0]})"
        for kind, at, what in module.forms(name):
            if kind in WHY:
                findings.append(f"{where} {what}: {WHY[kind]}; {HELP}")
                continue
            for k, how in module.reach(what).items():
                findings.append(f"{where} calls {what} at line {at}, and {how}: {WHY[k]}; {HELP}")
    return findings


def node_findings(language: str, path: str, rules: dict) -> list[str]:
    findings = []
    addon = rules.get("addon")
    if addon is not None:
        source = os.path.join(os.path.dirname(path), addon["file"])
        if os.path.exists(source):
            findings.extend(c_findings(language, source, addon))
        else:
            findings.append(f"{language}: {os.path.relpath(source, ROOT)} is missing; run "
                            f"`python3 clients/generate.py {language}`")
    with open(path, encoding="utf-8") as f:
        module = Module(f.read())
    present = [name for name in rules["functions"] if name in module.functions]
    if not present:
        return findings + [f"{language}: none of the per-message functions {', '.join(rules['functions'])} is in "
                           f"{path}; the generator renamed them or the rules are stale; {HELP}"]
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


# ------------------------------------------------------------------ C#, the .NET client

CS_FORMS = (
    # (kind, pattern over the code with comments and strings blanked, what the finding says)
    ("threading", re.compile(r"(?<![\w.])lock\s*\("), "uses `lock`"),
    ("threading", re.compile(r"(?<![\w.])(?:Monitor|Mutex|Semaphore|SemaphoreSlim|SpinLock|SpinWait|ReaderWriterLock"
                             r"|ReaderWriterLockSlim|Barrier|CountdownEvent|ManualResetEvent|ManualResetEventSlim"
                             r"|AutoResetEvent|EventWaitHandle|WaitHandle)(?![\w])"), "uses a lock or a wait of `System.Threading`"),
    ("threading", re.compile(r"(?<![\w.])System\s*\.\s*Threading\s*\.\s*Lock(?![\w])|(?<![\w.])Lock\s+\w+\s*=\s*new"),
     "uses `System.Threading.Lock`"),
    ("await", re.compile(r"(?<![\w.])await(?![\w])"), "awaits"),
    ("promise", re.compile(r"(?<![\w])(?:Task|ValueTask|TaskCompletionSource)(?![\w])"), "uses a task"),
    ("wait", re.compile(r"\.\s*(?:Wait|WaitOne|WaitAll|WaitAny|Join|GetAwaiter|GetResult)\s*\(|\.\s*Result(?![\w])"
                        r"|(?<![\w.])Thread\s*\.\s*(?:Sleep|SpinWait|Yield)\s*\("), "blocks, with `.Wait(`, `.Result`, `.Join(` or `Thread.Sleep`"),
    ("thread", re.compile(r"(?<![\w.])new\s+Thread\s*\(|(?<![\w.])ThreadPool\s*\.|\.\s*Start\s*\(\s*\)"), "starts a thread"),
    ("raise", re.compile(r"(?<![\w.])throw(?![\w])"), "throws"),
    ("alloc", re.compile(r"\$\""), "allocates an interpolated string"),
)
CS_WORDS = {"if", "for", "foreach", "while", "switch", "catch", "lock", "using", "fixed", "return", "new", "else", "do",
            "throw", "base", "this", "is", "as", "typeof", "sizeof", "nameof", "default", "checked", "unchecked", "when"}
CS_FOREIGN = {"System", "Math", "Marshal", "GC", "Console", "Environment", "Path", "File", "Directory", "NativeLibrary",
              "GCHandle", "MemoryMarshal", "Unsafe", "Thread", "Interlocked", "Volatile", "BinaryPrimitives", "List",
              "string", "Exception"}
CS_VIEWS = {"Span", "ReadOnlySpan"}


def paren_end(code: str, start: int) -> int:
    """The index of the `)` that closes the `(` at `start`, or -1."""
    depth = 0
    for i in range(start, len(code)):
        if code[i] == "(":
            depth += 1
        elif code[i] == ")":
            depth -= 1
            if depth == 0:
                return i
    return -1


class CsModule:
    """The package's own members across its files, as `Class.Member` for a method, a
    constructor or a property, each with its file, line, head (the modifiers before its
    name), body and where the body starts, found inside a class body at member level: a
    method as a name before `(`, its parameters and `{` or `=>`; a property as a name before
    `{ get` or `=>`; the package's own exception classes, those whose base reaches an
    Exception; and what each member reaches, as `Module.reach` does for JavaScript. A guard
    throws for a call made outside the handler call, before the ABI is reached, so its
    raise is not one."""

    CLASS = re.compile(r"(?<![\w.])(?:class|struct|record(?:\s+struct)?|interface)\s+(\w+)(?:\s*\([^)]*\))?"
                       r"(?:\s*:\s*([\w.]+)(?:\s*,\s*[\w.]+)*)?(?:\s+where[^{;]*)?\s*\{")
    MEMBER = re.compile(r"(?<![\w.])(\w+)\s*(\(|=>|\{\s*get)")

    def __init__(self, files: list, guards=()):
        self.files = [(path, blank(text)) for path, text in files]
        self.functions: dict[str, tuple[str, int, str, str, int, str]] = {}  # label: (file, line, head, body, start, code)
        self.classes: dict[str, set[str]] = {}
        self.guards = set(guards)
        extends: dict[str, str] = {}
        for path, code in self.files:
            for m in self.CLASS.finditer(code):
                name, base = m.group(1), m.group(2)
                body_start = m.end() - 1
                body_end = block_end(code, body_start)
                self.classes.setdefault(name, set())
                if base:
                    extends[name] = base.split(".")[-1]
                i = body_start + 1
                while i < body_end:
                    mm = self.MEMBER.search(code, i, body_end)
                    if mm is None:
                        break
                    member, opener = mm.group(1), mm.group(2)
                    if member in CS_WORDS or opener != "(" and member[0].islower():
                        i = mm.end()
                        continue
                    if opener == "(":
                        params_end = paren_end(code, mm.end() - 1)
                        rest = code[params_end + 1:params_end + 200].lstrip() if params_end >= 0 else ""
                        if params_end < 0 or not (rest.startswith("{") or rest.startswith("=>")):
                            i = mm.end()
                            continue
                        at = params_end + 1 + (len(code[params_end + 1:params_end + 200]) - len(rest))
                    else:
                        at = mm.start() + len(member) + (len(mm.group(0)) - len(member) - len(mm.group(2).lstrip()))
                        at = code.index(mm.group(2).strip()[0], mm.start() + len(member))
                    if code[at] == "{":
                        end = block_end(code, at)
                    else:  # an expression body: to the `;` that ends it
                        end = self._statement_end(code, at)
                    if end > body_end:
                        i = mm.end()
                        continue
                    self.classes[name].add(member)
                    head = code[max(code.rfind(c, 0, mm.start()) for c in ";{}") + 1:mm.start()]
                    self.functions[f"{name}.{member}"] = (path, line_of(code, mm.start()), head, code[at:end], at, code)
                    i = end
        self.exceptions: set[str] = set()
        while True:
            more = {name for name, base in extends.items() if name not in self.exceptions
                    and (base.endswith("Exception") or base in self.exceptions)}
            if not more:
                break
            self.exceptions |= more
        self.reached: dict[str, dict[str, str]] = {}

    @staticmethod
    def _statement_end(code: str, start: int) -> int:
        """The index after the `;` that ends the expression starting at `start`, braces and
        parentheses matched."""
        depth = 0
        for i in range(start, len(code)):
            c = code[i]
            if c in "({[":
                depth += 1
            elif c in ")}]":
                depth -= 1
            elif c == ";" and depth == 0:
                return i + 1
        return len(code)

    def callees(self, cls: str, text: str) -> list:
        """(offset, label) for each call in `text` that reaches one of the package's own
        members: `Name(` a member of the same class, `new Name(` a class's constructor,
        `this.Name(` the same class's member, `Class.Name(` that class's member, `x.Name(`
        or `(...).Name(` any class's member of that name, and a property read `x.Name` of
        a property any class has by that name."""
        out = []
        properties = {c: {m for m in members if f"{c}.{m}" in self.functions and not self._is_method(c, m)}
                      for c, members in self.classes.items()}
        for m in re.finditer(r"(?<![\w.])(?:(new)\s+)?(?:([\w.]+|\))\s*\.\s*)?(\w+)(\s*\()?", text):
            is_new, owner, name, call = m.group(1), m.group(2), m.group(3), m.group(4)
            if owner == ")":  # a call on a cast or a parenthesised expression: a member of that name in any class
                owner = "(expression)"
            if is_new:
                if call and name in self.classes and name in self.classes[name]:
                    out.append((m.start(), f"{name}.{name}"))
                continue
            if name in CS_WORDS:
                continue
            if owner is None:
                if call and name in self.classes.get(cls, ()) or name in properties.get(cls, ()):
                    if name != cls:
                        out.append((m.start(), f"{cls}.{name}"))
                continue
            last = owner.split(".")[-1]
            if last in ("this", "base"):
                if name in self.classes.get(cls, ()):
                    out.append((m.start(), f"{cls}.{name}"))
            elif last in self.classes:
                if name in self.classes[last]:
                    out.append((m.start(), f"{last}.{name}"))
            elif last not in CS_FOREIGN:
                for c, members in self.classes.items():
                    if name in members and (call or name in properties.get(c, ())):
                        out.append((m.start(), f"{c}.{name}"))
        return out

    def _is_method(self, cls: str, member: str) -> bool:
        _, _, head, body, start, code = self.functions[f"{cls}.{member}"]
        return code[code.rfind(member, 0, start) + len(member):start].lstrip().startswith("(")

    def forms(self, label: str, raise_after):
        """Each form in the member's own body as (kind, line, text), and each call to the
        package's own members as ("call" or "call-allowed", line, callee); a throw or an
        exception built in a guard or on a line after `raise_after` is allowed and not
        yielded, and a call from there is allowed to raise."""
        path, line, head, body, start, code = self.functions[label]
        cls = label.split(".", 1)[0]
        base = line_of(code, start) - 1

        def place(offset):
            at = base + body.count("\n", 0, offset) + 1
            return at, raise_after is not None and at > raise_after

        if re.search(r"(?<![\w.])async(?![\w])", head):
            yield "async", line, "is `async`"
        for kind, pattern, what in CS_FORMS:
            for m in pattern.finditer(body):
                at, allowed = place(m.start())
                if kind == "raise" and (allowed or label in self.guards):
                    continue
                yield kind, at, f"{what} at line {at}"
        for m in re.finditer(r"(?<![\w.])new\s+([\w.]+)\s*(?:<[^>]*>)?\s*[(\[]", body):
            name = m.group(1).split(".")[-1]
            at, allowed = place(m.start())
            if name in self.exceptions or name.endswith("Exception"):
                if not (allowed or label in self.guards):
                    yield "raise", at, f"builds {name} at line {at}"
            elif name not in CS_VIEWS:
                yield "alloc", at, f"allocates {name} with `new` at line {at}"
        for offset, callee in self.callees(cls, body):
            if callee == label:
                continue
            at, allowed = place(offset)
            yield ("call-allowed" if allowed else "call"), at, callee

    def reach(self, label: str) -> dict[str, str]:
        """{kind: how `label` reaches it}, for the first form of each kind in its body or in
        the package's members it calls; memoized, as `Package.reach`. A guard's raise does not
        reach its callers."""
        if label in self.reached:
            return self.reached[label]
        found: dict[str, str] = {}
        self.reached[label] = found
        for kind, line, what in self.forms(label, None):
            if kind in WHY:
                found.setdefault(kind, f"{label} {what}")
            elif kind != "async":
                for k, how in self.reach(what).items():
                    if kind == "call-allowed" and k == "raise":
                        continue
                    found.setdefault(k, f"{label} calls {what} at line {line}, and {how}")
        if label in self.guards:
            found.pop("raise", None)
        return found


def dotnet_findings(language: str, path: str, rules: dict) -> list[str]:
    findings = []
    package = os.path.dirname(path)
    files = []
    for name in rules["files"]:
        file = os.path.join(package, name)
        if not os.path.exists(file):
            findings.append(f"{language}: {os.path.relpath(file, ROOT)} is missing; run `python3 clients/generate.py {language}`")
            continue
        with open(file, encoding="utf-8") as f:
            files.append((file, f.read()))
    module = CsModule(files, rules.get("guards", ()))
    present = [name for name in rules["functions"] if name in module.functions]
    if not present:
        return findings + [f"{language}: none of the per-message functions {', '.join(rules['functions'])} is in "
                           f"{os.path.relpath(package, ROOT)}; the generator renamed them or the rules are stale; {HELP}"]
    for name in rules["functions"]:
        if name not in module.functions:
            findings.append(f"{language}: the per-message function {name} is not in {os.path.relpath(package, ROOT)}; {HELP}")
    allowed_in, abi_call = rules["raise_after"]
    for name in present:
        file, line, head, body, start, code = module.functions[name]
        where = f"{language}: {name} ({os.path.relpath(file, ROOT)}:{line})"
        abi_line = None
        if name == allowed_in:  # the last line that calls the ABI's poll
            base = line_of(code, start) - 1
            for m in re.finditer(re.escape(abi_call) + r"\s*\(", body):
                at = base + body.count("\n", 0, m.start()) + 1
                abi_line = at if abi_line is None else max(abi_line, at)
        for kind, at, what in module.forms(name, abi_line):
            if kind == "async":
                findings.append(f"{where} {what}: a per-message function runs on the caller's thread and returns when "
                                f"the ABI returns, with no task around it; {HELP}")
            elif kind in WHY:
                findings.append(f"{where} {what}: {WHY[kind]}; {HELP}")
            else:
                for k, how in module.reach(what).items():
                    if kind == "call-allowed" and k == "raise":
                        continue
                    findings.append(f"{where} calls {what} at line {at}, and {how}: {WHY[k]}; {HELP}")
    return findings


CHECKERS = {"python": python_findings, "node": node_findings, "dotnet": dotnet_findings}


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
          f"hold no lock, no await, no promise, no thread, no allocation and no raise")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
