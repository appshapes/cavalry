#!/usr/bin/env python3
"""The generator's tests (ADR 0026): its parse of the committed header equals
`spec/vectors/c-abi.json` each way, the committed Python package is what the parse gives,
a change to the header changes the package, `offer` takes bytes and refuses a `str`,
`load` refuses a library of another ABI version before it touches any other function,
`Header.values()` returns the one frame struct it keeps, `HandlerError` says what to do and
names a heading of the README, and `--check` fails on a package that differs. The same
for the Node package (`NodePackage`): the committed five files are what the parse gives,
a header change changes the addon and the client, the addon wraps every function but the
poll, which is its own `poll` around the one `cavalry_subscription_poll` call, the struct
layout the client reads its `DataView`s by is the layout `ctypes` gives the Python
binding, the trampoline, compiled with `cc` against a stand-in for N-API, counts and
reports every fragment the handler did not see, `HandlerError`'s message and the addon's
Error for such a fragment say what to do and name a heading of the README, and `--check`
fails on a package that differs. `just test` runs this through `test-scripts`. Python 3.9
standard library only. The same for the Go module (`GoPackage`): the committed six
generated files are what the parse gives, a header change changes the constants, every
define is a constant under its Go name with the ABI's name beside it, the shim installs
the exported trampoline around the one poll call, each per-message call keeps its error
in the C call that failed, every call that reads the thread's error holds its OS thread,
every call on the client handle holds the client's mutex, the package tests its
subscription table at the bound and `run.sh` runs that test, `HandlerError`'s message
says what to do and names a heading of the README, the generated files are gofmt's own
and vet clean when `go` is on the path, and `--check` fails on a module that differs. The same for the JVM package (`JvmPackage`): the committed eleven
files are what the parse gives, a header change changes the constants, every define is a
constant under its Java name with the ABI's name beside it, the binding reads
`cavalry_abi_version` before it binds any other function and refuses a header without the
check's names, the offer's handle alone is critical and each subscription makes one upcall
stub bound to its own `onFragment`, a publication's close zeroes the claim struct before
the ABI frees the handle the struct names, a claim's segment is one slice of a private
segment over the address space, `HandlerException`'s message says what to do and names
a heading of the README, the generated files compile under Java 25 with every lint an error
when a `javac` of that release is on the path, and `--check` fails on a package that
differs. The same for the .NET package (`DotnetPackage`): the committed
twelve files are what the parse gives, a header change changes the constants, every define
is a constant under its name with the ABI's name beside it, the binding resolves the library
and reads `cavalry_abi_version` before any other function and refuses a header without the
check's names, every function is a `LibraryImport` with blittable parameters and the one
trampoline is `UnmanagedCallersOnly` whose address the poll passes, `HandlerException`'s
message says what to do and names a heading of the README, the generated project builds
with every warning an error when a .NET 10 SDK is on the path, and `--check` fails on a
package that differs.
"""

import ctypes
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import generate as gen  # noqa: E402
import generate_dotnet as gen_dotnet  # noqa: E402
import generate_go as gen_go  # noqa: E402
import generate_jvm as gen_jvm  # noqa: E402
import generate_node as gen_node  # noqa: E402
import lint  # noqa: E402

VECTORS = os.path.join(gen.ROOT, "spec", "vectors", "c-abi.json")


def load_package():
    """The committed package, imported as `cavalry` from clients/python; nothing loads the
    library until a test asks."""
    sys.path.insert(0, os.path.join(gen.ROOT, "clients", "python"))
    try:
        import cavalry
    finally:
        sys.path.pop(0)
    return cavalry


def flat(c: str) -> str:
    """A C type or signature with one spacing, so that `void *clientd` and `void * clientd`
    compare equal."""
    return re.sub(r"\s*\*\s*", "*", " ".join(c.split()))


