#!/usr/bin/env python3
"""Generate a language client from the committed C header.

    generate.py <language> [--check]

Reads `crates/cavalry-ffi/include/cavalry.h`, the header cbindgen writes from
`cavalry-ffi` (spec/c-abi.md; ADR 0051), and writes the client's package under
`clients/<language>/`: the binding, which declares every define, opaque handle type,
struct, the fragment handler's type and every function to the language's foreign-function
layer, and the client's own code, the wrapper types between the user's code and the ABI
that hold invariant 2's per-message rules (ADR 0046, the boundary's second part). Every
file of a package comes from here, and `just generate-clients` runs it for every
language; `--check` writes nothing, compares the package it would write in memory with
the committed files and exits 1 when one differs, which is how `just lint` holds the
packages to the header (clients/README.md). The languages are `python`, written here by
`write_python`, `node`, written by `generate_node.py` beside this file (card 206), `go`,
written by `generate_go.py` (card 207), `jvm`, written by `generate_jvm.py` (card 208), and
`dotnet`, written by `generate_dotnet.py` (card 209), each registered in `WRITERS`.

The header is parsed, not the vectors, so that a change to the header that the vectors
do not yet hold shows up here first; `generate-test.py` holds the parse to
`spec/vectors/c-abi.json`, each way. Python 3.9 standard library only (ADR 0003).
"""

from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass, field

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HEADER = os.path.join(ROOT, "crates", "cavalry-ffi", "include", "cavalry.h")
LANGUAGES = {"python": "python", "node": "node", "go": "go", "jvm": "jvm", "dotnet": "dotnet"}

# The ABI's per-message functions, as crates/cavalry-ffi/README.md and
# crates/cavalry-ffi/tests/allocations.rs name them; `generate-test.py` holds this list
# to the test's. A binding may call these differently from the rest: Python's keeps the
# GIL held across them and releases it across every other function.
PER_MESSAGE = (
    "cavalry_publication_try_claim",
    "cavalry_buffer_claim_set_reserved_value",
    "cavalry_buffer_claim_commit",
    "cavalry_buffer_claim_abort",
    "cavalry_publication_offer",
    "cavalry_subscription_poll",
    "cavalry_header_values",
    "cavalry_committed_log_poll",
    "cavalry_committed_log_entry_position",
    "cavalry_cluster_session_offer",
    "cavalry_cluster_session_poll",
    "cavalry_cluster_reply_values",
    "cavalry_cluster_service_poll",
    "cavalry_cluster_entry_values",
    "cavalry_cluster_service_reply",
    "cavalry_cluster_service_action",
)


@dataclass
class Param:
    type: str
    name: str


@dataclass
class Function:
    ret: str
    name: str
    params: list[Param]


@dataclass
class Struct:
    name: str
    fields: list[Param]


@dataclass
class Callback:
    name: str
    ret: str
    params: list[Param]


@dataclass
class Header:
    """What the header declares, in the header's order within each kind."""

    defines: list[tuple[str, str]] = field(default_factory=list)
    opaque: list[str] = field(default_factory=list)
    structs: list[Struct] = field(default_factory=list)
    callbacks: list[Callback] = field(default_factory=list)
    functions: list[Function] = field(default_factory=list)
    macros: list[str] = field(default_factory=list)


def strip_comments(text: str) -> str:
    return re.sub(r"/\*.*?\*/", "", text, flags=re.S)


def split_params(text: str) -> list[Param]:
    text = text.strip()
    if text in ("", "void"):
        return []
    params = []
    for piece in text.split(","):
        piece = " ".join(piece.split())
        m = re.match(r"^(.+?)\s*(\**)\s*(\w+)$", piece)
        if not m:
            raise SystemExit(f"generate: cannot read the parameter {piece!r}")
        params.append(Param(type=(m.group(1) + " " + m.group(2)).strip(), name=m.group(3)))
    return params


def parse(text: str) -> Header:
    """The header's declarations. cbindgen writes one shape for each kind, so a regular
    expression per kind reads it; anything the expressions miss is a change to the header
    this generator has to learn, and the test against the vectors says so."""
    header = Header()
    for m in re.finditer(r"^#define (CAVALRY_[A-Z0-9_]+) (.+)$", text, flags=re.M):
        header.defines.append((m.group(1), m.group(2).strip()))
    for m in re.finditer(r"^#define (cavalry_\w+)\(", text, flags=re.M):
        header.macros.append(m.group(1))
    clean = strip_comments(text)
    for m in re.finditer(r"typedef struct (\w+) (\w+);", clean):
        if m.group(1) == m.group(2):
            header.opaque.append(m.group(1))
    for m in re.finditer(r"typedef struct \{(.*?)\} (\w+);", clean, flags=re.S):
        fields = []
        for line in m.group(1).split(";"):
            line = " ".join(line.split())
            if line:
                fields.append(split_params(line)[0])
        header.structs.append(Struct(name=m.group(2), fields=fields))
    for m in re.finditer(r"typedef (\w+) \(\*(\w+)\)\((.*?)\);", clean, flags=re.S):
        header.callbacks.append(Callback(name=m.group(2), ret=m.group(1), params=split_params(m.group(3))))
    for m in re.finditer(r"^((?:const )?[\w]+(?: \*)?)\s*\n?\s*(cavalry_\w+)\((.*?)\);", clean, flags=re.S | re.M):
        ret, name, params = m.group(1).strip(), m.group(2), m.group(3)
        header.functions.append(Function(ret=ret, name=name, params=split_params(params)))
    return header


# ---------------------------------------------------------------- Python

CTYPES = {
    "int": "c_int",
    "int8_t": "c_int8",
    "int32_t": "c_int32",
    "int64_t": "c_int64",
    "uint8_t": "c_uint8",
    "uint16_t": "c_uint16",
    "size_t": "c_size_t",
    "bool": "c_bool",
    "void": "None",
}


def ctype(c: str, header: Header, struct_pointer_as_pointer: bool = True, function_param: bool = False) -> str:
    """The ctypes type for a C type of the header. Opaque handles, byte buffers and `void *`
    are `c_void_p`, an integer address, so a handle costs one `int` and a body's view is
    built from its address; a pointer to a caller-owned struct is `POINTER(struct)`; an
    out parameter for a handle is `POINTER(c_void_p)`. A function's `const uint8_t *`
    parameter, bytes the caller hands in, is `c_char_p`, which takes `bytes` or a `c_char`
    array and refuses anything else: `c_void_p` would take a `str` as its `wchar_t`
    buffer and publish bytes the caller never offered. The fragment handler's `buffer`
    stays an address."""
    c = " ".join(c.split())
    const = c.startswith("const ")
    base = c[6:] if const else c
    if base == "char *":
        return "c_char_p"
    if base == "uint8_t *" and const and function_param:
        return "c_char_p"
    if base in ("void *", "uint8_t *"):
        return "c_void_p"
    if base.endswith(" **"):
        return "POINTER(c_void_p)"
    if base.endswith(" *"):
        inner = base[:-2]
        if inner in header.opaque:
            return "c_void_p"
        if inner in [s.name for s in header.structs]:
            return f"POINTER({inner})" if struct_pointer_as_pointer else inner
        if inner in CTYPES:
            return f"POINTER({CTYPES[inner]})"
        raise SystemExit(f"generate: no ctypes type for {c!r}")
    if base in [cb.name for cb in header.callbacks] or base in [s.name for s in header.structs]:
        return base
    if base in CTYPES:
        return CTYPES[base]
    raise SystemExit(f"generate: no ctypes type for {c!r}")


C_LIMITS = {"INT64_MAX": "(1 << 63) - 1", "INT64_MIN": "-(1 << 63)", "INT32_MAX": "(1 << 31) - 1",
            "INT32_MIN": "-(1 << 31)", "UINT8_MAX": "255", "UINT16_MAX": "65535", "SIZE_MAX": "(1 << 64) - 1"}


def python_value(value: str) -> str:
    if value in C_LIMITS:
        return C_LIMITS[value]
    if value.startswith('"'):
        return value
    if re.fullmatch(r"-?\d+", value) or re.fullmatch(r"0x[0-9a-fA-F]+", value):
        return value
    if re.fullmatch(r"\(?-?\d+\)?", value):
        return value.strip("()")
    raise SystemExit(f"generate: cannot read the define's value {value!r}")


def write_python(header: Header) -> dict[str, str]:
    """The Python package: `cavalry/_abi.py`, the ctypes binding, and
    `cavalry/__init__.py`, the client's own code, from PYTHON_CLIENT below."""
    out = []
    out.append('"""The C ABI of spec/c-abi.md as ctypes declares it.')
    out.append("")
    out.append("Generated by clients/generate.py from crates/cavalry-ffi/include/cavalry.h; do not edit by")
    out.append("hand, `just lint` regenerates and compares (clients/README.md). Every define is a module")
    out.append("constant, every struct a `ctypes.Structure`, the fragment handler a `CFUNCTYPE`, and every")
    out.append("function has its `restype` and `argtypes` set by `load`, the per-message ones through")
    out.append("`ctypes.PyDLL` and the rest through `ctypes.CDLL`.")
    out.append('"""')
    out.append("")
    out.append("import ctypes")
    out.append("from ctypes import (CFUNCTYPE, POINTER, Structure, c_bool, c_char_p, c_int, c_int8, c_int32,")
    out.append("                    c_int64, c_size_t, c_uint8, c_uint16, c_void_p)")
    out.append("")
    out.append("# The defines, the header's constants (spec/c-abi.md, \"Constants\", \"Result codes\",")
    out.append("# \"Error codes\").")
    for name, value in header.defines:
        if name == "CAVALRY_H":
            continue
        out.append(f"{name} = {python_value(value)}")
    out.append("")
    out.append("# The opaque handle types, each a `c_void_p` here: the ABI hands out a pointer the client")
    out.append("# keeps and passes back, and never reads through.")
    for name in header.opaque:
        out.append(f"{name} = c_void_p")
    out.append("")
    for struct in header.structs:
        out.append(f"class {struct.name}(Structure):")
        out.append(f'    """`{struct.name}` of spec/c-abi.md, \"Structs\": the caller\'s copy the ABI fills."""')
        out.append("")
        out.append("    _fields_ = [")
        for f in struct.fields:
            out.append(f'        ("{f.name}", {ctype(f.type, header, struct_pointer_as_pointer=False)}),')
        out.append("    ]")
        out.append("")
        out.append("")
    for cb in header.callbacks:
        args = ", ".join([ctype(cb.ret, header)] + [ctype(p.type, header) for p in cb.params])
        out.append(f"# `{cb.name}`: ({', '.join(p.type + ' ' + p.name for p in cb.params)}).")
        out.append(f"{cb.name} = CFUNCTYPE({args})")
        out.append("")
    out.append("# Every function: name -> (restype, argtypes), in the header's order.")
    out.append("FUNCTIONS = {")
    for fn in header.functions:
        argtypes = ", ".join(ctype(p.type, header, function_param=True) for p in fn.params)
        out.append(f'    "{fn.name}": ({ctype(fn.ret, header)}, [{argtypes}]),')
    out.append("}")
    out.append("")
    names = [fn.name for fn in header.functions]
    missing = [name for name in PER_MESSAGE if name not in names]
    if missing:
        raise SystemExit(f"generate: the per-message function(s) {', '.join(missing)} are not in the header; update "
                         f"PER_MESSAGE in clients/generate.py with crates/cavalry-ffi/tests/allocations.rs")
    defines = dict(header.defines)
    version_check = [name for name in ("CAVALRY_ABI_VERSION", "CAVALRY_ERR_ABI_VERSION") if name not in defines]
    version_check += [name for name in ("cavalry_abi_version",) if name not in names]
    if version_check:
        raise SystemExit(f"generate: {', '.join(version_check)} is not in the header, and `load` checks the library's "
                         f"ABI version with it (spec/c-abi.md, \"Versions and what a reader rejects\"); teach "
                         f"clients/generate.py the header's new check")
    out.append("# The per-message functions (crates/cavalry-ffi/README.md), loaded through `ctypes.PyDLL`.")
    out.append("PER_MESSAGE = (")
    for name in PER_MESSAGE:
        out.append(f'    "{name}",')
    out.append(")")
    out.append("")
    out.append("")
    out.append("def load(path):")
    out.append('    """Load libcavalry_ffi from `path` and declare every function\'s types. First it calls')
    out.append("    `cavalry_abi_version()`, before any other function is bound or called, and refuses a")
    out.append("    library whose ABI version is not this binding's `CAVALRY_ABI_VERSION` with")
    out.append("    `cavalry.CavalryError`, code `CAVALRY_ERR_ABI_VERSION`, naming both numbers, so that a")
    out.append("    mismatch fails here and not at a function the other version lacks or reads differently")
    out.append('    (spec/c-abi.md, "Versions and what a reader rejects"). The per-message')
    out.append("    functions come through `ctypes.PyDLL`, which keeps the GIL held across the call and so")
    out.append("    around each call-back: no release and re-acquire pair per message. Every other function")
    out.append("    comes through `ctypes.CDLL`, which releases the GIL for the call, so that an add, a command")
    out.append("    or a close, which waits on the driver for up to 10 s, does not stop the process's other")
    out.append("    Python threads. Both open the one library the loader maps (clients/python/README.md,")
    out.append('    "The runtime\'s side")."""')
    out.append("    library = ctypes.CDLL(path)")
    out.append("    abi_version = library.cavalry_abi_version")
    out.append('    abi_version.restype, abi_version.argtypes = FUNCTIONS["cavalry_abi_version"]')
    out.append("    found = abi_version()")
    out.append("    if found != CAVALRY_ABI_VERSION:")
    out.append("        from . import CavalryError  # the package's error; the package has loaded by the time load runs")
    out.append('        raise CavalryError("cavalry_abi_version", CAVALRY_ERR_ABI_VERSION,')
    out.append('                           f"the library at {path} is ABI version {found} and this package was generated "')
    out.append('                           f"from a header of ABI version {CAVALRY_ABI_VERSION}; load the library built from "')
    out.append('                           f"the same checkout as the package, at CAVALRY_FFI_LIBRARY or the checkout\'s "')
    out.append('                           f"target/release, or regenerate the package from the library\'s header with "')
    out.append('                           f"`python3 clients/generate.py python`; see "')
    out.append('                           f"docs/reference/c-abi.md#versions-and-what-a-reader-rejects")')
    out.append("    held = ctypes.PyDLL(path)")
    out.append("    for name, (restype, argtypes) in FUNCTIONS.items():")
    out.append("        function = getattr(held if name in PER_MESSAGE else library, name)")
    out.append("        function.restype = restype")
    out.append("        function.argtypes = argtypes")
    out.append("        setattr(library, name, function)")
    out.append("    return library")
    abi = "\n".join(out) + "\n"
    client = PYTHON_CLIENT.replace("@ABI_VERSION@", defines["CAVALRY_ABI_VERSION"])
    return {"cavalry/_abi.py": abi, "cavalry/__init__.py": client}