class Parse(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with open(gen.HEADER, encoding="utf-8") as f:
            cls.text = f.read()
        cls.header = gen.parse(cls.text)
        with open(VECTORS, encoding="utf-8") as f:
            cls.vectors = json.load(f)

    def test_the_opaque_types_are_the_vectors(self):
        self.assertEqual(sorted(self.header.opaque), sorted(self.vectors["opaque_types"]))

    def test_the_structs_and_their_fields_are_the_vectors(self):
        parsed = {s.name: [(flat(f.type), f.name) for f in s.fields] for s in self.header.structs}
        expected = {name: [(flat(t), n) for t, n in fields] for name, fields in self.vectors["structs"].items()}
        self.assertEqual(parsed, expected)

    def test_the_fragment_handler_is_the_vectors_typedef(self):
        [cb] = self.header.callbacks
        signature = f"{cb.ret} (*)({', '.join(p.type + ' ' + p.name for p in cb.params)})"
        self.assertEqual(flat(signature), flat(self.vectors["typedefs"][cb.name]))
        self.assertEqual(len(self.header.callbacks), len(self.vectors["typedefs"]))

    def test_the_macros_are_the_vectors(self):
        self.assertEqual(sorted(self.header.macros), sorted(self.vectors["macros"]))

    def test_the_functions_are_the_vectors(self):
        parsed = {fn.name: (flat(fn.ret), [(flat(p.type), p.name) for p in fn.params]) for fn in self.header.functions}
        expected = {fn["name"]: (flat(fn["returns"]), [(flat(t), n) for t, n in fn["params"]])
                    for fn in self.vectors["functions"]}
        self.assertEqual(parsed, expected)
        self.assertEqual(len(self.header.functions), 83)

    def test_the_defines_are_the_vectors(self):
        parsed = {name: gen.python_value(value) for name, value in self.header.defines if name != "CAVALRY_H"}
        self.assertEqual(sorted(parsed), sorted(self.vectors["defines"]))
        for name, value in self.vectors["defines"].items():
            got = eval(parsed[name], {"__builtins__": {}})  # the generator's own text, numbers and one string
            self.assertEqual(got, value, name)


class Package(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with open(gen.HEADER, encoding="utf-8") as f:
            cls.text = f.read()
        cls.header = gen.parse(cls.text)

    def committed(self, name):
        with open(os.path.join(gen.ROOT, "clients", "python", name), encoding="utf-8") as f:
            return f.read()

    def test_the_committed_package_is_what_the_header_gives(self):
        files = gen.write_python(self.header)
        self.assertEqual(sorted(files), ["cavalry/__init__.py", "cavalry/_abi.py"])
        for name, content in files.items():
            self.assertEqual(self.committed(name), content, name)

    def test_a_header_change_changes_the_binding(self):
        changed = gen.parse(self.text.replace("#define CAVALRY_MAX_IMAGES 256", "#define CAVALRY_MAX_IMAGES 512"))
        files = gen.write_python(changed)
        self.assertIn("CAVALRY_MAX_IMAGES = 512", files["cavalry/_abi.py"])
        self.assertNotEqual(files["cavalry/_abi.py"], self.committed("cavalry/_abi.py"))

    def test_the_binding_declares_every_function_with_its_types(self):
        abi = gen.write_python(self.header)["cavalry/_abi.py"]
        for fn in self.header.functions:
            self.assertIn(f'"{fn.name}": (', abi)
        self.assertIn("cavalry_fragment_handler_t = CFUNCTYPE(None, c_void_p, c_void_p, c_size_t, c_void_p)", abi)

    def test_the_per_message_functions_are_the_allocation_tests_and_load_through_pydll(self):
        """The twelve functions `crates/cavalry-ffi/tests/allocations.rs` counts are the ones
        the binding keeps the GIL held across; every other function releases it, so an add
        that waits on the driver does not stop the process's other threads."""
        with open(os.path.join(gen.ROOT, "crates", "cavalry-ffi", "tests", "allocations.rs"), encoding="utf-8") as f:
            doc = "".join(line for line in f if line.startswith("//!"))
        self.assertEqual(sorted(gen.PER_MESSAGE), sorted(set(re.findall(r"`(cavalry_\w+)`", doc))))
        abi = gen.write_python(self.header)["cavalry/_abi.py"]
        self.assertIn("    library = ctypes.CDLL(path)\n", abi)
        self.assertIn("    held = ctypes.PyDLL(path)\n", abi)
        self.assertIn("function = getattr(held if name in PER_MESSAGE else library, name)", abi)
        for name in gen.PER_MESSAGE:
            self.assertIn(f'    "{name}",\n', abi.split("PER_MESSAGE = (", 1)[1].split(")", 1)[0])

    def test_a_publications_close_zeroes_the_claim_struct_before_the_abis_close(self):
        """The claim struct's `publication` field names the handle the close frees, and the
        ABI reads it on a commit, an abort or a reserved value, so `close` zeroes the struct
        before the ABI's close (spec/c-abi.md, "Which claim a struct names"); the runner's
        refusals round makes the three calls after a close, and this check fails without the
        zeroing every time, where a read of the freed handle may answer NO_CLAIM by luck."""
        code = gen.write_python(self.header)["cavalry/__init__.py"]
        publication = code.split("class Publication:", 1)[1].split("\nclass ", 1)[0]
        close = publication.split("    def close(self):\n", 1)[1]
        zeroed = close.index("ctypes.memset(ctypes.addressof(self._claim), 0, ctypes.sizeof(self._claim))")
        freed = close.index("self._abi.cavalry_publication_close(self._handle)")
        self.assertLess(zeroed, freed)
        self.assertLess(close.index("self._release()"), zeroed)

    def test_the_committed_log_reads_the_position_in_its_trampoline_and_keeps_a_refused_close_open(self):
        """The Python committed log (ADR 0070): the trampoline asks the header for the
        entry's position while the pointer is the ABI's, before the handler runs; the open
        returns None for the null handle of a driver with no member; and `close` drops its
        handle only when the ABI freed it, which a call refused from inside the log's own
        handler did not (spec/c-abi.md, "Calls during a claim or a poll")."""
        code = gen.write_python(self.header)["cavalry/__init__.py"]
        log = code.split("class CommittedLog:", 1)[1].split("\ndef abi_version", 1)[0]
        trampoline = log.split("    def _on_fragment(self, clientd, buffer, length, header):\n", 1)[1].split("    def _keep", 1)[0]
        self.assertIn("self._handler(body, self._header, self._abi.cavalry_committed_log_entry_position(header))", trampoline)
        self.assertLess(trampoline.index("self._header._pointer = header"),
                        trampoline.index("cavalry_committed_log_entry_position(header)"))
        self.assertEqual(log.count("self._abi.cavalry_committed_log_poll("), 2)  # the nested refusal and the poll itself
        close = log.split("    def close(self):\n", 1)[1]
        self.assertIn("if error is None or error.code != _abi.CAVALRY_ERR_POLLING:\n            self._handle = None", close)
        # Both reports of a short end, each None unless the ABI answered true.
        for report in ("failed_at", "withdrawn_at"):
            body = log.split(f"    def {report}(self):\n", 1)[1].split("\n    def ", 1)[0]
            self.assertIn(f"self._abi.cavalry_committed_log_{report}(self._handle, ctypes.byref(position))", body)
            self.assertIn("return position.value if ", body)
        # The third report, the lasting read of the image's fault (ADR 0080): the three
        # fields when the ABI answered true, None when it answered false.
        faulted = log.split("    def faulted(self):\n", 1)[1].split("\n    def ", 1)[0]
        self.assertIn("if not self._abi.cavalry_committed_log_faulted(self._handle, ctypes.byref(fault)):\n"
                      "            return None\n"
                      "        return fault.code, fault.session_id, fault.position", faulted)
        # A log whose constants cannot be read is closed, as the Go client closes one it
        # cannot return, after its error is read and before the error is raised.
        opened = log.split("    def __init__(self, abi, handle, client):\n", 1)[1].split("\n    def ", 1)[0]
        read = opened.index('error = _error(abi, "cavalry_committed_log_constants")')
        closed = opened.index("abi.cavalry_committed_log_close(handle, client._handle)")
        self.assertLess(opened.index("abi.cavalry_committed_log_constants(handle, ctypes.byref(constants)) != 0"), read)
        self.assertLess(read, closed)
        self.assertLess(closed, opened.index("raise error"))
        client = code.split("class Client:", 1)[1].split("\nclass ", 1)[0]
        self.assertIn("return CommittedLog(self._abi, handle.value, self) if handle.value else None", client)
        for name in ("cavalry_committed_log_poll", "cavalry_committed_log_entry_position"):
            self.assertIn(name, gen.PER_MESSAGE)

    def test_offer_takes_bytes_and_refuses_a_str(self):
        """`c_void_p` took a `str` as its `wchar_t` buffer, so `offer("hello")` published
        `h\\0\\0\\0e`; the binding's `c_char_p` refuses it, and every other non-bytes form,
        before the ABI is called. The fragment handler's buffer stays an address."""
        abi = gen.write_python(self.header)["cavalry/_abi.py"]
        self.assertIn('"cavalry_publication_offer": (c_int64, [c_void_p, c_char_p, c_size_t]),', abi)
        _abi = load_package()._abi
        buffer = _abi.FUNCTIONS["cavalry_publication_offer"][1][1]
        self.assertIs(buffer, ctypes.c_char_p)
        for refused in ("hello", bytearray(b"hello"), memoryview(b"hello"), 5):
            with self.assertRaises(TypeError, msg=type(refused).__name__):
                buffer.from_param(refused)
        buffer.from_param(b"hello")
        buffer.from_param(ctypes.create_string_buffer(5))

    def test_a_header_without_what_the_version_check_reads_fails_to_generate(self):
        for removed, name in (("#define CAVALRY_ERR_ABI_VERSION 135\n", "CAVALRY_ERR_ABI_VERSION"),
                              ("int cavalry_abi_version(void);\n", "cavalry_abi_version")):
            self.assertIn(removed, self.text)
            with self.assertRaises(SystemExit) as raised:
                gen.write_python(gen.parse(self.text.replace(removed, "")))
            self.assertIn(f"{name} is not in the header", str(raised.exception))

    def test_load_checks_the_abi_version_before_any_other_function(self):
        """spec/c-abi.md, "Versions and what a reader rejects": a generated client calls
        `cavalry_abi_version()` when it loads the library, before any other function. A
        library of another version is refused with CavalryError naming both numbers, and
        nothing else of it is touched; a library of the package's version is bound whole,
        `cavalry_abi_version` first."""
        _abi = load_package()._abi

        class Library:
            def __init__(self, version):
                self.touched = []
                self.version = version

            def __getattr__(self, name):
                self.touched.append(name)
                function = (lambda: self.version) if name == "cavalry_abi_version" else (lambda *args: 0)
                setattr(self, name, function)
                return function

        package = _abi.CAVALRY_ABI_VERSION
        for found in (package + 1, package - 1):
            library = Library(found)
            with mock.patch.object(ctypes, "CDLL", lambda path: library), \
                    mock.patch.object(ctypes, "PyDLL", lambda path: self.fail("PyDLL opened before the check")):
                with self.assertRaises(sys.modules["cavalry"].CavalryError) as raised:
                    _abi.load("libcavalry_ffi.so")
            self.assertEqual(library.touched, ["cavalry_abi_version"])
            self.assertEqual(raised.exception.code, _abi.CAVALRY_ERR_ABI_VERSION)
            self.assertEqual(raised.exception.function, "cavalry_abi_version")
            self.assertIn(f"is ABI version {found} and", raised.exception.message)
            self.assertIn(f"from a header of ABI version {package};", raised.exception.message)
            self.assertIn("python3 clients/generate.py python", raised.exception.message)
            self.assertIn("docs/reference/c-abi.md#versions-and-what-a-reader-rejects", raised.exception.message)
        library, held = Library(package), Library(package)
        with mock.patch.object(ctypes, "CDLL", lambda path: library), mock.patch.object(ctypes, "PyDLL", lambda path: held):
            self.assertIs(_abi.load("libcavalry_ffi.so"), library)
        self.assertEqual(library.touched[0], "cavalry_abi_version")
        self.assertEqual(sorted(library.touched + held.touched), sorted(_abi.FUNCTIONS))
        self.assertEqual(sorted(held.touched), sorted(_abi.PER_MESSAGE))

    def test_header_values_returns_the_one_frame_struct_it_keeps(self):
        """ctypes builds a new object on every read of a struct field, so `values()` keeps
        one over its struct's memory: every call returns it, and it reads what the latest
        `cavalry_header_values` wrote."""
        cavalry = load_package()

        class Abi:
            session_id = 0

            def cavalry_header_values(self, pointer, values):
                if pointer is None:
                    return -1
                self.session_id += 1
                values._obj.frame.session_id = self.session_id  # the struct `byref` points at
                return 0

        header = cavalry.Header(Abi())
        header._pointer = 1
        frame = header.values()
        self.assertIs(header.values(), frame)
        self.assertEqual(frame.session_id, 2)
        self.assertEqual(header.session_id, 3)
        self.assertEqual(frame.session_id, 3)
        header._pointer = None
        self.assertIsNone(header.values())

    def test_handler_errors_message_says_what_to_do_and_names_a_heading_of_the_readme(self):
        """CLAUDE.md: every error message says what to do next and names the docs anchor
        that explains it. The anchor `HandlerError` names is a heading of the README."""
        cavalry = load_package()
        cause = ValueError("message 3")
        for fragments, delivered in ((10, "raised on 1 of the 10 fragment(s) this poll delivered;"),
                                     (-1, "before it stopped on a fault, which Subscription.fault() reads;")):
            message = str(cavalry.HandlerError(fragments, 1, cause))
            self.assertIn(delivered, message)
            self.assertIn(f"Fix the handler: its first exception, {cause!r}, is this error's __cause__", message)
            self.assertTrue(message.endswith("See clients/python/README.md#the-exception"), message)
        # A committed log's poll names its own fault() and its entries: the fault of the
        # log's image is reported once, so the message must not send the reader to a
        # subscription's (clients/python/README.md, "The committed log").
        for entries, delivered in ((1, "the entry handler raised on 1 of the 1 entry this poll delivered;"),
                                   (3, "the entry handler raised on 1 of the 3 entries this poll delivered;"),
                                   (-1, "before it stopped on a fault, which this CommittedLog's fault() reads until "
                                        "its next poll, and its faulted() from then on when the fault is the log's "
                                        "own image's;")):
            message = str(cavalry.HandlerError(entries, 1, cause, committed_log=True))
            self.assertIn(delivered, message)
            self.assertNotIn("Subscription", message)
            self.assertNotIn("fragment", message)
            self.assertIn(f"Fix the handler: its first exception, {cause!r}, is this error's __cause__", message)
            self.assertTrue(message.endswith("See clients/python/README.md#the-exception"), message)
        code = gen.write_python(self.header)["cavalry/__init__.py"]
        log = code.split("class CommittedLog:", 1)[1].split("\ndef abi_version", 1)[0]
        self.assertIn("raise HandlerError(entries, raised, pending, committed_log=True)", log)
        self.assertIn("raise HandlerError(fragments, raised, pending)", code.split("class Subscription:", 1)[1])
        with open(os.path.join(gen.ROOT, "clients", "python", "README.md"), encoding="utf-8") as f:
            slugs = {re.sub(r"[^a-z0-9 -]", "", line.lstrip("#").strip().lower()).replace(" ", "-")
                     for line in f if line.startswith("#")}
        self.assertIn("the-exception", slugs)

    def test_check_fails_on_a_package_that_differs_and_says_what_to_do(self):
        with tempfile.TemporaryDirectory() as tmp:
            shutil.copytree(os.path.join(gen.ROOT, "clients", "python"), os.path.join(tmp, "clients", "python"))
            os.makedirs(os.path.join(tmp, "crates", "cavalry-ffi", "include"))
            shutil.copy(gen.HEADER, os.path.join(tmp, "crates", "cavalry-ffi", "include", "cavalry.h"))
            root, header = gen.ROOT, gen.HEADER
            gen.ROOT = tmp
            gen.HEADER = os.path.join(tmp, "crates", "cavalry-ffi", "include", "cavalry.h")
            try:
                self.assertEqual(gen.main(["generate.py", "python", "--check"]), 0)
                with open(os.path.join(tmp, "clients", "python", "cavalry", "_abi.py"), "a", encoding="utf-8") as f:
                    f.write("# edited by hand\n")
                import io
                import contextlib
                err = io.StringIO()
                with contextlib.redirect_stderr(err):
                    self.assertEqual(gen.main(["generate.py", "python", "--check"]), 1)
                self.assertIn("differs from what the header gives in cavalry/_abi.py", err.getvalue())
                self.assertIn("python3 clients/generate.py python", err.getvalue())
            finally:
                gen.ROOT, gen.HEADER = root, header


# The generated trampoline and its helpers, from N-API's status names to the trampoline's
# closing brace, compiled against a stand-in for the N-API calls they make: each call is
# recorded by name, and the one at index `fail_at` returns napi_generic_failure. `main` runs
# one fragment with no failure to learn its calls, then the same fragment with each of them
# failing in turn, for a handler that returns and one that throws, and checks what the
# state kept, and after a fragment the handler never saw, what the next one of the same poll
# does; it prints each finding and the clean run's calls, and exits 1 on a finding.
TRAMPOLINE_HARNESS = r'''#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "cavalry.h"

typedef struct napi_env__ *napi_env;
typedef struct napi_value__ *napi_value;
typedef struct napi_ref__ *napi_ref;
typedef struct napi_handle_scope__ *napi_handle_scope;
typedef enum { napi_ok = 0, napi_generic_failure = 9, napi_pending_exception = 10 } napi_status;
typedef enum { napi_uint8_array = 1 } napi_typedarray_type;
typedef void (*napi_finalize)(napi_env env, void *data, void *hint);

#define NAPI_AUTO_LENGTH SIZE_MAX

static const char *calls[64];
static int ncalls, fail_at = -1;
static const napi_status fail_with = napi_generic_failure;
static bool handler_throws, pending;
static int handler_ran, scopes_open, buffers_live;
static size_t handler_argc;
static int64_t position_made;
static char handles[12];
#define HANDLE(i) ((void *) &handles[i])
#define NULL_VALUE HANDLE(9)
#define TUPLE HANDLE(10)
#define ERROR_VALUE HANDLE(11)

/* What report and failure_error make: each number a value of its own that reads back, the
 * text of the last string, the named properties set and the tuple's elements. */
static int64_t numbers[64];
static int nnumbers;
static char text_made[1024];
static const char *named[16];
static napi_value named_values[16];
static int nnamed;
static napi_value tuple_elements[8];

static napi_value number(int64_t n) {
  int i = nnumbers < 64 ? nnumbers++ : 63;
  numbers[i] = n;
  return (napi_value) &numbers[i];
}
static bool is_number(napi_value v) {
  return (uintptr_t) v >= (uintptr_t) &numbers[0] && (uintptr_t) v < (uintptr_t) &numbers[64];
}
static napi_value property(const char *name) {
  for (int i = 0; i < nnamed; i++) if (strcmp(named[i], name) == 0) return named_values[i];
  return NULL;
}

static napi_status step(const char *name) {
  int at = ncalls;
  if (ncalls < 64) calls[ncalls++] = name;
  return at == fail_at ? fail_with : napi_ok;
}

napi_status napi_open_handle_scope(napi_env env, napi_handle_scope *scope) {
  (void) env;
  napi_status s = step("napi_open_handle_scope");
  if (s == napi_ok) { *scope = HANDLE(0); scopes_open++; }
  return s;
}
napi_status napi_close_handle_scope(napi_env env, napi_handle_scope scope) {
  (void) env; (void) scope;
  scopes_open--;
  return step("napi_close_handle_scope");
}
napi_status napi_create_external_buffer(napi_env env, size_t length, void *data, napi_finalize cb, void *hint,
                                        napi_value *result) {
  (void) env; (void) length; (void) data; (void) cb; (void) hint;
  napi_status s = step("napi_create_external_buffer");
  if (s == napi_ok) { *result = HANDLE(1); buffers_live++; }
  return s;
}
napi_status napi_get_reference_value(napi_env env, napi_ref ref, napi_value *result) {
  (void) env; (void) ref;
  napi_status s = step("napi_get_reference_value");
  if (s == napi_ok) *result = HANDLE(2);
  return s;
}
napi_status napi_create_reference(napi_env env, napi_value value, uint32_t count, napi_ref *result) {
  (void) env; (void) value; (void) count;
  napi_status s = step("napi_create_reference");
  if (s == napi_ok) *result = HANDLE(3);
  return s;
}
napi_status napi_set_element(napi_env env, napi_value object, uint32_t index, napi_value value) {
  (void) env;
  if (object == TUPLE && index < 8) tuple_elements[index] = value;
  return step("napi_set_element");
}
napi_status napi_get_element(napi_env env, napi_value object, uint32_t index, napi_value *result) {
  (void) env; (void) object; (void) index;
  napi_status s = step("napi_get_element");
  if (s == napi_ok) *result = HANDLE(6);
  return s;
}
napi_status napi_create_array_with_length(napi_env env, size_t length, napi_value *result) {
  (void) env; (void) length;
  napi_status s = step("napi_create_array_with_length");
  if (s == napi_ok) *result = TUPLE;
  return s;
}
napi_status napi_create_string_utf8(napi_env env, const char *str, size_t length, napi_value *result) {
  (void) env; (void) length;
  napi_status s = step("napi_create_string_utf8");
  if (s == napi_ok) { snprintf(text_made, sizeof text_made, "%s", str); *result = HANDLE(3); }
  return s;
}
napi_status napi_create_error(napi_env env, napi_value code, napi_value msg, napi_value *result) {
  (void) env; (void) code; (void) msg;
  napi_status s = step("napi_create_error");
  if (s == napi_ok) *result = ERROR_VALUE;
  return s;
}
napi_status napi_set_named_property(napi_env env, napi_value object, const char *utf8name, napi_value value) {
  (void) env;
  if (object == ERROR_VALUE && nnamed < 16) { named[nnamed] = utf8name; named_values[nnamed++] = value; }
  return step("napi_set_named_property");
}
napi_status napi_create_int32(napi_env env, int32_t value, napi_value *result) {
  (void) env;
  napi_status s = step("napi_create_int32");
  if (s == napi_ok) *result = number(value);
  return s;
}
napi_status napi_create_bigint_int64(napi_env env, int64_t value, napi_value *result) {
  (void) env;
  napi_status s = step("napi_create_bigint_int64");
  if (s == napi_ok) *result = number(value);
  return s;
}
napi_status napi_get_boolean(napi_env env, bool value, napi_value *result) {
  (void) env; (void) value;
  napi_status s = step("napi_get_boolean");
  if (s == napi_ok) *result = HANDLE(4);
  return s;
}
napi_status napi_get_null(napi_env env, napi_value *result) {
  (void) env;
  napi_status s = step("napi_get_null");
  if (s == napi_ok) *result = NULL_VALUE;
  return s;
}
napi_status napi_create_external(napi_env env, void *data, napi_finalize cb, void *hint, napi_value *result) {
  (void) env; (void) data; (void) cb; (void) hint;
  napi_status s = step("napi_create_external");
  if (s == napi_ok) *result = HANDLE(3);
  return s;
}
napi_status napi_get_undefined(napi_env env, napi_value *result) {
  (void) env;
  napi_status s = step("napi_get_undefined");
  if (s == napi_ok) *result = HANDLE(4);
  return s;
}
napi_status napi_call_function(napi_env env, napi_value recv, napi_value func, size_t argc, const napi_value *argv,
                               napi_value *result) {
  (void) env; (void) recv; (void) func; (void) argv;
  napi_status s = step("napi_call_function");
  if (s != napi_ok) return s;
  handler_ran++;
  handler_argc = argc;
  if (handler_throws) { pending = true; return napi_pending_exception; }
  *result = HANDLE(5);
  return napi_ok;
}
napi_status napi_is_exception_pending(napi_env env, bool *result) {
  (void) env;
  napi_status s = step("napi_is_exception_pending");
  if (s == napi_ok) *result = pending;
  return s;
}
napi_status napi_get_and_clear_last_exception(napi_env env, napi_value *result) {
  (void) env;
  napi_status s = step("napi_get_and_clear_last_exception");
  if (s == napi_ok) { *result = HANDLE(6); pending = false; }
  return s;
}
napi_status napi_get_typedarray_info(napi_env env, napi_value array, napi_typedarray_type *type, size_t *length,
                                     void **data, napi_value *arraybuffer, size_t *offset) {
  (void) env; (void) array; (void) type; (void) length; (void) data; (void) offset;
  napi_status s = step("napi_get_typedarray_info");
  if (s == napi_ok) *arraybuffer = HANDLE(7);
  return s;
}
napi_status napi_detach_arraybuffer(napi_env env, napi_value arraybuffer) {
  (void) env; (void) arraybuffer;
  napi_status s = step("napi_detach_arraybuffer");
  if (s == napi_ok) buffers_live--;
  return s;
}
napi_status napi_create_int64(napi_env env, int64_t value, napi_value *result) {
  (void) env;
  napi_status s = step("napi_create_int64");
  if (s == napi_ok) { *result = number(value); position_made = value; }
  return s;
}
/* The position the ABI gives for the header: 960 for the frame every run hands over, 1056 for
 * the later entry of the same poll that the run after a kept failure hands over. */
int64_t cavalry_committed_log_entry_position(const cavalry_header_t *header) {
  return header == (const cavalry_header_t *) HANDLE(1) ? 960 : 1056;
}
int cavalry_header_values(const cavalry_header_t *header, cavalry_header_values_t *values) {
  (void) header;
  memset(values, 0, sizeof *values);
  values->frame.session_id = 7;
  values->frame.term_offset = 4096;
  return 0;
}

@TRAMPOLINE@

static int failures;
#define EXPECT(cond, ...) do { if (!(cond)) { failures++; printf(__VA_ARGS__); printf("\n"); } } while (0)

static void reset(int at, bool throws) {
  ncalls = 0; fail_at = at; handler_throws = throws; pending = false;
  handler_ran = 0; scopes_open = 0; buffers_live = 0; handler_argc = 0; position_made = -1;
  nnumbers = 0; nnamed = 0; text_made[0] = '\0'; memset(tuple_elements, 0, sizeof tuple_elements);
}

static void fragment(subscription_state_t *s) {
  trampoline(s, (const uint8_t *) "body", 4, (const cavalry_header_t *) HANDLE(1));
}

static void one(int at, bool throws, bool positions) {
  subscription_state_t s;
  memset(&s, 0, sizeof s);
  s.env = (napi_env) HANDLE(0);
  s.positions = positions;
  reset(at, throws);
  fragment(&s);
  const char *failed = at < ncalls ? calls[at] : NULL;
  char where[200];
  snprintf(where, sizeof where, "%s, with %s failing (call %d), the handler %s",
           positions ? "a committed log's entry" : "a subscription's fragment", failed ? failed : "no call", at,
           throws ? "throwing" : "returning");
  /* A committed log's handler is handed three arguments, the third the position the ABI gave
   * for the header; a subscription's two, with no position asked for. */
  if (handler_ran) {
    EXPECT(handler_argc == (positions ? 3u : 2u), "%s: the handler was handed %u argument(s)", where,
           (unsigned) handler_argc);
    EXPECT(position_made == (positions ? 960 : -1), "%s: the position made was %lld", where, (long long) position_made);
  }
  if (handler_ran && !throws) {
    EXPECT(s.raised == 0 && s.undelivered == 0 && s.first == FIRST_NONE,
           "%s: the handler returned, yet the fragment was counted (raised %u, undelivered %u)", where,
           (unsigned) s.raised, (unsigned) s.undelivered);
  } else {
    EXPECT(s.raised + s.undelivered == 1, "%s: the fragment was counted %u time(s) (raised %u, undelivered %u), not once",
           where, (unsigned) (s.raised + s.undelivered), (unsigned) s.raised, (unsigned) s.undelivered);
    EXPECT(s.first != FIRST_NONE, "%s: nothing kept for poll to report", where);
    if (!handler_ran) EXPECT(s.undelivered == 1, "%s: the handler never ran, yet undelivered is %u", where, (unsigned) s.undelivered);
  }
  if (s.first == FIRST_FAILURE) {
    EXPECT(failed != NULL && s.failed != NULL && strstr(s.failed, failed) != NULL,
           "%s: the failure kept says \"%s\", not the call that failed", where, s.failed ? s.failed : "(nothing)");
    EXPECT(s.failed_status == (int) fail_with, "%s: the failure kept has status %d, not %d", where, s.failed_status,
           (int) fail_with);
    EXPECT(s.failed_frame && s.failed_session_id == 7 && s.failed_term_offset == 4096,
           "%s: the failure kept does not name the frame's session and term offset", where);
    /* A committed log's failure names its entry by the position the ABI gave. */
    if (positions) EXPECT(s.failed_position == 960, "%s: the failure kept names position %lld, not 960", where,
                          (long long) s.failed_position);
  }
  /* An entry the handler never saw is where a service opens the log again: the position the
   * ABI gave for it is kept. */
  if (positions && s.undelivered != 0) {
    EXPECT(s.undelivered_at == 960, "%s: the first undelivered entry is kept at position %lld, not 960", where,
           (long long) s.undelivered_at);
  }
  EXPECT(scopes_open == 0, "%s: %d handle scope(s) left open", where, scopes_open);
  if (failed == NULL || (strcmp(failed, "napi_get_typedarray_info") != 0 && strcmp(failed, "napi_detach_arraybuffer") != 0)) {
    EXPECT(buffers_live == 0, "%s: %d body Buffer(s) left attached", where, buffers_live);
  }
  /* A committed log hands its handler no entry of the poll after one the handler never saw,
   * so that a service never applies an entry after a gap: the next entry of the same poll
   * makes no N-API call, does not reach the handler, and is counted as undelivered, with the
   * first entry's position and failure still the ones kept. A subscription's next fragment
   * still reaches the handler. */
  if (s.undelivered != 0) {
    uint32_t undelivered = s.undelivered;
    const char *kept = s.failed;
    reset(-1, false);
    trampoline(&s, (const uint8_t *) "next", 4, (const cavalry_header_t *) HANDLE(2));
    if (positions) {
      EXPECT(handler_ran == 0 && ncalls == 0, "%s, then a later entry of the same poll: the handler ran %d time(s) "
             "and %d N-API call(s) were made", where, handler_ran, ncalls);
      EXPECT(s.undelivered == undelivered + 1, "%s, then a later entry of the same poll: undelivered is %u, not %u",
             where, (unsigned) s.undelivered, (unsigned) (undelivered + 1));
      EXPECT(s.undelivered_at == 960, "%s, then a later entry at 1056: the first undelivered entry is kept at %lld",
             where, (long long) s.undelivered_at);
      EXPECT(s.failed == kept, "%s, then a later entry of the same poll: the failure kept changed", where);
    } else {
      EXPECT(handler_ran == 1 && s.undelivered == undelivered, "%s, then a later fragment of the same poll: the "
             "handler ran %d time(s), undelivered is %u", where, handler_ran, (unsigned) s.undelivered);
    }
  }
}

int main(void) {
  for (int positions = 0; positions < 2; positions++) {
    for (int throws = 0; throws < 2; throws++) {
      subscription_state_t s;
      memset(&s, 0, sizeof s);
      s.positions = positions;
      reset(-1, throws);
      fragment(&s);
      int n = ncalls;
      printf("%s whose handler %s calls:", positions ? "an entry" : "a fragment", throws ? "throws" : "returns");
      for (int i = 0; i < n; i++) printf(" %s", calls[i]);
      printf("\n");
      for (int at = 0; at <= n; at++) one(at, throws, positions);
    }
  }
  /* The first failure of a poll stays its cause: a later fragment's throw is counted only. */
  subscription_state_t s;
  memset(&s, 0, sizeof s);
  reset(0, false);
  fragment(&s);
  reset(-1, true);
  fragment(&s);
  EXPECT(s.undelivered == 1 && s.raised == 1 && s.first == FIRST_FAILURE && s.failed != NULL &&
         strstr(s.failed, "napi_open_handle_scope") != NULL,
         "a fragment kept from the handler, then one whose handler threw: undelivered %u, raised %u, and the cause "
         "is not the first failure", (unsigned) s.undelivered, (unsigned) s.raised);
  /* What poll reports when the runtime kept the poll's first entry from the handler, the body's
   * Buffer refused, and the poll had one more after it. A committed log's report gives the
   * first such entry's position, where the service opens the log again, as the tuple's fifth
   * element; its cause names the entry by that position, as its `position` property too, and
   * says to open the log again there. A subscription's has null there, no `position`, and says
   * polling again does not deliver the fragment. */
  for (int positions = 0; positions < 2; positions++) {
    const char *where = positions ? "a committed log's report" : "a subscription's report";
    memset(&s, 0, sizeof s);
    s.env = (napi_env) HANDLE(0);
    s.positions = positions;
    reset(1, false);
    fragment(&s);
    reset(-1, false);
    trampoline(&s, (const uint8_t *) "next", 4, (const cavalry_header_t *) HANDLE(2));
    reset(-1, false);
    napi_value tuple = report(s.env, &s, 2);
    napi_value at = tuple_elements[4], count = tuple_elements[3], position = property("position");
    printf("%s's cause: %s\n", where, text_made);
    EXPECT(tuple == TUPLE && tuple_elements[2] == ERROR_VALUE, "%s: no tuple with the failure's Error as its cause",
           where);
    EXPECT(is_number(count) && *(int64_t *) count == (positions ? 2 : 1), "%s: undelivered is not %d", where,
           positions ? 2 : 1);
    if (positions) {
      EXPECT(is_number(at) && *(int64_t *) at == 960, "%s: undeliveredAt is not the first entry's position, 960", where);
      EXPECT(position != NULL && is_number(position) && *(int64_t *) position == 960,
             "%s: the cause's position property is not 960", where);
      EXPECT(strstr(text_made, "for the entry at position 960, of session 7 at term offset 4096") != NULL,
             "%s: the cause does not name the entry by its position", where);
      EXPECT(strstr(text_made, "close the log and open a new one at position 960") != NULL,
             "%s: the cause does not say where to open the log again", where);
    } else {
      EXPECT(at == NULL_VALUE, "%s: undeliveredAt is not null", where);
      EXPECT(position == NULL, "%s: the cause has a position property", where);
      EXPECT(strstr(text_made, "for the fragment of session 7 at term offset 4096") != NULL &&
             strstr(text_made, "The ABI consumed the fragment, so polling again does not deliver it") != NULL,
             "%s: the cause does not name the fragment or say that polling again does not deliver it", where);
    }
    EXPECT(strstr(text_made, "See clients/node/README.md#the-exception") != NULL, "%s: the cause names no anchor", where);
    EXPECT(s.first == FIRST_NONE && s.raised == 0 && s.undelivered == 0, "%s: the state was not cleared", where);
  }
  (void) make_bool; (void) make_string; (void) make_external; (void) make_bigint64;
  printf("%d failure(s)\n", failures);
  return failures != 0;
}
'''


class NodePackage(unittest.TestCase):
    """The Node package, `clients/node` (clients/node/README.md; ADR 0046, card 206)."""

    FILES = ["binding.gyp", "index.d.ts", "index.js", "package.json", "src/addon.c"]

    @classmethod
    def setUpClass(cls):
        with open(gen.HEADER, encoding="utf-8") as f:
            cls.text = f.read()
        cls.header = gen.parse(cls.text)

    def committed(self, name):
        with open(os.path.join(gen.ROOT, "clients", "node", name), encoding="utf-8") as f:
            return f.read()

    def test_the_committed_package_is_what_the_header_gives(self):
        files = gen_node.write_node(self.header)
        self.assertEqual(sorted(files), self.FILES)
        for name, content in files.items():
            self.assertEqual(self.committed(name), content, name)
        self.assertIs(gen.WRITERS["node"], gen_node.write_node)
        self.assertEqual(gen.LANGUAGES["node"], "node")

    def test_the_committed_log_reads_both_reports_and_closes_a_log_it_cannot_return(self):
        """The Node committed log (ADR 0070): `failedAt` and `withdrawnAt` each answer the
        position the ABI wrote when it answered true and null when it answered false, the
        addon handing both back from one call; and a log whose constants cannot be read is
        closed, as the Go client closes one it cannot return, after its error is read and
        before the error is thrown."""
        files = gen_node.write_node(self.header)
        log = files["index.js"].split("class CommittedLog {", 1)[1].split("\nclass ", 1)[0]
        for report, answered in (("failedAt", "failed"), ("withdrawnAt", "withdrawn")):
            abi = "cavalry_committed_log_" + ("failed_at" if report == "failedAt" else "withdrawn_at")
            body = log.split(f"  {report}() {{\n", 1)[1].split("\n  }", 1)[0]
            self.assertIn(f"const [{answered}, position] = addon.{abi}(this.handle);", body)
            self.assertIn(f"return {answered} ? position : null;", body)
            native = files["src/addon.c"].split(f"static napi_value fn_{abi}(", 1)[1].split("\n}\n", 1)[0]
            self.assertIn("int64_t position = 0;", native)
            self.assertIn(f"bool result = {abi}(log, &position);", native)
            self.assertLess(native.index("napi_set_element(env, tuple, 0, make_bool(env, result));"),
                            native.index("napi_set_element(env, tuple, 1, make_int64(env, position));"))
            self.assertIn(f"  {report}(): number | null;", files["index.d.ts"])
        # The third report, the lasting read of the image's fault (ADR 0080).
        faulted = log.split("  faulted() {\n", 1)[1].split("\n  }", 1)[0]
        self.assertIn("if (!addon.cavalry_committed_log_faulted(this.handle, f.buffer)) return null;", faulted)
        self.assertIn('return { code: f.int32("code"), sessionId: f.int32("session_id"), position: f.int64("position") };',
                      faulted)
        self.assertIn("  faulted(): { code: number; sessionId: number; position: number } | null;", files["index.d.ts"])
        native = files["src/addon.c"].split("static napi_value fn_cavalry_committed_log_faulted(", 1)[1].split("\n}\n", 1)[0]
        self.assertIn("bool result = cavalry_committed_log_faulted(log, fault);", native)
        opened = log.split("  constructor(handle, client) {\n", 1)[1].split("\n  }\n", 1)[0]
        read = opened.index('const failed = error("cavalry_committed_log_constants");')
        closed = opened.index("addon.cavalry_committed_log_close(handle, client.handle);")
        self.assertLess(opened.index("if (addon.cavalry_committed_log_constants(handle, c.buffer) !== 0) {"), read)
        self.assertLess(read, closed)
        self.assertLess(closed, opened.index("throw failed;"))

    def test_a_header_change_changes_the_addon_and_the_client(self):
        changed = gen.parse(self.text.replace("#define CAVALRY_MAX_IMAGES 256", "#define CAVALRY_MAX_IMAGES 512"))
        files = gen_node.write_node(changed)
        self.assertIn("  CAVALRY_MAX_IMAGES: 512,", files["index.js"])
        self.assertNotEqual(files["index.js"], self.committed("index.js"))
        changed.functions = [fn for fn in changed.functions if fn.name != "cavalry_version_patch"]
        addon = gen_node.write_node(changed)["src/addon.c"]
        self.assertNotIn("fn_cavalry_version_patch", addon)
        self.assertIn("fn_cavalry_version_minor", addon)

    def test_a_publications_close_zeroes_the_claim_struct_before_the_abis_close(self):
        """As the Python package's close does: the struct zeroed after the Buffer's release
        and before the ABI's close, so a commit, abort or reserved value after the close hands
        the ABI no freed handle (spec/c-abi.md, "Which claim a struct names")."""
        code = gen_node.write_node(self.header)["index.js"]
        close = code.split('closeHandle(this, "cavalry_publication_close")', 1)[0]
        close = close[close.rindex("  close() {"):]
        self.assertLess(close.index("this._release();"), close.index("new Uint8Array(this._claim.buffer).fill(0);"))

    def test_the_addon_wraps_every_function_and_the_poll_is_its_own(self):
        """One N-API function per ABI function, registered under the ABI's name, except the
        three that take the C fragment handler: the addon's `poll` makes the one
        `cavalry_subscription_poll` call with its trampoline and its `committedLogPoll` the
        one `cavalry_committed_log_poll` call with the same trampoline (ADR 0070), and the
        header's fields are read through `headerField`. The third, a cluster session's
        poll, has no binding yet: the addon's own poll for it comes with the client's
        session object (ADR 0122)."""
        addon = gen_node.write_node(self.header)["src/addon.c"]
        self.assertEqual(gen_node.HANDLER_FUNCTIONS, ("cavalry_subscription_poll", "cavalry_committed_log_poll",
                                                      "cavalry_cluster_session_poll"))
        self.assertNotIn("cavalry_cluster_session_poll", addon)
        self.assertEqual(addon.count("cavalry_committed_log_poll("), 2)  # the nested refusal and the poll itself
        for fn in self.header.functions:
            if fn.name in gen_node.HANDLER_FUNCTIONS:
                self.assertNotIn(f"fn_{fn.name}", addon)
                continue
            self.assertIn(f"static napi_value fn_{fn.name}(napi_env env, napi_callback_info info)", addon)
            self.assertIn(f'{{ "{fn.name}", NULL, fn_{fn.name}, NULL, NULL, NULL, napi_default, NULL }},', addon)
        self.assertEqual(addon.count("cavalry_subscription_poll("), 2)  # the nested refusal and the poll itself
        for own in ("registerSubscription", "poll", "registerCommittedLog", "committedLogPoll", "headerField", "view",
                    "detach", "stringAt", "buffersHeld"):
            self.assertIn(f'{{ "{own}", NULL, fn_{own}, NULL, NULL, NULL, napi_default, NULL }},', addon)
        self.assertIn("cavalry_header_values(s->current, &s->values)", addon)

    def test_the_layout_the_client_reads_by_is_the_layout_ctypes_gives(self):
        """index.js reads a struct's fields from a DataView at the offsets `layout` computes
        from the header; the Python binding's ctypes structures are the same header's, laid
        out by the C ABI of this machine, so the two must agree, size and offset."""
        _abi = load_package()._abi
        layout = gen_node.layout(self.header)
        for struct in self.header.structs:
            cls = getattr(_abi, struct.name)
            self.assertEqual(layout[struct.name]["size"], ctypes.sizeof(cls), struct.name)
            for field in struct.fields:
                self.assertEqual(layout[struct.name]["fields"][field.name]["offset"], getattr(cls, field.name).offset,
                                 f"{struct.name}.{field.name}")
        index = gen_node.write_node(self.header)["index.js"]
        self.assertIn(f'"cavalry_buffer_claim_t": {{\n    "size": {layout["cavalry_buffer_claim_t"]["size"]},', index)

    def test_handler_errors_message_says_what_to_do_and_names_a_heading_of_the_readme(self):
        """CLAUDE.md: every error message says what to do next and names the docs anchor
        that explains it. The anchor `HandlerError` names is a heading of the README, and so
        is the one the addon's Error for a fragment N-API kept from the handler names; the
        messages are read from the generated source, since the test does not need Node."""
        files = gen_node.write_node(self.header)
        body = files["index.js"].split("class HandlerError extends Error {", 1)[1].split("\n}\n", 1)[0]
        self.assertIn("fragment(s) this poll delivered", body)
        self.assertIn("before it stopped on a fault, which subscription.fault() reads", body)
        self.assertIn("Fix the handler: its first exception, ${describe(cause)}, is this error's cause", body)
        self.assertIn('const one = committedLog ? "entry" : "fragment";', body)
        self.assertIn("never reached the ${one} handler", body)
        self.assertIn("The first failure, ${describe(cause)}, is this ` +\n      `error's cause and says what to do.", body)
        self.assertIn("super(`${what} See clients/node/README.md#the-exception`, { cause });", body)
        # A committed log's poll names its own fault() and its entries: the fault of the
        # log's image is reported once, so the message must not send the reader to a
        # subscription's (clients/node/README.md, "The committed log").
        self.assertIn("which this CommittedLog's fault() reads \" +\n      \"until its next poll, and its faulted() "
                      "from then on when the fault is the log's own image's", body)
        log = files["index.js"].split("class CommittedLog {", 1)[1].split("\n}\n", 1)[0]
        self.assertIn("throw new HandlerError(result[0], result[1], result[2], result[3], true, result[4]);", log)
        subscription = files["index.js"].split("class Subscription {", 1)[1].split("\n}\n", 1)[0]
        self.assertIn("throw new HandlerError(result[0], result[1], result[2], result[3]);", subscription)
        # A committed log's entry the runtime kept from the handler, and every later entry of
        # that poll, did not reach it: the message says where to open the log again, and
        # `undeliveredAt` is that position (clients/node/README.md, "The committed log").
        self.assertIn("close this log and open a new one at position ${undeliveredAt}", body)
        self.assertIn("this.undeliveredAt = undeliveredAt;", body)
        failure = files["src/addon.c"].split("static napi_value failure_error(", 1)[1].split("\n}\n", 1)[0]
        self.assertIn("run the client on a Node built", failure)
        self.assertIn('"The ABI consumed the %s, so polling again does not deliver it", one);', failure)
        self.assertIn("close the log and open a new one at position %lld", failure)
        self.assertIn('the entry at position %lld, of session %d at term offset %d', failure)
        self.assertIn('napi_set_named_property(env, error, "position", make_int64(env, s->failed_position));', failure)
        self.assertIn('%s. %s. See "\n           "clients/node/README.md#the-exception"', failure)
        with open(os.path.join(gen.ROOT, "clients", "node", "README.md"), encoding="utf-8") as f:
            slugs = {re.sub(r"[^a-z0-9 -]", "", line.lstrip("#").strip().lower()).replace(" ", "-")
                     for line in f if line.startswith("#")}
        self.assertIn("the-exception", slugs)

    def test_the_trampoline_counts_and_reports_every_fragment_the_handler_did_not_see(self):
        """Invariant 1: the ABI has consumed a fragment before the trampoline runs, so a
        fragment the handler does not see, because N-API refused a step of the call, is
        counted and its failure kept for poll to report, never dropped; a committed log's
        entry is held to the same, with the step that makes its position. The generated
        trampoline and its helpers are compiled with the runner's `cc` against
        TRAMPOLINE_HARNESS, a stand-in for N-API that fails each call of a fragment in turn,
        for a handler that returns and one that throws: a fragment the handler returned from
        is counted nowhere, every other is counted once, in `raised` or `undelivered`, with
        the first failure naming the call that failed, its status and the frame; every handle
        scope opened is closed and every body made is detached; and a later fragment does not
        replace the poll's first failure. A committed log's entry the handler never saw keeps
        the position the ABI gave for it, 960 in the stand-in, and a later entry of the same
        poll makes no N-API call and does not reach the handler, so that a service never
        applies an entry after a gap; a subscription's later fragment still reaches it. The
        harness compiles the addon's `failure_error` and `report` too: a committed log's poll
        reports that first position, and its cause names the entry by its position, as its
        `position` property too, and says to open the log again there."""
        cc = shutil.which("cc")
        if cc is None:
            self.skipTest("no C compiler on PATH; the Node client's install needs one, and both CI runners have it")
        addon = gen_node.write_node(self.header)["src/addon.c"]
        code = lint.blank(addon)
        # The value makers report uses, then the trampoline and its helpers, then the Error a
        # poll reports and the tuple it returns.
        makers = addon.index("static napi_value make_int32(")
        start = addon.index("/* N-API's statuses by number")
        end = lint.block_end(code, addon.index("{", addon.index("static void trampoline(")))
        error_start = addon.index("static napi_value failure_error(")
        error_end = lint.block_end(code, addon.index("{", addon.index("static napi_value report(")))
        with tempfile.TemporaryDirectory() as tmp:
            source = os.path.join(tmp, "harness.c")
            with open(source, "w", encoding="utf-8") as f:
                f.write(TRAMPOLINE_HARNESS.replace("@TRAMPOLINE@", addon[makers:end] + "\n\n" + addon[error_start:error_end]))
            binary = os.path.join(tmp, "harness")
            include = os.path.join(gen.ROOT, "crates", "cavalry-ffi", "include")
            built = subprocess.run([cc, "-std=c11", "-Wall", "-Wextra", "-I", include, source, "-o", binary],
                                   capture_output=True, text=True)
            self.assertEqual(built.returncode, 0, built.stderr)
            ran = subprocess.run([binary], capture_output=True, text=True)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        # What the runtime is asked for per fragment, which clients/node/README.md, "The
        # call-back, by construction", records: one reference read, the Header's, and none made.
        self.assertIn("a fragment whose handler returns calls: napi_open_handle_scope napi_create_external_buffer "
                      "napi_get_reference_value napi_get_undefined napi_call_function napi_get_typedarray_info "
                      "napi_detach_arraybuffer napi_close_handle_scope\n", ran.stdout)
        # A committed log's entry asks for one thing more, the number its position is handed
        # to the handler as (ADR 0070).
        self.assertIn("an entry whose handler returns calls: napi_open_handle_scope napi_create_external_buffer "
                      "napi_get_reference_value napi_get_undefined napi_create_int64 napi_call_function "
                      "napi_get_typedarray_info napi_detach_arraybuffer napi_close_handle_scope\n", ran.stdout)
        # The cause a service reads when the runtime kept an entry from the handler: the entry
        # by its position, and the position to open the log again at, which is the first such
        # entry's (clients/node/README.md, "The committed log").
        self.assertIn("a committed log's report's cause: the addon could not make the body's Buffer "
                      "(napi_create_external_buffer) for the entry at position 960, of session 7 at term offset 4096: "
                      "N-API status 9, napi_generic_failure; the runtime refused a call the addon makes for every "
                      "fragment: call poll on the thread that made the client, on a Node that package.json's engines "
                      "admits. The ABI consumed the entry, and the handler was handed no entry of this poll from "
                      "position 960 on: close the log and open a new one at position 960. See "
                      "clients/node/README.md#the-exception\n", ran.stdout)
        self.assertIn("a subscription's report's cause: the addon could not make the body's Buffer "
                      "(napi_create_external_buffer) for the fragment of session 7 at term offset 4096: N-API status 9, "
                      "napi_generic_failure; the runtime refused a call the addon makes for every fragment: call poll "
                      "on the thread that made the client, on a Node that package.json's engines admits. The ABI "
                      "consumed the fragment, so polling again does not deliver it. See "
                      "clients/node/README.md#the-exception\n", ran.stdout)
        self.assertIn("0 failure(s)", ran.stdout)

    def test_every_struct_argument_is_checked_for_its_size_and_alignment(self):
        """`module.exports.addon` makes every wrapper public, and the ABI reads and writes a
        whole struct at the address it is given, so a wrapper reads a struct argument with
        struct_arg, which refuses a buffer smaller than the struct or not aligned for it,
        never with bytes_arg alone."""
        addon = gen_node.write_node(self.header)["src/addon.c"]
        structs = {s.name for s in self.header.structs}
        checked = 0
        for fn in self.header.functions:
            for p in fn.params:
                t = " ".join(p.type.split())
                base = t[6:] if t.startswith("const ") else t
                if base.endswith(" *") and base[:-2] in structs:
                    inner = base[:-2]
                    body = addon.split(f"static napi_value fn_{fn.name}(", 1)[1].split("\n}\n", 1)[0]
                    self.assertIn(f"struct_arg(env, argv[", body, fn.name)
                    self.assertIn(f"sizeof({inner}), _Alignof({inner}), \"{p.name}\")", body, fn.name)
                    checked += 1
        self.assertGreaterEqual(checked, 10)
        self.assertNotRegex(addon, r"bytes_arg\(env, argv\[\d+\], NULL,")

    def test_a_poll_makes_no_reference_of_its_own(self):
        """ADR 0046: the client's own code makes no allocation of its own per message. A
        reference is a native object and a V8 global handle, so the addon's poll holds the
        handler's napi_value, valid for the call, and neither the poll nor the trampoline
        and its helpers make or delete a reference."""
        addon = gen_node.write_node(self.header)["src/addon.c"]
        code = lint.blank(addon)
        for name in ("fn_poll", "trampoline", "keep_failure", "undelivered", "threw", "report", "failure_error"):
            start = code.index("{", re.search(rf"^static \w+ \*?{name}\(", code, re.M).start())
            body = code[start:lint.block_end(code, start)]
            for made in ("napi_create_reference(", "napi_delete_reference(", "napi_reference_ref(", "napi_reference_unref("):
                self.assertNotIn(made, body, f"{name} calls {made}")
        self.assertIn("s->handler = argv[2];", addon)

    def test_the_reserved_value_is_a_bigint_both_ways(self):
        """Invariant 1: the reserved value is any int64 a publisher sets, so the header's
        field reaches the handler as a BigInt, exact over the whole range, never as a number,
        which N-API rounds beyond 2^53; `setReservedValue` takes a BigInt and refuses one
        outside the int64 range rather than let N-API wrap it; and the types say both."""
        files = gen_node.write_node(self.header)
        addon = files["src/addon.c"]
        index = gen_node.HEADER_FIELDS.index("reserved_value")
        self.assertIn(f"    case {index}: return make_bigint64(env, f->reserved_value);", addon)
        self.assertIn("static napi_value make_bigint64(napi_env env, int64_t n) { napi_value v; "
                      "napi_create_bigint_int64(env, n, &v); return v; }", addon)
        reader = addon.split("static int64_t int64_arg(", 1)[1].split("\n}\n", 1)[0]
        self.assertIn("if (lossless) return n;", reader)
        self.assertIn("napi_throw_range_error(env, NULL, message);", reader)
        self.assertIn(f"get reservedValue() {{ return addon.headerField(this._state, {index}); }}", files["index.js"])
        self.assertIn("  readonly reservedValue: bigint | undefined;", files["index.d.ts"])
        self.assertIn("  setReservedValue(value: number | bigint): number;", files["index.d.ts"])

    def test_check_fails_on_a_package_that_differs_and_says_what_to_do(self):
        with tempfile.TemporaryDirectory() as tmp:
            shutil.copytree(os.path.join(gen.ROOT, "clients", "node"), os.path.join(tmp, "clients", "node"),
                            ignore=shutil.ignore_patterns("build", "node_modules", "package-lock.json"))
            os.makedirs(os.path.join(tmp, "crates", "cavalry-ffi", "include"))
            shutil.copy(gen.HEADER, os.path.join(tmp, "crates", "cavalry-ffi", "include", "cavalry.h"))
            root, header = gen.ROOT, gen.HEADER
            gen.ROOT = tmp
            gen.HEADER = os.path.join(tmp, "crates", "cavalry-ffi", "include", "cavalry.h")
            try:
                self.assertEqual(gen.main(["generate.py", "node", "--check"]), 0)
                with open(os.path.join(tmp, "clients", "node", "src", "addon.c"), "a", encoding="utf-8") as f:
                    f.write("/* edited by hand */\n")
                import io
                import contextlib
                err = io.StringIO()
                with contextlib.redirect_stderr(err):
                    self.assertEqual(gen.main(["generate.py", "node", "--check"]), 1)
                self.assertIn("differs from what the header gives in src/addon.c", err.getvalue())
                self.assertIn("python3 clients/generate.py node", err.getvalue())
            finally:
                gen.ROOT, gen.HEADER = root, header


class GoPackage(unittest.TestCase):
    """The Go module, `clients/go` (clients/go/README.md; ADR 0046, card 207)."""

    FILES = ["cavalry.go", "cavalry_test.go", "go.mod", "shim.c", "shim.h", "trampoline.go"]

    @classmethod
    def setUpClass(cls):
        with open(gen.HEADER, encoding="utf-8") as f:
            cls.text = f.read()
        cls.header = gen.parse(cls.text)

    def committed(self, name):
        with open(os.path.join(gen.ROOT, "clients", "go", name), encoding="utf-8") as f:
            return f.read()

    def test_the_committed_module_is_what_the_header_gives(self):
        files = gen_go.write_go(self.header)
        self.assertEqual(sorted(files), self.FILES)
        for name, content in files.items():
            self.assertEqual(self.committed(name), content, name)
        self.assertIs(gen.WRITERS["go"], gen_go.write_go)
        self.assertEqual(gen.LANGUAGES["go"], "go")

    def test_a_header_change_changes_the_constants(self):
        changed = gen.parse(self.text.replace("#define CAVALRY_MAX_IMAGES 256", "#define CAVALRY_MAX_IMAGES 512"))
        code = gen_go.write_go(changed)["cavalry.go"]
        self.assertRegex(code, r"MaxImages\s+= 512\s+// CAVALRY_MAX_IMAGES")
        self.assertNotEqual(code, self.committed("cavalry.go"))

    def test_a_publications_close_zeroes_the_claim_struct_before_the_abis_close(self):
        """As the Python package's close does: `p.claim` zeroed beside `p.Claim = nil` and
        before the ABI's close, so Commit, Abort or SetReservedValue after the close hands
        the ABI no freed handle (spec/c-abi.md, "Which claim a struct names")."""
        code = gen_go.write_go(self.header)["cavalry.go"]
        close = code.split("func (p *Publication) Close() error {\n", 1)[1].split("\n}\n", 1)[0]
        zeroed = close.index("p.claim = C.cavalry_buffer_claim_t{}")
        self.assertLess(close.index("p.Claim = nil"), zeroed)
        self.assertLess(zeroed, close.index("C.cavalry_publication_close(p.handle)"))

    def test_the_committed_log_reads_both_reports_and_closes_a_log_it_cannot_return(self):
        """The Go committed log (ADR 0070): `FailedAt` and `WithdrawnAt` each answer the
        position the ABI wrote and true when it answered true, and 0 and false when it
        answered false; and the open closes a log whose constants cannot be read, or that
        the table has no slot for, before it returns the error."""
        code = gen_go.write_go(self.header)["cavalry.go"]
        for report, abi in (("FailedAt", "cavalry_committed_log_failed_at"),
                            ("WithdrawnAt", "cavalry_committed_log_withdrawn_at")):
            body = code.split(f"func (l *CommittedLog) {report}() (int64, bool) {{\n", 1)[1].split("\n}\n", 1)[0]
            self.assertEqual(body, "\tvar position C.int64_t\n"
                                   f"\tif !bool(C.{abi}(l.handle, &position)) {{\n"
                                   "\t\treturn 0, false\n"
                                   "\t}\n"
                                   "\treturn int64(position), true")
        # The third report, the lasting read of the image's fault (ADR 0080).
        faulted = code.split("func (l *CommittedLog) Faulted() (code int32, sessionID int32, position int64, "
                             "faulted bool) {\n", 1)[1].split("\n}\n", 1)[0]
        self.assertEqual(faulted, "\tvar f C.cavalry_fault_t\n"
                                  "\tif !bool(C.cavalry_committed_log_faulted(l.handle, &f)) {\n"
                                  "\t\treturn 0, 0, 0, false\n"
                                  "\t}\n"
                                  "\treturn int32(f.code), int32(f.session_id), int64(f.position), true")
        opened = code.split("func (c *Client) CommittedLog(position int64) (*CommittedLog, error) {\n", 1)[1]
        opened = opened.split("\n}\n", 1)[0]
        constants = opened.index('check("cavalry_committed_log_constants", C.cavalry_committed_log_constants(handle, &k))')
        table = opened.index("if err := register(state); err != nil {")
        closes = [at for at in range(len(opened)) if opened.startswith("C.cavalry_committed_log_close(handle, c.handle)", at)]
        self.assertEqual(len(closes), 2)
        self.assertLess(constants, closes[0])
        self.assertLess(closes[0], table)
        self.assertLess(table, closes[1])

    def test_every_define_is_a_constant_under_its_go_name(self):
        code = gen_go.write_go(self.header)["cavalry.go"]
        for name, _ in self.header.defines:
            if name != "CAVALRY_H":
                self.assertRegex(code, r"\t" + gen_go.go_name(name) + r"\s+= \S.*// " + name + r"\n")
        self.assertEqual(gen_go.go_name("CAVALRY_ABI_VERSION"), "ABIVersion")
        self.assertEqual(gen_go.go_name("CAVALRY_ERR_NOT_UTF8"), "ErrNotUTF8")
        self.assertEqual(gen_go.go_name("CAVALRY_PUBLICATION_BACK_PRESSURED"), "PublicationBackPressured")
        self.assertEqual(gen_go.go_value("INT64_MAX"), "math.MaxInt64")

    def test_the_shim_installs_the_exported_trampoline_around_the_one_poll(self):
        files = gen_go.write_go(self.header)
        self.assertIn("cavalryGoFragment((uintptr_t) clientd, (uint8_t *) buffer, length, (cavalry_header_t *) header);",
                      files["shim.c"])
        self.assertIn("cavalry_subscription_poll(subscription, cavalry_go_trampoline, (void *) index, fragment_limit)",
                      files["shim.c"])
        self.assertEqual(files["shim.c"].count("cavalry_subscription_poll("), 1)
        self.assertIn("//export cavalryGoFragment\n", files["trampoline.go"])
        self.assertIn("C.cavalry_go_poll(s.handle, C.uintptr_t(s.slot), C.size_t(fragmentLimit), &s.err)", files["cavalry.go"])
        self.assertNotIn("C.cavalry_subscription_poll(", files["cavalry.go"])

    def test_a_committed_logs_trampoline_reads_the_position_in_c(self):
        """A committed log's poll installs a trampoline of its own, which reads the entry's
        position from the header and hands it to `cavalryGoEntry` in the one call back, as
        the Node addon's does: the Go side makes no cgo call for it, which it made once
        for each entry before (clients/go/README.md, "The call-back, by construction")."""
        files = gen_go.write_go(self.header)
        shim = files["shim.c"]
        entry = shim.split("static void cavalry_go_entry_trampoline(", 1)[1].split("\n}\n", 1)[0]
        self.assertIn("cavalryGoEntry((uintptr_t) clientd, (uint8_t *) buffer, length, (cavalry_header_t *) header,\n"
                      "                 cavalry_committed_log_entry_position(header));", entry)
        self.assertIn("cavalry_committed_log_poll(log, cavalry_go_entry_trampoline, (void *) index, fragment_limit)", shim)
        self.assertEqual(shim.count("cavalry_committed_log_poll("), 1)
        self.assertEqual(shim.count("cavalry_committed_log_entry_position("), 1)
        trampoline = files["trampoline.go"]
        self.assertIn("//export cavalryGoEntry\n", trampoline)
        entered = trampoline.split("func cavalryGoEntry(", 1)[1].split("\n}\n", 1)[0]
        self.assertIn("position C.int64_t) {", entered)
        self.assertIn("s.deliverEntry(body, int64(position))", entered)
        self.assertNotRegex(entered, r"C\.cavalry_\w+\(")   # no call back into C: types alone
        code = files["cavalry.go"]
        self.assertNotIn("C.cavalry_committed_log_entry_position(", code)
        delivered = code.split("func (s *Subscription) deliverEntry(body []byte, position int64) {\n", 1)[1].split("\n}\n", 1)[0]
        # No guard for a nil handler or a slot of the other kind: that call panics, the
        # one deferred call recovers and counts it, and the poll reports it.
        self.assertEqual(delivered, "\tdefer s.recovered()\n\ts.entry(body, &s.header, position)")
        fragment = code.split("func (s *Subscription) deliver(body []byte) {\n", 1)[1].split("\n}\n", 1)[0]
        self.assertEqual(fragment, "\tdefer s.recovered()\n\ts.handler(body, &s.header)")
        recovered = code.split("func (s *Subscription) recovered() {\n", 1)[1].split("\n}\n", 1)[0]
        self.assertIn("if r := recover(); r != nil {", recovered)
        self.assertIn("s.raised++", recovered)
        self.assertIn("log.raised != 1 || log.first == nil || subscription.raised != 1", files["cavalry_test.go"])
        for call in ("cavalryGoEntry(0, nil, 0, nil, 96)", "cavalryGoEntry(maxSubscriptions+1, nil, 0, nil, 96)",
                     "cavalryGoFragment(1, nil, 0, nil)"):
            self.assertIn(call, files["cavalry_test.go"])
        for name in ("Subscription.deliverEntry", "cavalryGoEntry"):
            self.assertIn(name, gen_go.PER_MESSAGE_GO)

    def test_each_per_message_call_keeps_its_error_in_the_c_call_that_failed(self):
        """The ABI's error is the calling thread's and Go may run a goroutine's next cgo
        call on another thread (clients/go/README.md, "The thread's error"), so the Go code
        reaches each per-message function through a function of shim.c that copies the
        thread's error into the handle when the call failed, and reads no error itself."""
        files = gen_go.write_go(self.header)
        shim, code = files["shim.c"], files["cavalry.go"]
        calls = {"cavalry_go_try_claim": ("cavalry_publication_try_claim", "position == CAVALRY_PUBLICATION_ERROR"),
                 "cavalry_go_offer": ("cavalry_publication_offer", "position == CAVALRY_PUBLICATION_ERROR"),
                 "cavalry_go_commit": ("cavalry_buffer_claim_commit", "position < 0"),
                 "cavalry_go_abort": ("cavalry_buffer_claim_abort", "position < 0"),
                 "cavalry_go_set_reserved_value": ("cavalry_buffer_claim_set_reserved_value", "result != 0"),
                 "cavalry_go_poll": ("cavalry_subscription_poll", "count < 0")}
        for shim_call, (abi, failed) in calls.items():
            body = shim.split(f" {shim_call}(", 1)[1].split("\n}\n", 1)[0]
            self.assertEqual(body.count(f"{abi}("), 1, shim_call)
            self.assertIn(f"if ({failed}) {{\n    cavalry_go_keep(error, \"{abi}\");\n  }}", body)
            self.assertIn(f"C.{shim_call}(", code)
            self.assertNotIn(f"C.{abi}(", code)
            self.assertIn(f"{shim_call}(", files["shim.h"])
        self.assertIn("error->code = (int32_t) cavalry_errcode();", shim)
        self.assertIn("cavalry_go_copy(error->message, sizeof error->message, cavalry_errmsg());", shim)
        self.assertIn("char message[512];", files["shim.h"])
        for method in ("Publication.TryClaim", "Publication.Offer", "Publication.Commit", "Publication.Abort",
                       "Publication.SetReservedValue", "Subscription.Poll"):
            receiver, name = method.split(".")
            body = code.split(f") {name}(", 1)[1].split("\n}\n", 1)[0]
            self.assertNotIn("cavalry_errcode", body, method)
            self.assertNotIn("Errcode", body, method)
            self.assertIn(f"&{receiver[0].lower()}.err)", body, method)

    def test_the_package_tests_the_table_at_its_bound_and_run_sh_runs_them(self):
        """The subscription table is bounded (CLAUDE.md, invariant 3; gate 3: a test at
        capacity), so the module carries a test that fills it and calls the trampoline with
        slots the table does not hold, and conformance/go/run.sh runs it beside go vet. The
        trampoline takes its slot as an integer, never as a pointer, so the test calls it
        with constants and Go's stack copier never meets a small value in a pointer."""
        files = gen_go.write_go(self.header)
        test = files["cavalry_test.go"]
        self.assertIn("func TestTheTableAtItsBound(t *testing.T) {", test)
        self.assertIn("for len(held) < maxSubscriptions {", test)
        self.assertIn("abi.Code != ErrSubscriptionsFull", test)
        self.assertIn('"see clients/go/README.md#threads"', test)
        for call in ("cavalryGoFragment(0, nil, 0, nil)", "cavalryGoFragment(maxSubscriptions+1, nil, 0, nil)",
                     "cavalryGoFragment(1, nil, 0, nil)"):
            self.assertIn(call, test)
        self.assertNotIn('import "C"', test)
        self.assertIn("func cavalryGoFragment(slot C.uintptr_t, buffer *C.uint8_t,", files["trampoline.go"])
        self.assertNotIn("unsafe.Pointer(clientd)", files["trampoline.go"])
        self.assertIn("see clients/go/README.md#threads", files["cavalry.go"])
        with open(os.path.join(gen.ROOT, "conformance", "go", "run.sh"), encoding="utf-8") as f:
            self.assertIn("(cd clients/go && go vet ./... && go build ./... && go test -count=1 ./...)", f.read())

    def test_every_call_on_the_client_handle_holds_the_clients_mutex(self):
        """Goroutines may share a Client (clients/go/README.md, "Threads"): the ABI runs
        one call on a handle at a time, so each method that reaches the client handle
        begins by taking the client's mutex, and none of the per-message functions does."""
        files = gen_go.write_go(self.header)
        code = files["cavalry.go"]
        module = lint.GoModule([("cavalry.go", code), ("trampoline.go", files["trampoline.go"])])
        methods = [label for label, (_, _, body, _, _) in module.functions.items()
                   if label.startswith("Client.") and "c.handle" in body]
        self.assertEqual(sorted(methods), sorted(
            ["Client.IsClosed", "Client.Closed", "Client.Close", "Client.AddPublication", "Client.AddExclusivePublication",
             "Client.AddSubscription", "Client.StartRecording", "Client.StopRecording", "Client.TruncateRecording",
             "Client.PurgeSegments", "Client.PurgeRecording", "Client.StartReplay", "Client.StopReplay",
             "Client.AddDestination", "Client.RemoveDestination", "Client.CommittedLog"]))
        for label in methods:
            body = module.functions[label][2]
            self.assertTrue(body.startswith("{\n\tc.mu.Lock()\n\tdefer c.mu.Unlock()\n"), label)
        self.assertIn("\tmu       sync.Mutex\n\thandle   *C.cavalry_t\n", code)
        for label in lint.RULES["go"]["functions"]:
            self.assertNotIn(".mu.", module.functions[label][2], label)

    def test_every_call_that_reads_the_threads_error_holds_its_os_thread(self):
        """The committed module passes lint.py's thread rule, and each function that reads
        the error through check or lastError begins its ABI work under the lock."""
        code = gen_go.write_go(self.header)["cavalry.go"]
        module = lint.GoModule([("cavalry.go", code)])
        self.assertEqual(lint.go_thread_findings("go", module, ["check", "lastError"]), [])
        readers = [label for label, (_, _, body, _, _) in module.functions.items()
                   if label not in ("check", "lastError") and re.search(r"(?<![\w.])(check|lastError)\(", body)]
        self.assertGreaterEqual(len(readers), 20, readers)
        for name in ("NewClient", "Client.Close", "Client.AddSubscription", "Subscription.Close", "Subscription.Fault",
                     "newPublication", "Replay.Close"):
            self.assertIn(name, readers)

    def test_handler_errors_message_says_what_to_do_and_names_a_heading_of_the_readme(self):
        code = gen_go.write_go(self.header)["cavalry.go"]
        body = code.split("func (e *HandlerError) Error() string {", 1)[1].split("\n}\n", 1)[0]
        self.assertIn("fragment(s) this poll delivered", body)
        self.assertIn("before it stopped on a fault, which Subscription.Fault reads", body)
        self.assertIn("Fix the handler: its first panic's value, %v, is this error's Cause", body)
        self.assertIn("See clients/go/README.md#the-panic", body)
        # A committed log's poll names its own Fault and Err and its entries: the fault of
        # the log's image is reported once, so the message must not send the reader to a
        # subscription's (clients/go/README.md, "The committed log").
        self.assertIn('one, many = "entry", "entries"', body)
        self.assertIn("which this CommittedLog's Fault \" +\n\t\t\t\"reads until its next poll and its Err until its next "
                      "failure, and its Faulted from then on when \" +\n\t\t\t\"the fault is the log's own image's", body)
        log = code.split("func (l *CommittedLog) Poll(", 1)[1].split("\n}\n", 1)[0]
        self.assertIn("&HandlerError{Fragments: count, Raised: s.raised, Cause: first, CommittedLog: true}", log)
        subscription = code.split("func (s *Subscription) Poll(", 1)[1].split("\n}\n", 1)[0]
        self.assertIn("&HandlerError{Fragments: count, Raised: s.raised, Cause: first}", subscription)
        with open(os.path.join(gen.ROOT, "clients", "go", "README.md"), encoding="utf-8") as f:
            slugs = {re.sub(r"[^a-z0-9 -]", "", line.lstrip("#").strip().lower()).replace(" ", "-")
                     for line in f if line.startswith("#")}
        self.assertIn("the-panic", slugs)

    def test_the_generated_go_is_gofmts_and_vets_clean_when_go_is_on_the_path(self):
        """gofmt's layout is the generator's own, so the committed files never need a
        formatter; `go vet` compiles the module's cgo against the header, which needs the
        C compiler and no library. Without `go`, as on a machine that builds no client,
        the check is CI's (`just conformance`)."""
        if shutil.which("go") is None or shutil.which("gofmt") is None:
            print("generate-test: no go on the path; gofmt and vet are checked in CI")
            return
        with tempfile.TemporaryDirectory() as tmp:
            for name, content in gen_go.write_go(self.header).items():
                with open(os.path.join(tmp, name), "w", encoding="utf-8") as f:
                    f.write(content)
            listed = subprocess.run(["gofmt", "-l", tmp], capture_output=True, text=True, check=True).stdout
            self.assertEqual(listed.strip(), "", listed)
        vet = subprocess.run(["go", "vet", "./..."], cwd=os.path.join(gen.ROOT, "clients", "go"), capture_output=True, text=True)
        self.assertEqual(vet.returncode, 0, vet.stderr)

    def test_check_fails_on_a_module_that_differs_and_says_what_to_do(self):
        with tempfile.TemporaryDirectory() as tmp:
            shutil.copytree(os.path.join(gen.ROOT, "clients", "go"), os.path.join(tmp, "clients", "go"))
            os.makedirs(os.path.join(tmp, "crates", "cavalry-ffi", "include"))
            shutil.copy(gen.HEADER, os.path.join(tmp, "crates", "cavalry-ffi", "include", "cavalry.h"))
            root, header = gen.ROOT, gen.HEADER
            gen.ROOT = tmp
            gen.HEADER = os.path.join(tmp, "crates", "cavalry-ffi", "include", "cavalry.h")
            try:
                self.assertEqual(gen.main(["generate.py", "go", "--check"]), 0)
                with open(os.path.join(tmp, "clients", "go", "shim.c"), "a", encoding="utf-8") as f:
                    f.write("/* edited by hand */\n")
                import io
                import contextlib
                err = io.StringIO()
                with contextlib.redirect_stderr(err):
                    self.assertEqual(gen.main(["generate.py", "go", "--check"]), 1)
                self.assertIn("differs from what the header gives in shim.c", err.getvalue())
                self.assertIn("python3 clients/generate.py go", err.getvalue())
            finally:
                gen.ROOT, gen.HEADER = root, header



JVM_BASE = "src/com/appshapes/cavalry/"


class JvmPackage(unittest.TestCase):
    """The JVM package, `clients/jvm` (clients/jvm/README.md; ADR 0046, card 208)."""

    BASE = JVM_BASE
    FILES = sorted(JVM_BASE + name for name in (
        "Abi.java", "CavalryException.java", "Client.java", "CommittedLog.java", "EntryHandler.java",
        "FragmentHandler.java", "HandlerException.java", "Header.java", "Image.java", "Publication.java",
        "Recording.java", "Replay.java", "Subscription.java"))

    @classmethod
    def setUpClass(cls):
        with open(gen.HEADER, encoding="utf-8") as f:
            cls.text = f.read()
        cls.header = gen.parse(cls.text)

    def committed(self, name):
        with open(os.path.join(gen.ROOT, "clients", "jvm", name), encoding="utf-8") as f:
            return f.read()

    def test_the_committed_package_is_what_the_header_gives(self):
        files = gen_jvm.write_jvm(self.header)
        self.assertEqual(sorted(files), self.FILES)
        for name, content in files.items():
            self.assertEqual(self.committed(name), content, name)
        self.assertIs(gen.WRITERS["jvm"], gen_jvm.write_jvm)
        self.assertEqual(gen.LANGUAGES["jvm"], "jvm")

    def test_the_committed_log_reads_both_reports_and_closes_a_log_it_cannot_return(self):
        """The JVM committed log (ADR 0070): `failedAt` and `withdrawnAt` each answer the
        position the ABI wrote when it answered true and an empty `OptionalLong` when it
        answered false; and a log whose upcall stub cannot be made or whose constants
        cannot be read is closed, as the Go client closes one it cannot return, with its
        arena, the constants' error read before the close."""
        log = gen_jvm.write_jvm(self.header)[self.BASE + "CommittedLog.java"]
        for report, answered in (("failedAt", "failed"), ("withdrawnAt", "withdrawn")):
            abi = "cavalry_committed_log_" + ("failed_at" if report == "failedAt" else "withdrawn_at")
            body = log.split(f"    public OptionalLong {report}() {{\n", 1)[1].split("\n    }\n", 1)[0]
            self.assertIn("MemorySegment position = scratch.allocate(JAVA_LONG);", body)
            self.assertIn(f"boolean {answered} = (boolean) Abi.{abi}.invokeExact(handle, position);", body)
            self.assertIn(f"return {answered} ? OptionalLong.of(position.get(JAVA_LONG, 0)) : OptionalLong.empty();", body)
            self.assertIn(f'throw Abi.failed("{abi}", t);', body)
        # The third report, the lasting read of the image's fault (ADR 0080).
        faulted = log.split("    public Optional<Subscription.Fault> faulted() {\n", 1)[1].split("\n    }\n", 1)[0]
        self.assertIn("boolean faulted = (boolean) Abi.cavalry_committed_log_faulted.invokeExact(handle, f);", faulted)
        self.assertLess(faulted.index("return Optional.empty();"),
                        faulted.index("return Optional.of(new Subscription.Fault(f.get(JAVA_INT, Abi.cavalry_fault_t_code),"))
        self.assertIn('throw Abi.failed("cavalry_committed_log_faulted", t);', faulted)
        self.assertIn("import java.util.Optional;\n", log)
        opened = log.split("    CommittedLog(MemorySegment handle, Client client) {\n", 1)[1].split("\n    }\n", 1)[0]
        # Three ways out, each closing the log before anything is thrown: the stub's two
        # and the constants', where `Abi.failed` throws an Error on and so comes after.
        self.assertEqual(opened.count("abandon();"), 3)
        self.assertNotIn("arena.close();", opened)
        for thrown in ('throw new CavalryException("cavalry_committed_log", 0, "the upcall stub could not be made: " + e);',
                       "throw e;", 'throw Abi.failed("cavalry_committed_log_constants", t);'):
            at = opened.index(thrown)
            self.assertEqual(opened[:at].rstrip().rsplit("\n", 1)[-1].strip(), "abandon();", thrown)
        self.assertIn("} catch (RuntimeException e) {", opened)
        abandon = log.split("    private void abandon() {\n", 1)[1].split("\n    }\n", 1)[0]
        closed = abandon.index("client.closeCommittedLog(handle);")
        self.assertLess(closed, abandon.index("} finally {"))
        self.assertLess(abandon.index("} finally {"), abandon.index("handle = MemorySegment.NULL;"))
        self.assertLess(abandon.index("handle = MemorySegment.NULL;"), abandon.index("arena.close();"))

    def test_a_header_change_changes_the_constants(self):
        changed = gen.parse(self.text.replace("#define CAVALRY_MAX_IMAGES 256", "#define CAVALRY_MAX_IMAGES 512"))
        code = gen_jvm.write_jvm(changed)[self.BASE + "Abi.java"]
        self.assertIn("    public static final int MAX_IMAGES = 512; // CAVALRY_MAX_IMAGES\n", code)
        self.assertNotEqual(code, self.committed(self.BASE + "Abi.java"))

    def test_every_define_is_a_constant_under_its_java_name(self):
        code = gen_jvm.write_jvm(self.header)[self.BASE + "Abi.java"]
        for name, _ in self.header.defines:
            if name != "CAVALRY_H":
                self.assertRegex(code, r"    public static final \w+ " + gen_jvm.java_name(name) + r" = \S.*; // " + name + r"\n")
        self.assertEqual(gen_jvm.java_name("CAVALRY_ERR_POLLING"), "ERR_POLLING")
        self.assertEqual(gen_jvm.java_name("CAVALRY_ABI_VERSION"), "ABI_VERSION")
        self.assertEqual(gen_jvm.java_constant("INT64_MAX"), ("long", "Long.MAX_VALUE"))
        self.assertEqual(gen_jvm.java_constant("(-1)"), ("int", "-1"))

    def test_the_binding_reads_the_abi_version_before_it_binds_any_other_function(self):
        """The static initializer binds `cavalry_abi_version`, calls it and compares, and
        binds every other function after the comparison (spec/c-abi.md, "Versions and what
        a reader rejects"); a header without the check's names cannot be generated."""
        code = gen_jvm.write_jvm(self.header)[self.BASE + "Abi.java"]
        block = code.split("    static {\n", 1)[1].split("\n    }\n", 1)[0]
        first = block.index("cavalry_abi_version = LINKER.downcallHandle(")
        read = block.index("found = (int) cavalry_abi_version.invokeExact();")
        check = block.index("if (found != ABI_VERSION) {")
        self.assertLess(first, read)
        self.assertLess(read, check)
        others = [m.start() for m in re.finditer(r"\n        cavalry_\w+ = LINKER\.downcallHandle\(", block)]
        self.assertEqual(len(others), len(self.header.functions) - 1)
        self.assertTrue(all(check < at for at in others), "a function bound before the version check")
        self.assertIn("ERR_ABI_VERSION", block)
        self.assertIn("docs/reference/c-abi.md#versions-and-what-a-reader-rejects", block)
        without = gen.parse(self.text.replace("#define CAVALRY_ABI_VERSION ", "#define CAVALRY_ABI_VERSION_GONE "))
        with self.assertRaises(SystemExit) as refused:
            gen_jvm.write_jvm(without)
        self.assertIn("CAVALRY_ABI_VERSION", str(refused.exception))

    def test_the_offer_alone_is_critical_and_each_subscription_makes_one_upcall_stub(self):
        files = gen_jvm.write_jvm(self.header)
        abi, subscription = files[self.BASE + "Abi.java"], files[self.BASE + "Subscription.java"]
        self.assertRegex(abi, r'cavalry_publication_offer = LINKER\.downcallHandle\(LOOKUP\.findOrThrow\("cavalry_publication_offer"\), '
                              r'FunctionDescriptor\.of\(JAVA_LONG, ADDRESS, ADDRESS, JAVA_LONG\), Linker\.Option\.critical\(true\)\);')
        self.assertEqual(abi.count("Linker.Option.critical("), 1)
        self.assertEqual(subscription.count("Abi.LINKER.upcallStub("), 1)
        self.assertIn('findVirtual(Subscription.class, "onFragment"', subscription)
        self.assertIn(".bindTo(this)", subscription)
        self.assertEqual(subscription.count("Abi.cavalry_subscription_poll.invokeExact("), 2)  # the refused nested poll and the poll

    def test_a_publications_close_zeroes_the_claim_struct_before_the_abi_frees_its_handle(self):
        """The claim struct's `publication` field names the handle the close frees, and the
        ABI reads that field on a commit, an abort or a reserved value, so `close()` zeroes
        the struct before the ABI's close: a claim's call after it hands the ABI a struct
        that names no publication, which it answers with NO_CLAIM without reading a handle
        (spec/c-abi.md, "Which claim a struct names"). The runner's refusals round makes
        those calls; this check fails without the zeroing every time, where a read of the
        freed handle may still answer NO_CLAIM by luck."""
        code = gen_jvm.write_jvm(self.header)[self.BASE + "Publication.java"]
        close = code.split("    public void close() {\n", 1)[1].split("\n    }\n", 1)[0]
        guard = close.index("if (handle.address() == 0) {")
        zeroed = close.index("claimStruct.fill((byte) 0);")
        freed = close.index("Abi.cavalry_publication_close.invokeExact(")
        self.assertLess(guard, zeroed)
        self.assertLess(zeroed, freed)
        for call in ("cavalry_buffer_claim_commit", "cavalry_buffer_claim_abort", "cavalry_buffer_claim_set_reserved_value"):
            self.assertIn("Abi." + call + ".invokeExact(claimStruct", code)

    def test_a_claims_segment_is_one_slice_of_a_private_segment_over_the_address_space(self):
        """`tryClaim` makes the claim's segment with one `asSlice` of the publication's
        private `EVERYTHING`, at the address the ABI filled in read as a `long`, so one
        object per claim. Reading the address as an `ADDRESS` would make a segment of length
        zero first, and the `reinterpret` over it a second (clients/jvm/README.md, "The view
        over a claimed slice"). The segment over the address space is never handed out, so
        the caller sees the claimed bytes alone."""
        code = gen_jvm.write_jvm(self.header)[self.BASE + "Publication.java"]
        self.assertIn("    private static final MemorySegment EVERYTHING = MemorySegment.NULL.reinterpret(Long.MAX_VALUE);\n", code)
        self.assertEqual(code.count("EVERYTHING"), 3)  # the field, the Javadoc of tryClaim, and its one slice
        body = code.split("    public long tryClaim(int length) {\n", 1)[1].split("\n    }\n", 1)[0]
        self.assertIn("claim = EVERYTHING.asSlice(claimStruct.get(JAVA_LONG, Abi.cavalry_buffer_claim_t_data),\n"
                      "                    claimStruct.get(JAVA_LONG, Abi.cavalry_buffer_claim_t_length));", body)
        self.assertNotIn("ADDRESS", body)
        self.assertNotIn("reinterpret", body)
        self.assertRegex(gen_jvm.write_jvm(self.header)[self.BASE + "Abi.java"],
                         r"static final long cavalry_buffer_claim_t_length = 8;")  # the 8-byte pointer before it

    def test_handler_exceptions_message_says_what_to_do_and_names_a_heading_of_the_readme(self):
        files = gen_jvm.write_jvm(self.header)
        code = files[self.BASE + "HandlerException.java"]
        self.assertIn("    public HandlerException(int fragments, int raised, Throwable cause, boolean committedLog) {\n"
                      "        super(describe(fragments, raised, cause, committedLog), cause);\n", code)
        body = code.split("private static String describe(int fragments, int raised, Throwable cause, boolean committedLog) {",
                          1)[1].split("\n    }\n", 1)[0]
        self.assertIn("fragment(s) this poll delivered", body)
        self.assertIn("before it stopped on a fault, which Subscription.fault() reads", body)
        self.assertIn("Fix the handler: its first throwable, ", body)
        self.assertIn("See clients/jvm/README.md#the-exception", body)
        # A committed log's poll names its own fault() and its entries: the fault of the
        # log's image is reported once, so the message must not send the reader to a
        # subscription's (clients/jvm/README.md, "The committed log").
        self.assertIn('String one = committedLog ? "entry" : "fragment";', body)
        self.assertIn("which this CommittedLog's fault() reads\"\n                    + \" until its next poll, and its "
                      "faulted() from then on when the fault is the log's own image's\"", body)
        self.assertIn("throw new HandlerException(count, raised, cause, true);", files[self.BASE + "CommittedLog.java"])
        self.assertIn("throw new HandlerException(count, raised, cause);", files[self.BASE + "Subscription.java"])
        with open(os.path.join(gen.ROOT, "clients", "jvm", "README.md"), encoding="utf-8") as f:
            slugs = {re.sub(r"[^a-z0-9 -]", "", line.lstrip("#").strip().lower()).replace(" ", "-")
                     for line in f if line.startswith("#")}
        self.assertIn("the-exception", slugs)

    def test_the_generated_java_compiles_with_every_lint_an_error_when_javac_25_is_on_the_path(self):
        """The README builds the client with `javac --release 25` and run.sh adds
        `-Xlint:all -Werror`, so the generated files carry no warning; without a javac of
        Java 25 or later on the path, as on a machine that builds no client, the check is
        CI's (`just conformance`)."""
        javac = shutil.which("javac")
        version = subprocess.run([javac, "-version"], capture_output=True, text=True) if javac else None
        found = re.search(r"javac (\d+)", (version.stdout + version.stderr) if version else "")
        if version is None or version.returncode != 0 or found is None or int(found.group(1)) < 25:
            print("generate-test: no javac of Java 25 or later on the path; the compile is checked in CI")
            return
        with tempfile.TemporaryDirectory() as tmp:
            paths = []
            for name, content in gen_jvm.write_jvm(self.header).items():
                path = os.path.join(tmp, name)
                os.makedirs(os.path.dirname(path), exist_ok=True)
                with open(path, "w", encoding="utf-8") as f:
                    f.write(content)
                paths.append(path)
            compiled = subprocess.run([javac, "--release", "25", "-Xlint:all", "-Werror", "-d", os.path.join(tmp, "classes")]
                                      + paths, capture_output=True, text=True)
            self.assertEqual(compiled.returncode, 0, compiled.stderr)

    def test_check_fails_on_a_package_that_differs_and_says_what_to_do(self):
        with tempfile.TemporaryDirectory() as tmp:
            shutil.copytree(os.path.join(gen.ROOT, "clients", "jvm"), os.path.join(tmp, "clients", "jvm"),
                            ignore=shutil.ignore_patterns("build"))
            os.makedirs(os.path.join(tmp, "crates", "cavalry-ffi", "include"))
            shutil.copy(gen.HEADER, os.path.join(tmp, "crates", "cavalry-ffi", "include", "cavalry.h"))
            root, header = gen.ROOT, gen.HEADER
            gen.ROOT = tmp
            gen.HEADER = os.path.join(tmp, "crates", "cavalry-ffi", "include", "cavalry.h")
            try:
                self.assertEqual(gen.main(["generate.py", "jvm", "--check"]), 0)
                with open(os.path.join(tmp, "clients", "jvm", self.BASE + "Header.java"), "a", encoding="utf-8") as f:
                    f.write("// edited by hand\n")
                import io
                import contextlib
                err = io.StringIO()
                with contextlib.redirect_stderr(err):
                    self.assertEqual(gen.main(["generate.py", "jvm", "--check"]), 1)
                self.assertIn("differs from what the header gives in " + self.BASE + "Header.java", err.getvalue())
                self.assertIn("python3 clients/generate.py jvm", err.getvalue())
            finally:
                gen.ROOT, gen.HEADER = root, header



class DotnetPackage(unittest.TestCase):
    """The .NET package, `clients/dotnet` (clients/dotnet/README.md; ADR 0046, card 209)."""

    FILES = sorted(["Cavalry.csproj", "Abi.cs", "CavalryException.cs", "Client.cs", "CommittedLog.cs", "FragmentHandler.cs",
                    "HandlerException.cs", "Header.cs", "Image.cs", "Publication.cs", "Recording.cs", "Replay.cs",
                    "Subscription.cs"])

    @classmethod
    def setUpClass(cls):
        with open(gen.HEADER, encoding="utf-8") as f:
            cls.text = f.read()
        cls.header = gen.parse(cls.text)

    def committed(self, name):
        with open(os.path.join(gen.ROOT, "clients", "dotnet", name), encoding="utf-8") as f:
            return f.read()

    def test_the_committed_package_is_what_the_header_gives(self):
        files = gen_dotnet.write_dotnet(self.header)
        self.assertEqual(sorted(files), self.FILES)
        for name, content in files.items():
            self.assertEqual(self.committed(name), content, name)
        self.assertIs(gen.WRITERS["dotnet"], gen_dotnet.write_dotnet)
        self.assertEqual(gen.LANGUAGES["dotnet"], "dotnet")

    def test_the_committed_log_reads_both_reports_and_closes_a_log_it_cannot_return(self):
        """The .NET committed log (ADR 0070): `FailedAt` and `WithdrawnAt` each answer the
        position the ABI wrote when it answered true and null when it answered false; and
        a log whose constants cannot be read is closed, as the Go client closes one it
        cannot return, before its `GCHandle` is freed and the exception goes on."""
        log = gen_dotnet.write_dotnet(self.header)["CommittedLog.cs"]
        for report, abi in (("FailedAt", "cavalry_committed_log_failed_at"),
                            ("WithdrawnAt", "cavalry_committed_log_withdrawn_at")):
            body = log.split(f"    public long? {report}()\n    {{\n", 1)[1].split("\n    }\n", 1)[0]
            self.assertEqual(body.strip(), f"return Abi.{abi}(handle, out long position) ? position : null;")
        # The third report, the lasting read of the image's fault (ADR 0080).
        faulted = log.split("    public Subscription.Fault? Faulted()\n    {\n", 1)[1].split("\n    }\n", 1)[0]
        self.assertLess(faulted.index("if (!Abi.cavalry_committed_log_faulted(handle, ref f))"),
                        faulted.index("return null;"))
        self.assertIn("return new Subscription.Fault(f.code, f.session_id, f.position);", faulted)
        opened = log.split("    internal CommittedLog(nint handle, Client client)\n    {\n", 1)[1].split("\n    }\n", 1)[0]
        caught = opened.split("        catch\n        {\n", 1)[1].split("\n        }\n", 1)[0]
        closed = caught.index("client.CloseCommittedLog(handle);")
        self.assertLess(opened.index('Abi.Check("cavalry_committed_log_constants"'), opened.index("        catch\n"))
        # The close is in a try of its own, so the GCHandle is freed and the constructor's
        # exception goes on whatever the close does.
        self.assertLess(caught.index("            try\n"), closed)
        self.assertLess(closed, caught.index("            catch\n"))
        self.assertLess(caught.index("            catch\n"), caught.index("self.Free();"))
        self.assertLess(caught.index("self.Free();"), caught.index("throw;"))

    def test_a_header_change_changes_the_constants(self):
        changed = gen.parse(self.text.replace("#define CAVALRY_MAX_IMAGES 256", "#define CAVALRY_MAX_IMAGES 512"))
        code = gen_dotnet.write_dotnet(changed)["Abi.cs"]
        self.assertIn("    public const int MAX_IMAGES = 512; // CAVALRY_MAX_IMAGES\n", code)
        self.assertNotEqual(code, self.committed("Abi.cs"))

    def test_every_define_is_a_constant_under_its_name(self):
        code = gen_dotnet.write_dotnet(self.header)["Abi.cs"]
        for name, _ in self.header.defines:
            if name != "CAVALRY_H":
                self.assertRegex(code, r"    public const \w+ " + gen_dotnet.cs_name(name) + r" = \S.*; // " + name + r"\n")
        self.assertEqual(gen_dotnet.cs_name("CAVALRY_ERR_POLLING"), "ERR_POLLING")
        self.assertEqual(gen_dotnet.cs_constant("INT64_MAX"), ("long", "long.MaxValue"))
        self.assertEqual(gen_dotnet.cs_constant("(-1)"), ("int", "-1"))

    def test_the_binding_reads_the_abi_version_before_any_other_function(self):
        """The static constructor resolves the library's path, installs the resolver and checks
        the version, in that order, before any other function can run (spec/c-abi.md,
        "Versions and what a reader rejects"); a header without the check's names cannot be
        generated."""
        code = gen_dotnet.write_dotnet(self.header)["Abi.cs"]
        block = code.split("    static Abi()\n    {\n", 1)[1].split("\n    }\n", 1)[0]
        found = block.index("LibraryPath = FindLibrary();")
        resolver = block.index("NativeLibrary.SetDllImportResolver(")
        check = block.index("CheckAbiVersion(ABI_VERSION);")
        self.assertLess(found, resolver)
        self.assertLess(resolver, check)
        self.assertIn("ERR_ABI_VERSION", code)
        self.assertIn("docs/reference/c-abi.md#versions-and-what-a-reader-rejects", code)
        without = gen.parse(self.text.replace("#define CAVALRY_ABI_VERSION ", "#define CAVALRY_ABI_VERSION_GONE "))
        with self.assertRaises(SystemExit) as refused:
            gen_dotnet.write_dotnet(without)
        self.assertIn("CAVALRY_ABI_VERSION", str(refused.exception))

    def test_every_function_is_a_library_import_with_blittable_parameters(self):
        files = gen_dotnet.write_dotnet(self.header)
        abi, subscription = files["Abi.cs"], files["Subscription.cs"]
        self.assertEqual(abi.count("[LibraryImport("), len(self.header.functions))
        for fn in self.header.functions:
            self.assertIn(f" {fn.name}(", abi)
        self.assertIn("internal static partial long cavalry_publication_try_claim(nint publication, nuint length, ref cavalry_buffer_claim_t claim);", abi)
        self.assertIn("internal static partial int cavalry_subscription_poll(nint subscription, " + gen_dotnet.CALLBACK
                      + " handler, nint clientd, nuint fragment_limit);", abi)
        self.assertIn("internal static partial long cavalry_publication_offer(nint publication, byte* buffer, nuint length);", abi)
        self.assertRegex(abi, r"\[LibraryImport\(Library, StringMarshalling = StringMarshalling\.Utf8\)\]\n"
                              r"    internal static partial int cavalry_add_publication\(out nint publication, nint client, string channel, int stream_id\);")
        self.assertIn("[return: MarshalAs(UnmanagedType.U1)]\n    internal static partial bool cavalry_is_closed(nint client);", abi)
        self.assertNotIn("MarshalAs", abi.split("public struct", 1)[1].split("[LibraryImport", 1)[0])  # the structs are blittable
        self.assertEqual(subscription.count("[UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]"), 1)
        self.assertEqual(subscription.count("Abi.cavalry_subscription_poll(handle, &OnFragment, GCHandle.ToIntPtr(self), (nuint) fragmentLimit)"), 2)

    def test_the_publications_close_clears_the_claim_struct(self):
        """A close leaves no claim, so the struct's `publication` must not name the freed
        handle: the close clears the struct before the ABI's close, and the claim's calls
        after it answer ERR_NO_CLAIM through the null-publication branch (spec/c-abi.md,
        "Which claim a struct names"); the runner checks the answers, this checks the clear."""
        code = gen_dotnet.write_dotnet(self.header)["Publication.cs"]
        body = code.split("    public void Close()\n    {\n", 1)[1].split("\n    }\n", 1)[0]
        self.assertIn("claim = default;", body)
        self.assertLess(body.index("claim = default;"), body.index("Abi.cavalry_publication_close(closing)"))

    def test_handler_exceptions_message_says_what_to_do_and_names_a_heading_of_the_readme(self):
        files = gen_dotnet.write_dotnet(self.header)
        code = files["HandlerException.cs"]
        self.assertIn("    public HandlerException(int fragments, int raised, Exception cause, bool committedLog = false)\n"
                      "        : base(Describe(fragments, raised, cause, committedLog), cause)\n", code)
        body = code.split("private static string Describe(int fragments, int raised, Exception cause, bool committedLog)",
                          1)[1].split("\n    }\n", 1)[0]
        self.assertIn("fragment(s) this poll delivered", body)
        self.assertIn("before it stopped on a fault, which Subscription.GetFault() reads", body)
        self.assertIn("    public Fault GetFault()\n", files["Subscription.cs"])
        self.assertIn("Fix the handler: its first exception, ", body)
        self.assertIn("See clients/dotnet/README.md#the-exception", body)
        # A committed log's poll names its own GetFault() and its entries: the fault of the
        # log's image is reported once, so the message must not send the reader to a
        # subscription's (clients/dotnet/README.md, "The committed log").
        self.assertIn('string one = committedLog ? "entry" : "fragment";', body)
        self.assertIn("which this CommittedLog's GetFault() reads\"\n                    + \" until its next poll, and its "
                      "Faulted() from then on when the fault is the log's own image's\"", body)
        self.assertIn("    public Subscription.Fault GetFault()\n", files["CommittedLog.cs"])
        self.assertIn("throw new HandlerException(count, raised, cause, committedLog: true);", files["CommittedLog.cs"])
        self.assertIn("throw new HandlerException(count, raised, cause);", files["Subscription.cs"])
        with open(os.path.join(gen.ROOT, "clients", "dotnet", "README.md"), encoding="utf-8") as f:
            slugs = {re.sub(r"[^a-z0-9 -]", "", line.lstrip("#").strip().lower()).replace(" ", "-")
                     for line in f if line.startswith("#")}
        self.assertIn("the-exception", slugs)

    def test_the_generated_project_builds_with_every_warning_an_error_when_dotnet_10_is_on_the_path(self):
        """The README builds the client with `dotnet build`, whose project makes every warning
        an error; without a .NET SDK of release 10 or later on the path, as on a machine that
        builds no client, the check is CI's (`just conformance`)."""
        dotnet = shutil.which("dotnet")
        version = subprocess.run([dotnet, "--version"], capture_output=True, text=True) if dotnet else None
        found = re.match(r"(\d+)\.", version.stdout.strip()) if version and version.returncode == 0 else None
        if found is None or int(found.group(1)) < 10:
            print("generate-test: no .NET SDK of release 10 or later on the path; the build is checked in CI")
            return
        with tempfile.TemporaryDirectory() as tmp:
            for name, content in gen_dotnet.write_dotnet(self.header).items():
                with open(os.path.join(tmp, name), "w", encoding="utf-8") as f:
                    f.write(content)
            env = dict(os.environ, DOTNET_CLI_TELEMETRY_OPTOUT="1", DOTNET_NOLOGO="1", DOTNET_SKIP_FIRST_TIME_EXPERIENCE="1")
            built = subprocess.run([dotnet, "build", "-c", "Release", os.path.join(tmp, "Cavalry.csproj")],
                                   capture_output=True, text=True, env=env)
            self.assertEqual(built.returncode, 0, built.stdout + built.stderr)

    def test_check_fails_on_a_package_that_differs_and_says_what_to_do(self):
        with tempfile.TemporaryDirectory() as tmp:
            shutil.copytree(os.path.join(gen.ROOT, "clients", "dotnet"), os.path.join(tmp, "clients", "dotnet"),
                            ignore=shutil.ignore_patterns("bin", "obj"))
            os.makedirs(os.path.join(tmp, "crates", "cavalry-ffi", "include"))
            shutil.copy(gen.HEADER, os.path.join(tmp, "crates", "cavalry-ffi", "include", "cavalry.h"))
            root, header = gen.ROOT, gen.HEADER
            gen.ROOT = tmp
            gen.HEADER = os.path.join(tmp, "crates", "cavalry-ffi", "include", "cavalry.h")
            try:
                self.assertEqual(gen.main(["generate.py", "dotnet", "--check"]), 0)
                with open(os.path.join(tmp, "clients", "dotnet", "Header.cs"), "a", encoding="utf-8") as f:
                    f.write("// edited by hand\n")
                import io
                import contextlib
                err = io.StringIO()
                with contextlib.redirect_stderr(err):
                    self.assertEqual(gen.main(["generate.py", "dotnet", "--check"]), 1)
                self.assertIn("differs from what the header gives in Header.cs", err.getvalue())
                self.assertIn("python3 clients/generate.py dotnet", err.getvalue())
            finally:
                gen.ROOT, gen.HEADER = root, header


if __name__ == "__main__":
    unittest.main()