PYTHON_CLIENT = '''"""The Python client over the C ABI of spec/c-abi.md (clients/python/README.md).

Generated by clients/generate.py; do not edit by hand, `just lint` regenerates and compares.
A `Client` is a context manager over a driver's directory; its `Publication`,
`Subscription`, `Recording`, `Replay`, `CommittedLog`, `ClusterSession` and
`ClusterService` wrap the ABI's handles. Every
message goes through one ABI call on the caller's thread: `try_claim`,
`set_reserved_value`, `offer`, `commit`, `abort`, `poll`, `Header.values` and a cluster
service's `reply` hold
invariant 2's rules, no lock, no `await`, no raise (the handler's own exception is raised
once `poll` has returned), and `clients/lint.py` reads them for Python's forms of each.
Those calls keep the GIL held; every other call releases it. Anything that is not a
message, an add, a command, a close, raises `CavalryError` with the ABI's code and
message. A `Client` may be shared between threads; a `Publication`, `Subscription`,
`Recording`, `Replay`, `CommittedLog`, `ClusterSession` or `Image` is used from one thread
at a time, and so is a `ClusterService` together with the `Client` it was made on, since
its poll is a call on that client (spec/c-abi.md, "Threads").
"""

import ctypes
import os
import sys
import threading

from . import _abi
from ._abi import *  # noqa: F401,F403  the header's constants, as the ABI names them

ABI_VERSION = @ABI_VERSION@

# The result codes a message path returns instead of raising (spec/c-abi.md, "Result
# codes"), under the names Aeron's C client uses without the prefix.
NOT_CONNECTED = _abi.CAVALRY_PUBLICATION_NOT_CONNECTED
BACK_PRESSURED = _abi.CAVALRY_PUBLICATION_BACK_PRESSURED
ADMIN_ACTION = _abi.CAVALRY_PUBLICATION_ADMIN_ACTION
CLOSED = _abi.CAVALRY_PUBLICATION_CLOSED
MAX_POSITION_EXCEEDED = _abi.CAVALRY_PUBLICATION_MAX_POSITION_EXCEEDED
ERROR = _abi.CAVALRY_PUBLICATION_ERROR
NULL_POSITION = _abi.CAVALRY_NULL_POSITION

# What a cluster service's handler returns for the entry it was handed (spec/c-abi.md,
# "Constants"), under the names Aeron's C client uses without the prefix. Returning
# nothing is CONTINUE.
ABORT = _abi.CAVALRY_ACTION_ABORT
BREAK = _abi.CAVALRY_ACTION_BREAK
CONTINUE = _abi.CAVALRY_ACTION_CONTINUE

_SUFFIX = ".dylib" if sys.platform == "darwin" else ".so"


class CavalryError(Exception):
    """A call that is not a message failed: `code` and `message` are the ABI's
    (`cavalry_errcode`, `cavalry_errmsg`), the message naming what to do next and its docs
    anchor; `function` is the ABI function that failed."""

    def __init__(self, function, code, message):
        super().__init__(f"{function}: {message} (error {code})")
        self.function = function
        self.code = code
        self.message = message


class HandlerError(Exception):
    """The user's handler raised an `Exception` during a poll: a subscription's fragment
    handler, a committed log's entry handler when `committed_log` is true, or a cluster
    session's reply handler when `cluster` is "session". The ABI's
    poll had consumed every fragment it delivered, so the poll handed every one to the
    handler, once and in order, the ones after a raise included: `fragments` is the
    poll's result, the count of fragments, of entries or of replies, or -1 when the poll
    also stopped
    on a fault, which the `fault()` of the object that was polled reads until its next
    poll, `Subscription.fault`, `CommittedLog.fault` or `ClusterSession.fault`; `raised`
    is how many of the
    handler's calls raised; `__cause__` is the first of their exceptions. A
    `KeyboardInterrupt`, `SystemExit` or any other exception that is not an `Exception`
    is raised by poll as itself.

    A cluster service's entry handler, `cluster` "service", is the one whose failure
    stops its poll: the entry the handler raised on, or returned what is no action for,
    is aborted, so it is not taken, the poll ends at it and the next poll hands it over
    again. `fragments` is then the count of entries the poll took before that entry,
    `raised` is 1, and `__cause__` is the handler's exception, or a `CavalryError` with
    code `CAVALRY_ERR_CLUSTER_ACTION` that names the value the handler returned."""

    def __init__(self, fragments, raised, cause, committed_log=False, cluster=None):
        self.fragments = fragments
        self.raised = raised
        self.committed_log = committed_log
        self.cluster = cluster
        self.__cause__ = cause
        if cluster == "service":
            # The one poll a handler's failure stops (clients/python/README.md, "The
            # cluster service").
            took = (f"after the {fragments} {'entry' if fragments == 1 else 'entries'} this poll took" if fragments >= 0
                    else "in a poll that also stopped on a fault, which this ClusterService's fault() reads until its "
                         "next poll")
            super().__init__(f"the entry handler raised, or returned what is no action, {took}. Its entry was aborted: "
                             f"it is not taken, the poll ended at it, and the next poll hands it over again. Fix the "
                             f"handler: what it did, {cause!r}, is this error's __cause__, whose traceback Python "
                             f"prints above this one; every poll hands the entry over, and raises this, until the "
                             f"handler takes it. See clients/python/README.md#the-exception")
            return
        if cluster == "session":
            one, many = "reply", "replies"
            count = f"the {fragments} {one if fragments == 1 else many} this poll handed over"
            fault = ("the replies this poll handed over before it stopped on a fault, which this ClusterSession's "
                     "fault() reads until its next poll")
        elif committed_log:
            one, many = "entry", "entries"
            count = f"the {fragments} {one if fragments == 1 else many} this poll delivered"
            # The fault of the log's own image is this one poll's to return; the log
            # keeps it (clients/python/README.md, "The committed log").
            fault = ("the entries this poll delivered before it stopped on a fault, which this CommittedLog's "
                     "fault() reads until its next poll, and its faulted() from then on when the fault is the "
                     "log's own image's")
        else:
            one, many = "fragment", "fragments"
            count = f"the {fragments} fragment(s) this poll delivered"
            fault = "the fragments this poll delivered before it stopped on a fault, which Subscription.fault() reads"
        delivered = count if fragments >= 0 else fault
        super().__init__(f"the {one} handler raised on {raised} of {delivered}; every {one} reached the handler "
                         f"once, in order. Fix the handler: its first exception, {cause!r}, is this error's __cause__, "
                         f"whose traceback Python prints above this one; polling again does not deliver these "
                         f"{many} again. See clients/python/README.md#the-exception")


def find_library():
    """The shared library: `CAVALRY_FFI_LIBRARY` when set, else `target/release/libcavalry_ffi`
    of the checkout this package sits in, which `cargo build --release -p cavalry-ffi --lib`
    writes."""
    path = os.environ.get("CAVALRY_FFI_LIBRARY")
    if path:
        return path
    here = os.path.dirname(os.path.abspath(__file__))
    for _ in range(6):
        here = os.path.dirname(here)
        candidate = os.path.join(here, "target", "release", "libcavalry_ffi" + _SUFFIX)
        if os.path.exists(candidate):
            return candidate
    raise CavalryError("find_library", 0,
                       "libcavalry_ffi" + _SUFFIX + " was not found: build it with `cargo build --release -p"
                       " cavalry-ffi --lib` in the checkout, or set CAVALRY_FFI_LIBRARY to its path;"
                       " see clients/python/README.md")


_library = None


def library(path=None):
    """The loaded ABI, loaded once per process. A library whose ABI version is not this
    package's is refused here, before any other function, with `CavalryError`, code
    `CAVALRY_ERR_ABI_VERSION` (`_abi.load`)."""
    global _library
    if _library is None:
        _library = _abi.load(path or find_library())
    return _library


def _error(abi, function):
    code = abi.cavalry_errcode()
    message = abi.cavalry_errmsg()
    return CavalryError(function, code, message.decode("utf-8", "replace") if message else "")


def _check(abi, function, result):
    if result != 0:
        raise _error(abi, function)


class Client:
    """A client of the driver at `dir`, or at `CAVALRY_DIR`, or at the driver's default
    directory: `cavalry_context_init_v` with this package's ABI version, then `cavalry_init`.
    A context manager that closes on exit; `close` is idempotent. Every call on the client's
    handle releases the GIL, an add or a command for up to the driver's 10 s, and takes the
    client's lock, since the ABI runs one call on a handle at a time (spec/c-abi.md,
    "Threads"): threads may share a client."""

    def __init__(self, dir=None, library_path=None):
        self._abi = library(library_path)
        self._lock = threading.Lock()
        context = ctypes.c_void_p()
        _check(self._abi, "cavalry_context_init_v",
               self._abi.cavalry_context_init_v(ctypes.byref(context), ABI_VERSION))
        try:
            if dir is not None:
                _check(self._abi, "cavalry_context_set_dir",
                       self._abi.cavalry_context_set_dir(context, os.fsencode(dir)))
            handle = ctypes.c_void_p()
            _check(self._abi, "cavalry_init", self._abi.cavalry_init(ctypes.byref(handle), context))
        finally:
            self._abi.cavalry_context_close(context)
        self._handle = handle.value
        try:
            self.dir = os.fsdecode(self._abi.cavalry_dir(self._handle))
            self.client_id = self._abi.cavalry_client_id(self._handle)
        except BaseException:  # no handle is left behind with its conductor running
            self._abi.cavalry_close(self._handle)
            self._handle = None
            raise

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    def close(self):
        with self._lock:
            if self._handle is not None:
                _check(self._abi, "cavalry_close", self._abi.cavalry_close(self._handle))
                self._handle = None

    @property
    def is_closed(self):
        with self._lock:
            return self._handle is None or self._abi.cavalry_is_closed(self._handle)

    def closed(self):
        """0 while open, else why the client closed, `CAVALRY_CLOSED_*`."""
        with self._lock:
            return self._abi.cavalry_closed(self._handle)

    def _command(self, function, *args):
        with self._lock:
            _check(self._abi, function, getattr(self._abi, function)(self._handle, *args))

    def _add(self, function, channel, stream_id):
        handle = ctypes.c_void_p()
        with self._lock:
            _check(self._abi, function,
                   getattr(self._abi, function)(ctypes.byref(handle), self._handle, channel.encode(), stream_id))
        return handle.value

    def add_publication(self, channel, stream_id):
        return Publication(self._abi, self._add("cavalry_add_publication", channel, stream_id))

    def add_exclusive_publication(self, channel, stream_id):
        return Publication(self._abi, self._add("cavalry_add_exclusive_publication", channel, stream_id))

    def add_subscription(self, channel, stream_id):
        return Subscription(self._abi, self._add("cavalry_add_subscription", channel, stream_id))

    def start_recording(self, channel, stream_id):
        return Recording(self._abi, self._add("cavalry_start_recording", channel, stream_id))

    def stop_recording(self, recording_id):
        self._command("cavalry_stop_recording", recording_id)

    def add_destination(self, publication, channel):
        """Add the destination `channel`, `cavalry:udp?endpoint=<host>:<port>`, to the
        control channel of `publication`, a Publication of this client; answered by the
        driver (spec/client-protocol.md, "Commands"; ADR 0053)."""
        self._command("cavalry_add_destination", publication._handle, channel.encode())

    def remove_destination(self, publication, channel):
        """Remove the destination `channel`, as it was added; one not there is removed
        by nothing."""
        self._command("cavalry_remove_destination", publication._handle, channel.encode())

    def truncate_recording(self, recording_id, position):
        self._command("cavalry_truncate_recording", recording_id, position)

    def purge_segments(self, recording_id, new_start_position):
        self._command("cavalry_purge_segments", recording_id, new_start_position)

    def purge_recording(self, recording_id):
        self._command("cavalry_purge_recording", recording_id)

    def start_replay(self, recording_id, position, length, replay_stream_id, channel):
        handle = ctypes.c_void_p()
        with self._lock:
            _check(self._abi, "cavalry_start_replay",
                   self._abi.cavalry_start_replay(ctypes.byref(handle), self._handle, recording_id, position, length,
                                                  replay_stream_id, channel.encode()))
        return Replay(self._abi, handle.value)

    def stop_replay(self, replay_session_id):
        self._command("cavalry_stop_replay", replay_session_id)

    def committed_log(self, position):
        """The committed log of the cluster member on this client's driver, opened at
        `position`, 0 or the `position()` an earlier `CommittedLog` reached: a
        `CommittedLog`, or None while the driver hosts no member that has a recording of
        its log, so a service started before its member asks again (spec/cluster.md, "The
        service"; spec/c-abi.md, "Functions", committed log)."""
        handle = ctypes.c_void_p()
        with self._lock:
            _check(self._abi, "cavalry_committed_log",
                   self._abi.cavalry_committed_log(ctypes.byref(handle), self._handle, position))
        return CommittedLog(self._abi, handle.value, self) if handle.value else None

    def connect_cluster(self, members, response_channel):
        """Open a session with a cluster: a `ClusterSession`. `members` is the members
        string the cluster's members were started with, and `response_channel` a channel
        of this host for the cluster's answers, `cavalry:udp?endpoint=<host>:<port>`. The
        call returns once the leader has opened the session, and waits for that for 20 s
        at most beyond its adds, with the GIL released. It raises `CavalryError` with
        `CAVALRY_ERR_CLUSTER_MEMBERS`, `CAVALRY_ERR_CLUSTER_REFUSED`,
        `CAVALRY_ERR_CLUSTER_VERSION` or `CAVALRY_ERR_CLUSTER_CONNECT_TIMEOUT`, with a
        fault's code, or with the client's or the driver's (spec/cluster.md, "Sessions";
        spec/c-abi.md, "Functions", cluster session; ADR 0122)."""
        handle = ctypes.c_void_p()
        with self._lock:
            _check(self._abi, "cavalry_connect_cluster",
                   self._abi.cavalry_connect_cluster(ctypes.byref(handle), self._handle, members.encode(),
                                                     response_channel.encode()))
        return ClusterSession(self._abi, handle.value)

    def cluster_service(self):
        """A cluster's service beside the member on this client's driver: a
        `ClusterService`. The call sends nothing: the service's first poll opens the
        member's log. The service's poll and its close are calls on this client, and the
        poll takes no lock, so from here until the service is closed this client is used
        by the thread that polls the service and by no other; give the process's other
        threads a `Client` of their own (spec/cluster.md, "The service"; spec/c-abi.md,
        "Functions", cluster service, and "Threads"; ADR 0123)."""
        handle = ctypes.c_void_p()
        with self._lock:
            _check(self._abi, "cavalry_cluster_service",
                   self._abi.cavalry_cluster_service(ctypes.byref(handle), self._handle))
        return ClusterService(self._abi, handle.value, self)


class Publication:
    """A publication: `try_claim` and `offer` return the position after the frame or a code
    below zero, never raising for a code (`offer` refuses a message that is not `bytes`
    before the call); `claim` is the view over the slice the last `try_claim` reserved, a
    `memoryview` built from the one `cavalry_buffer_claim_t` this object keeps."""

    def __init__(self, abi, handle):
        self._abi = abi
        self._handle = handle
        self._claim = _abi.cavalry_buffer_claim_t()
        self._claim_ref = ctypes.byref(self._claim)
        self.claim = None
        constants = _abi.cavalry_publication_constants_t()
        _check(abi, "cavalry_publication_constants", abi.cavalry_publication_constants(handle, ctypes.byref(constants)))
        self.channel = constants.channel.decode()
        self.registration_id = constants.registration_id
        self.session_id = constants.session_id
        self.stream_id = constants.stream_id
        self.max_payload_length = constants.max_payload_length
        self.max_message_length = constants.max_message_length
        self.is_exclusive = constants.is_exclusive

    def try_claim(self, length):
        """Claim `length` bytes: the position after the frame, with `self.claim` the writable
        view over them, or a result code below zero with `self.claim` as it was. That is
        None when no claim is outstanding. With one outstanding, the ABI refuses the call
        with `ERROR` and `CAVALRY_ERR_CLAIM_OUTSTANDING` and leaves the claim as it was, so
        `self.claim` stays its view, which its `commit` or `abort` still releases
        (spec/c-abi.md, "Calls during a claim or a poll"). One ABI call; the view is this
        client's one allocation per message (README, "The runtime's side")."""
        result = self._abi.cavalry_publication_try_claim(self._handle, length, self._claim_ref)
        if result >= 0:
            self.claim = memoryview((ctypes.c_uint8 * self._claim.length).from_address(self._claim.data)).cast("B")
        return result

    def _release(self):
        """Release the view over the claim, so that a view kept past its commit or abort
        raises ValueError on use instead of writing into the log. A view the caller has
        exported and still holds, a `numpy.frombuffer` array say, cannot be released and
        stays the caller's to drop before the next claim."""
        claim = self.claim
        self.claim = None
        if claim is not None:
            try:
                claim.release()
            except BufferError:
                pass

    def commit(self):
        """Commit the claim, releasing its view first: the position after the frame, or -1."""
        self._release()
        return self._abi.cavalry_buffer_claim_commit(self._claim_ref)

    def abort(self):
        """Abort the claim, leaving padding, releasing its view first: the position after
        the frame, or -1."""
        self._release()
        return self._abi.cavalry_buffer_claim_abort(self._claim_ref)

    def set_reserved_value(self, value):
        return self._abi.cavalry_buffer_claim_set_reserved_value(self._claim_ref, value)

    def offer(self, message):
        """Offer `message`, `bytes` or a `ctypes` `c_char` array such as
        `ctypes.create_string_buffer(n)`, which a caller may reuse across offers: the
        position after it, or a result code below zero. A `str`, `bytearray`,
        `memoryview` or anything else is refused by `ctypes` with `ctypes.ArgumentError`
        before the ABI is called, so nothing is published."""
        return self._abi.cavalry_publication_offer(self._handle, message, len(message))

    def position(self):
        result = self._abi.cavalry_publication_position(self._handle)
        if result < 0:
            raise _error(self._abi, "cavalry_publication_position")
        return result

    def position_limit(self):
        return self._abi.cavalry_publication_position_limit(self._handle)

    @property
    def is_connected(self):
        return self._abi.cavalry_publication_is_connected(self._handle)

    @property
    def is_closed(self):
        return self._abi.cavalry_publication_is_closed(self._handle)

    def close(self):
        """Close the publication. The ABI's close aborts a claim still outstanding, and the
        claim's view is released first, so a view kept past the close raises ValueError on
        use instead of writing into a log the close gives back. The claim struct is zeroed
        before the ABI's close, so that `commit`, `abort` and `set_reserved_value` after it
        hand the ABI a struct that names no publication, which it answers with -1 and
        `CAVALRY_ERR_NO_CLAIM` without reading the handle the close freed (spec/c-abi.md,
        "Which claim a struct names")."""
        if self._handle is not None:
            self._release()
            ctypes.memset(ctypes.addressof(self._claim), 0, ctypes.sizeof(self._claim))
            _check(self._abi, "cavalry_publication_close", self._abi.cavalry_publication_close(self._handle))
            self._handle = None


class Header:
    """The frame header a poll hands the handler, valid for the call: `values()` reads the
    header's fields through `cavalry_header_values` into the one struct this object keeps.
    The trampoline clears its pointer once the handler returns, so outside the call
    `values()` is None and a property raises AttributeError, and the ABI is never handed
    a stale pointer."""

    def __init__(self, abi):
        self._abi = abi
        self._pointer = None
        self._values = _abi.cavalry_header_values_t()
        self._values_ref = ctypes.byref(self._values)
        # ctypes builds a new object on every read of a struct field; this one, over
        # `_values`' memory, reads what each `cavalry_header_values` call writes there.
        self._frame = self._values.frame

    def values(self):
        """The frame's fields: the one `cavalry_header_values_frame_t` this object keeps,
        over the struct each call fills, so it reads the latest call's header and a field
        to keep is copied out. None when the ABI refuses the pointer, which on the
        handler's own header it cannot, so this never raises."""
        if self._abi.cavalry_header_values(self._pointer, self._values_ref) != 0:
            return None
        return self._frame

    @property
    def session_id(self):
        return self.values().session_id

    @property
    def stream_id(self):
        return self.values().stream_id

    @property
    def term_id(self):
        return self.values().term_id

    @property
    def term_offset(self):
        return self.values().term_offset

    @property
    def frame_length(self):
        return self.values().frame_length

    @property
    def flags(self):
        return self.values().flags

    @property
    def type(self):
        return self.values().type

    @property
    def version(self):
        return self.values().version

    @property
    def reserved_value(self):
        return self.values().reserved_value


class Image:
    """One of a subscription's images, valid until the subscription's next poll or close."""

    def __init__(self, abi, pointer):
        self._abi = abi
        self._pointer = pointer
        constants = _abi.cavalry_image_constants_t()
        _check(abi, "cavalry_image_constants", abi.cavalry_image_constants(pointer, ctypes.byref(constants)))
        self.source_identity = constants.source_identity.decode()
        self.correlation_id = constants.correlation_id
        self.session_id = constants.session_id
        self.initial_term_id = constants.initial_term_id
        self.position_bits_to_shift = constants.position_bits_to_shift

    def position(self):
        return self._abi.cavalry_image_position(self._pointer)

    @property
    def is_end_of_stream(self):
        return self._abi.cavalry_image_is_end_of_stream(self._pointer)


class Subscription:
    """A subscription: `poll(handler, fragment_limit)` calls `handler(body, header)` for each
    fragment, `body` a `memoryview` into the log valid for the call and `header` the one
    `Header` this object keeps, and returns the fragment count. Every fragment the ABI
    delivers reaches the handler, whether or not an earlier call raised; the exceptions
    are kept until the ABI has returned, then the first is raised as `HandlerError`, or one
    that is not an `Exception` as itself."""

    def __init__(self, abi, handle):
        self._abi = abi
        self._handle = handle
        self._header = Header(abi)
        self._handler = None
        self._pending = None
        self._interrupt = None
        self._raised = 0
        self._trampoline = _abi.cavalry_fragment_handler_t(self._on_fragment)
        constants = _abi.cavalry_subscription_constants_t()
        _check(abi, "cavalry_subscription_constants", abi.cavalry_subscription_constants(handle, ctypes.byref(constants)))
        self.channel = constants.channel.decode()
        self.registration_id = constants.registration_id
        self.stream_id = constants.stream_id

    def _on_fragment(self, clientd, buffer, length, header):
        # Once the handler returns, the header's pointer is cleared and the body's view
        # released, so a kept body raises ValueError on use; a BufferError says the
        # handler kept an export of the body past its call.
        try:
            body = memoryview((ctypes.c_uint8 * length).from_address(buffer)).cast("B")
            self._header._pointer = header
            try:
                self._handler(body, self._header)
            finally:
                self._header._pointer = None
                body.release()
        except BaseException as exc:  # kept for poll to raise once the ABI has returned
            self._keep(exc)

    def _keep(self, exc):
        """Count a handler call that raised; keep the first `Exception` and the first
        exception that is not one, a `KeyboardInterrupt` or a `SystemExit`."""
        self._raised += 1
        if isinstance(exc, Exception):
            if self._pending is None:
                self._pending = exc
        elif self._interrupt is None:
            self._interrupt = exc

    def poll(self, handler, fragment_limit):
        """One ABI call; the handler's exceptions, if any, are raised after it returns: one
        that is not an `Exception` as itself, else the first as `HandlerError`. Called from
        a handler of this subscription's own poll, it passes the call to the ABI, which
        refuses it with -1 and `CAVALRY_ERR_POLLING`, and touches nothing, so the poll in
        progress goes on with its own handler and its own count of raises (spec/c-abi.md,
        "Calls during a claim or a poll")."""
        if self._handler is not None:
            return self._abi.cavalry_subscription_poll(self._handle, self._trampoline, None, fragment_limit)
        self._handler = handler
        try:
            fragments = self._abi.cavalry_subscription_poll(self._handle, self._trampoline, None, fragment_limit)
        finally:
            self._handler = None
        if self._raised:
            pending, interrupt, raised = self._pending, self._interrupt, self._raised
            self._pending = self._interrupt = None
            self._raised = 0
            if interrupt is not None:
                raise interrupt
            raise HandlerError(fragments, raised, pending)
        return fragments

    def fault(self):
        """What the last poll stopped on: (code, session_id, position); code 0 for none."""
        fault = _abi.cavalry_fault_t()
        _check(self._abi, "cavalry_subscription_fault", self._abi.cavalry_subscription_fault(self._handle, ctypes.byref(fault)))
        return fault.code, fault.session_id, fault.position

    def image_count(self):
        return self._abi.cavalry_subscription_image_count(self._handle)

    def image_at_index(self, index):
        pointer = self._abi.cavalry_subscription_image_at_index(self._handle, index)
        return Image(self._abi, pointer) if pointer else None

    def images(self):
        return [self.image_at_index(i) for i in range(self.image_count())]

    @property
    def is_connected(self):
        return self._abi.cavalry_subscription_is_connected(self._handle)

    @property
    def is_closed(self):
        return self._abi.cavalry_subscription_is_closed(self._handle)

    def close(self):
        if self._handle is not None:
            _check(self._abi, "cavalry_subscription_close", self._abi.cavalry_subscription_close(self._handle))
            self._handle = None


class Recording:
    """A recording the driver makes; `close` frees the handle and stops nothing."""

    def __init__(self, abi, handle):
        self._abi = abi
        self._handle = handle
        constants = _abi.cavalry_recording_constants_t()
        _check(abi, "cavalry_recording_constants", abi.cavalry_recording_constants(handle, ctypes.byref(constants)))
        self.channel = constants.channel.decode()
        self.recording_id = constants.recording_id
        self.session_id = constants.session_id
        self.stream_id = constants.stream_id

    def position(self):
        return self._abi.cavalry_recording_position(self._handle)

    def synced_position(self):
        return self._abi.cavalry_recording_synced_position(self._handle)

    def close(self):
        if self._handle is not None:
            _check(self._abi, "cavalry_recording_close", self._abi.cavalry_recording_close(self._handle))
            self._handle = None


class Replay:
    """A replay the driver plays; `ended()` is (ended, position, reason)."""

    def __init__(self, abi, handle):
        self._abi = abi
        self._handle = handle
        constants = _abi.cavalry_replay_constants_t()
        _check(abi, "cavalry_replay_constants", abi.cavalry_replay_constants(handle, ctypes.byref(constants)))
        self.channel = constants.channel.decode()
        self.replay_session_id = constants.replay_session_id
        self.recording_id = constants.recording_id
        self.start_position = constants.start_position
        self.session_id = constants.session_id
        self.stream_id = constants.stream_id

    def position(self):
        return self._abi.cavalry_replay_position(self._handle)

    def ended(self):
        position = ctypes.c_int64()
        reason = ctypes.c_int32()
        done = self._abi.cavalry_replay_ended(self._handle, ctypes.byref(position), ctypes.byref(reason))
        return done, position.value, reason.value

    def close(self):
        if self._handle is not None:
            _check(self._abi, "cavalry_replay_close", self._abi.cavalry_replay_close(self._handle))
            self._handle = None


class CommittedLog:
    """A service's read of a cluster's committed log, beside the member on the client's
    driver (spec/cluster.md, "The service"): `poll(handler, fragment_limit)` calls
    `handler(body, header, position)` for each entry a quorum has committed, in order and
    none at or past the member's commit position, `body` a `memoryview` into the log valid
    for the call, `header` the one `Header` this object keeps and `position` the entry's
    position in the log, and returns the entry count, or the ABI's -1 when the poll
    stopped on a fault, which `fault()` reads until the next poll. Every entry the ABI
    delivers reaches the handler, whether or not an earlier call raised; the exceptions
    are kept until the ABI has returned, then the first is raised as `HandlerError`, or
    one that is not an `Exception` as itself, as a subscription's poll raises them.
    `position()` is the position after the last entry delivered: where a service that has
    applied every entry it was given opens the log again. `is_ended` says when to stop
    reading and what tells a whole end from a short one."""

    def __init__(self, abi, handle, client):
        self._abi = abi
        self._handle = handle
        self._client = client
        self._header = Header(abi)
        self._handler = None
        self._pending = None
        self._interrupt = None
        self._raised = 0
        self._trampoline = _abi.cavalry_fragment_handler_t(self._on_fragment)
        constants = _abi.cavalry_committed_log_constants_t()
        if abi.cavalry_committed_log_constants(handle, ctypes.byref(constants)) != 0:
            # The log is open and no object will hold it: its error is read first, since
            # the close sets the thread's own, and then the log is closed, as the Go
            # client closes one it cannot return.
            error = _error(abi, "cavalry_committed_log_constants")
            with client._lock:
                abi.cavalry_committed_log_close(handle, client._handle)
            self._handle = None
            raise error
        self.member_id = constants.member_id
        self.recording_id = constants.recording_id

    def _on_fragment(self, clientd, buffer, length, header):
        # As a subscription's trampoline: once the handler returns, the header's pointer
        # is cleared and the body's view released. The entry's position is read from the
        # header before the handler runs, while the pointer is the ABI's.
        try:
            body = memoryview((ctypes.c_uint8 * length).from_address(buffer)).cast("B")
            self._header._pointer = header
            try:
                self._handler(body, self._header, self._abi.cavalry_committed_log_entry_position(header))
            finally:
                self._header._pointer = None
                body.release()
        except BaseException as exc:  # kept for poll to raise once the ABI has returned
            self._keep(exc)

    def _keep(self, exc):
        """Count a handler call that raised; keep the first `Exception` and the first
        exception that is not one, a `KeyboardInterrupt` or a `SystemExit`."""
        self._raised += 1
        if isinstance(exc, Exception):
            if self._pending is None:
                self._pending = exc
        elif self._interrupt is None:
            self._interrupt = exc

    def poll(self, handler, fragment_limit):
        """One ABI call, and its result: the count of entries delivered, or -1 when the
        poll stopped on a fault. `fault()` then reads the fault, until the next poll: a
        code of `CAVALRY_ERR_FAULT_IMAGE` or `CAVALRY_ERR_FAULT_IMAGES_FULL` is the one
        report that the replay's image failed a check or could not join, the log is ended
        short of its entries, and the next poll returns 0 and clears the fault here;
        `faulted()` keeps it (`is_ended`). The handler's exceptions, if any, are raised after the
        ABI call returns: one that is not an `Exception` as itself, else the first as
        `HandlerError`, whose `fragments` is this result, -1 included. An entry whose
        handler call raised was delivered and is not delivered again: `position()` is past
        it, and past every entry this poll delivered after it. Called from a handler of
        this log's own poll, it passes the call to the ABI, which refuses it with -1 and
        `CAVALRY_ERR_POLLING`, and touches nothing (spec/c-abi.md, "Calls during a claim
        or a poll")."""
        if self._handler is not None:
            return self._abi.cavalry_committed_log_poll(self._handle, self._trampoline, None, fragment_limit)
        self._handler = handler
        try:
            entries = self._abi.cavalry_committed_log_poll(self._handle, self._trampoline, None, fragment_limit)
        finally:
            self._handler = None
        if self._raised:
            pending, interrupt, raised = self._pending, self._interrupt, self._raised
            self._pending = self._interrupt = None
            self._raised = 0
            if interrupt is not None:
                raise interrupt
            raise HandlerError(entries, raised, pending, committed_log=True)
        return entries

    def fault(self):
        """What the last poll stopped on: (code, session_id, position); code 0 when the last
        poll returned a count. A poll that returns a count clears it, so read it after a
        poll that returned -1 and before the next."""
        fault = _abi.cavalry_fault_t()
        _check(self._abi, "cavalry_committed_log_fault",
               self._abi.cavalry_committed_log_fault(self._handle, ctypes.byref(fault)))
        return fault.code, fault.session_id, fault.position

    def position(self):
        """The position after the last entry delivered, or the position the log was opened
        at before the first."""
        return self._abi.cavalry_committed_log_position(self._handle)

    def commit_position(self):
        """The bound of the last poll: the highest commit position taken from the member's
        counter, 0 before the first poll."""
        return self._abi.cavalry_committed_log_commit_position(self._handle)

    @property
    def is_ended(self):
        """Whether this log has come to its end: no poll will deliver another entry. Stop
        reading when it is true, not when a poll returns 0. The log delivered every entry
        it was owed only when its three reports are all None: `failed_at()`,
        `withdrawn_at()` and `faulted()`. In every case what the log delivered ends at
        `position()`, where a service opens the log again."""
        return self._abi.cavalry_committed_log_is_ended(self._handle)

    def failed_at(self):
        """The position after the last frame the driver's replay of the member's recording
        played before it failed, or None for a log that is live, that ended because its
        member closed or left the recording, or that ended whole at the stop of a recording
        below the member's live one. No entry from that position on is delivered.
        The entries below it may still be on their way when this first is a position, so
        read it once `is_ended` is true. They have all been delivered by then unless the
        driver withdrew the replay first, which it does 5 s after a replay ends, one that
        failed included: `withdrawn_at()` is then a position below this one, and the
        entries from there on, those below this position among them, were not delivered
        either. What the log delivered ends at `position()`."""
        position = ctypes.c_int64()
        failed = self._abi.cavalry_committed_log_failed_at(self._handle, ctypes.byref(position))
        return position.value if failed else None

    def withdrawn_at(self):
        """The position at which the entries this log was not given begin, when the driver
        withdrew the replay before the polls had read it to its end, or None. The driver
        keeps a replay 5 s after it ends and then withdraws it, read or not: a service
        that did not poll for that long after its member closed or left the log, or whose
        handler took that long, ends so, and so does a log on a recording below the
        member's live one while the member's commit position stays below that recording's
        stop for that long. The entries from that position on were played and not
        delivered. A replay that failed is withdrawn in the same way: `failed_at()`
        is then a position too, and this one is the lower. Read it once `is_ended` is
        true."""
        position = ctypes.c_int64()
        withdrawn = self._abi.cavalry_committed_log_withdrawn_at(self._handle, ctypes.byref(position))
        return position.value if withdrawn else None

    def faulted(self):
        """The fault of the log's own image, the replay's, as (code, session_id, position),
        or None for a log whose image has not faulted. The code is
        `CAVALRY_ERR_FAULT_IMAGE` when a frame of the image failed a check, with the
        reader's position, or when the image was gone before this client read it, with
        `NULL_POSITION`; it is `CAVALRY_ERR_FAULT_IMAGES_FULL` when the image could not
        join. The poll that meets the fault returns -1 once, `fault()` answers it until
        the next poll, and this answers it from that poll on, for as long as the log is
        held: a service that did not keep the poll's answer reads it here, once
        `is_ended` is true, and tells this end from a whole one. No entry from the fault
        on was delivered. `withdrawn_at()` is None for such a log, and `failed_at()` is a
        position only when the driver's replay failed as well. From inside a
        handler of this log's own poll the ABI refuses the call with
        `CAVALRY_ERR_POLLING`, and this is None, a refusal and not an answer."""
        fault = _abi.cavalry_fault_t()
        if not self._abi.cavalry_committed_log_faulted(self._handle, ctypes.byref(fault)):
            return None
        return fault.code, fault.session_id, fault.position

    def close(self):
        """Stops the replay through the client and frees the handle; idempotent. After the
        client's own close it frees the handle alone, and the driver ends the replay by
        itself. The handle is freed whatever the ABI answers. When the ABI could not stop
        the replay, through a client that the driver or a timeout closed or within the
        response timeout, this raises `CavalryError` after the handle is freed: the log is
        closed, a second close does nothing, and the driver ends the replay by itself. The
        one exception is a close from inside this log's own handler, which the ABI refuses
        with `CAVALRY_ERR_POLLING`: that raises `CavalryError` too, and the log stays
        open."""
        if self._handle is None:
            return
        with self._client._lock:
            result = self._abi.cavalry_committed_log_close(self._handle, self._client._handle)
            error = None if result == 0 else _error(self._abi, "cavalry_committed_log_close")
        if error is None or error.code != _abi.CAVALRY_ERR_POLLING:
            self._handle = None
        if error is not None:
            raise error


class ClusterSession:
    """A client's session with a cluster, made by `Client.connect_cluster`
    (spec/cluster.md, "Sessions"). `offer(message)` sends the leader a message of one
    frame, at most `max_message_length` bytes, and returns the position after it on the
    session's publication to the leader, or a result code below zero, never raising for a
    code. `poll(handler, fragment_limit)` calls `handler(body, reply)` for each reply
    that is not stale, `body` a `memoryview` into the log valid for the call and `reply`
    the one `cavalry_cluster_reply_values_t` this object keeps, filled for each reply:
    `reply.leadership_term_id`, the term of the entry the reply answers, and
    `reply.timestamp`, the cluster's time when its leader appended that entry, in
    milliseconds since 1970-01-01T00:00:00Z. It returns the count of replies handed over,
    or the ABI's -1 when the poll stopped on a fault, which `fault()` reads until the
    next poll. The poll is the session's duty cycle too: it sends the keepalive, follows
    a new leader, and closes the session when the cluster says so, so call it at least
    every 500 ms. A message taken is on its way to the leader and not committed, and a
    reply is no delivery: a program that needs an answer offers again when none comes.
    `session_id` is the id the leader gave the session."""

    def __init__(self, abi, handle):
        self._abi = abi
        self._handle = handle
        self._handler = None
        self._pending = None
        self._interrupt = None
        self._raised = 0
        self._reply = _abi.cavalry_cluster_reply_values_t()
        self._reply_ref = ctypes.byref(self._reply)
        self._trampoline = _abi.cavalry_fragment_handler_t(self._on_reply)
        constants = _abi.cavalry_cluster_session_constants_t()
        if abi.cavalry_cluster_session_constants(handle, ctypes.byref(constants)) != 0:
            # The session is open and no object will hold it: its error is read first,
            # since the close sets the thread's own, and then the session is closed, as a
            # committed log that cannot be returned is.
            error = _error(abi, "cavalry_cluster_session_constants")
            abi.cavalry_cluster_session_close(handle)
            self._handle = None
            raise error
        self.session_id = constants.session_id
        self.max_message_length = constants.max_message_length

    def offer(self, message):
        """Offer the leader `message`, `bytes` or a `ctypes` `c_char` array, as a
        publication's `offer` takes them: the position after it, or a result code below
        zero. `ERROR` with `CAVALRY_ERR_TOO_LONG` as the thread's error is a message
        longer than `max_message_length`, and `CLOSED` a session that is closed, for the
        reason `closed()` gives, or whose client is. From inside this session's own
        handler the ABI refuses the call with `ERROR` and `CAVALRY_ERR_POLLING`: keep
        what the answer needs and offer after the poll has returned. One ABI call."""
        return self._abi.cavalry_cluster_session_offer(self._handle, message, len(message))

    def _on_reply(self, clientd, buffer, length, header):
        # As a subscription's trampoline: the body's view is released once the handler
        # returns. The reply's values are read from the header before the handler runs,
        # while the pointer is the ABI's, into the one struct this object keeps; the
        # handler is handed None in its place if the ABI refuses the header, which on
        # the handler's own it cannot.
        try:
            body = memoryview((ctypes.c_uint8 * length).from_address(buffer)).cast("B")
            try:
                refused = self._abi.cavalry_cluster_reply_values(header, self._reply_ref)
                self._handler(body, self._reply if refused == 0 else None)
            finally:
                body.release()
        except BaseException as exc:  # kept for poll to raise once the ABI has returned
            self._keep(exc)

    def _keep(self, exc):
        """Count a handler call that raised; keep the first `Exception` and the first
        exception that is not one, a `KeyboardInterrupt` or a `SystemExit`."""
        self._raised += 1
        if isinstance(exc, Exception):
            if self._pending is None:
                self._pending = exc
        elif self._interrupt is None:
            self._interrupt = exc

    def poll(self, handler, fragment_limit):
        """One ABI call, and its result: the count of replies handed over, or -1 when the
        poll stopped on a fault, which `fault()` reads until the next poll. A stale
        reply, one of a leadership term below that of a reply this session has handed
        over, is passed over and counted in `stale_replies()`, and is not in the count.
        Every reply the ABI hands over reaches the handler, whether or not an earlier
        call raised; the handler's exceptions are raised after the ABI call returns, one
        that is not an `Exception` as itself, else the first as `HandlerError`, as a
        subscription's poll raises them. Called from a handler of this session's own
        poll, it passes the call to the ABI, which refuses it with -1 and
        `CAVALRY_ERR_POLLING`, and touches nothing (spec/c-abi.md, "Calls during a claim
        or a poll")."""
        if self._handler is not None:
            return self._abi.cavalry_cluster_session_poll(self._handle, self._trampoline, None, fragment_limit)
        self._handler = handler
        try:
            replies = self._abi.cavalry_cluster_session_poll(self._handle, self._trampoline, None, fragment_limit)
        finally:
            self._handler = None
        if self._raised:
            pending, interrupt, raised = self._pending, self._interrupt, self._raised
            self._pending = self._interrupt = None
            self._raised = 0
            if interrupt is not None:
                raise interrupt
            raise HandlerError(replies, raised, pending, cluster="session")
        return replies

    def fault(self):
        """What the last poll stopped on: (code, session_id, position); code 0 when the last
        poll returned a count."""
        fault = _abi.cavalry_fault_t()
        _check(self._abi, "cavalry_cluster_session_fault",
               self._abi.cavalry_cluster_session_fault(self._handle, ctypes.byref(fault)))
        return fault.code, fault.session_id, fault.position

    def leadership_term_id(self):
        """The leadership term the session sends with: the term of the leader that opened
        it, or of the last new leader event. -1 from inside this session's own handler,
        which has each reply's own term."""
        return self._abi.cavalry_cluster_session_leadership_term_id(self._handle)

    def leader_member_id(self):
        """The id of the member the session sends to: the leader, as the cluster last
        said. -1 from inside this session's own handler."""
        return self._abi.cavalry_cluster_session_leader_member_id(self._handle)

    def stale_replies(self):
        """How many stale replies this session's polls have passed over: the value of its
        counter `cluster.replies_stale`. -1 from inside this session's own handler."""
        return self._abi.cavalry_cluster_session_stale_replies(self._handle)

    def closed(self):
        """Why the session is closed, or None while it is open: "TIMEOUT" when the leader
        closed it for hearing nothing from it for the session timeout, "NO LEADER" when
        its leader was lost and no member took its place, or the detail of the cluster's
        closed event. A poll is what closes a session, so the answer changes only across
        one. From inside this session's own handler the ABI refuses the call with
        `CAVALRY_ERR_POLLING`, and this is None, a refusal and not an answer; it is None
        too once `close()` has freed the session."""
        reason = self._abi.cavalry_cluster_session_closed(self._handle)
        return None if reason is None else reason.decode("utf-8", "replace")

    def close(self):
        """Offers the leader a session close for at most 500 ms, unless the session is
        closed already, then closes the session's publications, its subscription and its
        counter and frees the handle; idempotent. Close a session before its client. The
        handle is freed whatever the ABI answers, so when the ABI's close failed this
        raises `CavalryError` after the session is freed and a second close does nothing.
        The one exception is a close from inside this session's own handler, which the
        ABI refuses with `CAVALRY_ERR_POLLING`: that raises `CavalryError` too, sends
        nothing, and the session stays open."""
        if self._handle is None:
            return
        result = self._abi.cavalry_cluster_session_close(self._handle)
        error = None if result == 0 else _error(self._abi, "cavalry_cluster_session_close")
        if error is None or error.code != _abi.CAVALRY_ERR_POLLING:
            self._handle = None
        if error is not None:
            raise error


class ClusterService:
    """A cluster's service beside the member on the client's driver, made by
    `Client.cluster_service` (spec/cluster.md, "The service" and "Replies").
    `poll(handler, fragment_limit)` calls `handler(body, entry, service)` for each entry
    a quorum has committed, in order: `body` is a session message's payload, a
    `memoryview` into the log valid for the call and empty for every other entry;
    `entry` is the one `cavalry_cluster_entry_values_t` this object keeps, filled for
    each entry, with its `position`, `leadership_term_id`, `timestamp`, `session_id`,
    `type`, one of `CAVALRY_CLUSTER_ENTRY_*`, `leader_member_id` and `timed_out`; and
    `service` is this object, for `service.reply(session_id, message)`. What the handler
    returns says what becomes of the entry: None or `CONTINUE` takes it and the poll goes
    on, `BREAK` takes it and ends the poll, and `ABORT` leaves it, ends the poll, and has
    the next poll hand it over again. A handler that raises, or returns anything else,
    has its entry aborted, and `poll` raises `HandlerError` once the ABI has returned:
    an entry is taken only by a handler that finished with it. `poll` returns the count
    of entries taken, or the ABI's -1 when the poll stopped on a fault, which `fault()`
    reads until the next poll. `entry.timestamp` is the one clock a service reads, so
    that every member's service reaches the same state."""

    def __init__(self, abi, handle, client):
        self._abi = abi
        self._handle = handle
        self._client = client
        self._handler = None
        self._pending = None
        self._interrupt = None
        self._refused = None
        self._raised = 0
        self._entry = _abi.cavalry_cluster_entry_values_t()
        self._entry_ref = ctypes.byref(self._entry)
        self._trampoline = _abi.cavalry_fragment_handler_t(self._on_entry)

    def reply(self, session_id, message):
        """From inside the handler: send `message`, `bytes` or a `ctypes` `c_char` array,
        to the session `session_id`, with the leadership term id and the cluster's time
        of the entry being handled, and return what became of the reply, one of
        `CAVALRY_CLUSTER_REPLY_*`, never raising for a code. `_SENT`: it is on its way, or
        an earlier handling of the same entry sent it. `_NOT_ANSWERING`: this service
        does not answer the entry; its member did not lead the entry's term, or the entry
        is history. `_RETRY`: the session's response channel did not take it now; return
        `ABORT`, and the next poll hands the entry over again. `_DROPPED`: it is lost,
        and counted in `cluster.replies_dropped`. `_NO_SESSION`: no session with that id
        is open in the log at this entry. `_TOO_LONG`: it is longer than one frame
        carries, with `CAVALRY_ERR_TOO_LONG` as the thread's error. Reply first, then
        change the service's state, since an aborted entry is handed over again. From
        outside the handler it is -1, with `CAVALRY_ERR_CLUSTER_NO_ENTRY`. One ABI call,
        which allocates nothing and waits for nothing."""
        return self._abi.cavalry_cluster_service_reply(self._handle, session_id, message, len(message))

    def _on_entry(self, clientd, buffer, length, header):
        # As a committed log's trampoline: the body's view is released once the handler
        # returns, and the entry's values are read from the header before the handler
        # runs, while the pointer is the ABI's, into the one struct this object keeps;
        # the handler is handed None in its place if the ABI refuses the header, which
        # on the handler's own it cannot. What the handler returns is its action, passed
        # to the ABI unless it is CONTINUE, which is what the ABI does with no call. An
        # action the ABI refuses, and a handler that raised, abort the entry: the poll
        # ends here, with the entry not taken, and raises once the ABI has returned.
        try:
            body = memoryview((ctypes.c_uint8 * length).from_address(buffer)).cast("B")
            try:
                refused = self._abi.cavalry_cluster_entry_values(header, self._entry_ref)
                action = self._handler(body, self._entry if refused == 0 else None, self)
            finally:
                body.release()
            if action is None or action == CONTINUE:
                return
            if self._abi.cavalry_cluster_service_action(self._handle, action) != 0:
                self._refused = action
                self._raised += 1
                self._abi.cavalry_cluster_service_action(self._handle, ABORT)
        except BaseException as exc:  # kept for poll to raise once the ABI has returned
            self._keep(exc)
            self._abi.cavalry_cluster_service_action(self._handle, ABORT)

    def _keep(self, exc):
        """Count a handler call that raised; keep the first `Exception` and the first
        exception that is not one, a `KeyboardInterrupt` or a `SystemExit`."""
        self._raised += 1
        if isinstance(exc, Exception):
            if self._pending is None:
                self._pending = exc
        elif self._interrupt is None:
            self._interrupt = exc

    def poll(self, handler, fragment_limit):
        """One ABI call, and its result: the count of entries the handler took, 0 while
        the driver hosts no member that has a log yet, or -1 when the poll stopped on a
        fault, which `fault()` reads until the next poll. The poll is the service's duty
        cycle: it opens the member's log, follows the log's sessions, and holds a
        publication to each open session while the service answers. Those are commands
        sent with the service's client, at a session's opening, where a term's leader
        changes and at a recording's end, and the poll waits for the driver's answer to
        each with the GIL held, as it holds it for every entry; none is sent for a
        session message. The poll takes no lock, and it is a call on the client: no
        other thread uses the service's client (`Client.cluster_service`). When the
        handler raised, or returned what is no action, its entry was aborted and this
        raises after the ABI call returns: an exception that is not an `Exception` as
        itself, else `HandlerError`, whose `fragments` is the count of entries taken
        before that entry and whose `__cause__` is the handler's exception, or a
        `CavalryError` with `CAVALRY_ERR_CLUSTER_ACTION` for a value that is no action.
        The next poll hands the aborted entry over again. Called from a handler of this
        service's own poll, it passes the call to the ABI, which refuses it with -1 and
        `CAVALRY_ERR_POLLING`, and touches nothing (spec/c-abi.md, "Calls during a claim
        or a poll")."""
        client = self._client._handle
        if self._handler is not None:
            return self._abi.cavalry_cluster_service_poll(self._handle, client, self._trampoline, None, fragment_limit)
        self._handler = handler
        try:
            entries = self._abi.cavalry_cluster_service_poll(self._handle, client, self._trampoline, None, fragment_limit)
        finally:
            self._handler = None
        if self._raised:
            pending, interrupt, refused = self._pending, self._interrupt, self._refused
            self._pending = self._interrupt = self._refused = None
            self._raised = 0
            if interrupt is not None:
                raise interrupt
            if pending is None:
                pending = CavalryError(
                    "cavalry_cluster_service_action", _abi.CAVALRY_ERR_CLUSTER_ACTION,
                    f"the entry handler returned {refused!r}, which is no action: return nothing or cavalry.CONTINUE "
                    f"to take the entry and go on, cavalry.BREAK to take it and end the poll, or cavalry.ABORT to "
                    f"leave it for the next poll; see docs/reference/c-abi.md#constants")
            raise HandlerError(entries, 1, pending, cluster="service")
        return entries

    def fault(self):
        """What the last poll stopped on: (code, session_id, position); code 0 when the
        last poll returned a count. The code is a fault's, `CAVALRY_ERR_CLUSTER_ENTRY`
        with the position of a committed entry that is no entry of this version's cluster
        protocol, or the client's or the driver's."""
        fault = _abi.cavalry_fault_t()
        _check(self._abi, "cavalry_cluster_service_fault",
               self._abi.cavalry_cluster_service_fault(self._handle, ctypes.byref(fault)))
        return fault.code, fault.session_id, fault.position

    def position(self):
        """The position after the last entry the handler took, 0 before the first: how
        far the service has applied the log. -1 from inside this service's own handler,
        which has each entry's own position; the poll moves this one when it returns."""
        return self._abi.cavalry_cluster_service_position(self._handle)

    def answers(self):
        """Whether the service answered the last entry it read: its member led that
        entry's term and, as the entry was read, led it still, and the entry was not
        history. False from inside this service's own handler, a refusal and not an
        answer: there, what `reply` returns says whether the service answers the entry."""
        return self._abi.cavalry_cluster_service_answers(self._handle)

    def sessions(self):
        """How many sessions the log holds open, as far as the service has read: 10 at
        most. -1 from inside this service's own handler."""
        return self._abi.cavalry_cluster_service_sessions(self._handle)

    def close(self):
        """Closes the service's publications and its counter, stops its read of the log
        and frees the handle; idempotent. Close a service before its client; after the
        client's own close this frees the handle alone, and the driver ends what the
        service held by itself. The handle is freed whatever the ABI answers, so when the
        ABI's close failed this raises `CavalryError` after the service is freed and a
        second close does nothing. The one exception is a close from inside this
        service's own handler, or from inside the handler of another service whose poll
        holds the client, which the ABI refuses with `CAVALRY_ERR_POLLING`: that raises
        `CavalryError` too, and the service stays open."""
        if self._handle is None:
            return
        with self._client._lock:
            result = self._abi.cavalry_cluster_service_close(self._handle, self._client._handle)
            error = None if result == 0 else _error(self._abi, "cavalry_cluster_service_close")
        if error is None or error.code != _abi.CAVALRY_ERR_POLLING:
            self._handle = None
        if error is not None:
            raise error


def abi_version():
    return library().cavalry_abi_version()


def version():
    """The library's own version, as `cavalry_version_full` gives it."""
    return library().cavalry_version_full().decode()
'''

from generate_dotnet import write_dotnet  # noqa: E402
from generate_go import write_go  # noqa: E402
from generate_jvm import write_jvm  # noqa: E402
from generate_node import write_node  # noqa: E402

WRITERS = {"python": write_python, "node": write_node, "go": write_go, "jvm": write_jvm, "dotnet": write_dotnet}


def main(argv: list[str]) -> int:
    args = [a for a in argv[1:] if not a.startswith("--")]
    check = "--check" in argv
    if len(args) != 1 or args[0] not in WRITERS:
        sys.stderr.write(f"usage: generate.py <{'|'.join(WRITERS)}> [--check]\n")
        return 2
    language = args[0]
    with open(HEADER, encoding="utf-8") as f:
        header = parse(f.read())
    files = WRITERS[language](header)
    target = os.path.join(ROOT, "clients", LANGUAGES[language])
    if check:
        differ = []
        for name, content in files.items():
            path = os.path.join(target, name)
            try:
                with open(path, encoding="utf-8") as f:
                    committed = f.read()
            except FileNotFoundError:
                committed = None
            if committed != content:
                differ.append(name)
        if differ:
            sys.stderr.write(f"generate: clients/{LANGUAGES[language]} differs from what the header gives in "
                             f"{', '.join(differ)}; run `python3 clients/generate.py {language}` and commit the result"
                             f" (clients/README.md)\n")
            return 1
        print(f"generate: clients/{LANGUAGES[language]} is what the header gives ({len(files)} files)")
        return 0
    for name, content in files.items():
        path = os.path.join(target, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            f.write(content)
        print(f"generate: wrote clients/{LANGUAGES[language]}/{name}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
