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
standard library only.
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
        self.assertEqual(len(self.header.functions), 58)

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
        """The seven functions `crates/cavalry-ffi/tests/allocations.rs` counts are the ones
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
# state kept; it prints each finding and the clean run's calls, and exits 1 on a finding.
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

static const char *calls[64];
static int ncalls, fail_at = -1;
static const napi_status fail_with = napi_generic_failure;
static bool handler_throws, pending;
static int handler_ran, scopes_open, buffers_live;
static char handles[8];
#define HANDLE(i) ((void *) &handles[i])

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
  (void) env; (void) object; (void) index; (void) value;
  return step("napi_set_element");
}
napi_status napi_get_undefined(napi_env env, napi_value *result) {
  (void) env;
  napi_status s = step("napi_get_undefined");
  if (s == napi_ok) *result = HANDLE(4);
  return s;
}
napi_status napi_call_function(napi_env env, napi_value recv, napi_value func, size_t argc, const napi_value *argv,
                               napi_value *result) {
  (void) env; (void) recv; (void) func; (void) argc; (void) argv;
  napi_status s = step("napi_call_function");
  if (s != napi_ok) return s;
  handler_ran++;
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
  handler_ran = 0; scopes_open = 0; buffers_live = 0;
}

static void fragment(subscription_state_t *s) {
  trampoline(s, (const uint8_t *) "body", 4, (const cavalry_header_t *) HANDLE(1));
}

static void one(int at, bool throws) {
  subscription_state_t s;
  memset(&s, 0, sizeof s);
  s.env = (napi_env) HANDLE(0);
  reset(at, throws);
  fragment(&s);
  const char *failed = at < ncalls ? calls[at] : NULL;
  char where[160];
  snprintf(where, sizeof where, "with %s failing (call %d), the handler %s", failed ? failed : "no call", at,
           throws ? "throwing" : "returning");
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
  }
  EXPECT(scopes_open == 0, "%s: %d handle scope(s) left open", where, scopes_open);
  if (failed == NULL || (strcmp(failed, "napi_get_typedarray_info") != 0 && strcmp(failed, "napi_detach_arraybuffer") != 0)) {
    EXPECT(buffers_live == 0, "%s: %d body Buffer(s) left attached", where, buffers_live);
  }
}

int main(void) {
  for (int throws = 0; throws < 2; throws++) {
    subscription_state_t s;
    memset(&s, 0, sizeof s);
    reset(-1, throws);
    fragment(&s);
    int n = ncalls;
    printf("a fragment whose handler %s calls:", throws ? "throws" : "returns");
    for (int i = 0; i < n; i++) printf(" %s", calls[i]);
    printf("\n");
    for (int at = 0; at <= n; at++) one(at, throws);
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
  (void) status_names;
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

    def test_a_header_change_changes_the_addon_and_the_client(self):
        changed = gen.parse(self.text.replace("#define CAVALRY_MAX_IMAGES 256", "#define CAVALRY_MAX_IMAGES 512"))
        files = gen_node.write_node(changed)
        self.assertIn("  CAVALRY_MAX_IMAGES: 512,", files["index.js"])
        self.assertNotEqual(files["index.js"], self.committed("index.js"))
        changed.functions = [fn for fn in changed.functions if fn.name != "cavalry_version_patch"]
        addon = gen_node.write_node(changed)["src/addon.c"]
        self.assertNotIn("fn_cavalry_version_patch", addon)
        self.assertIn("fn_cavalry_version_minor", addon)

    def test_the_addon_wraps_every_function_and_the_poll_is_its_own(self):
        """One N-API function per ABI function, registered under the ABI's name, except the
        poll: the addon's `poll` makes the one `cavalry_subscription_poll` call with its
        trampoline, and the header's fields are read through `headerField`."""
        addon = gen_node.write_node(self.header)["src/addon.c"]
        for fn in self.header.functions:
            if fn.name == "cavalry_subscription_poll":
                self.assertNotIn("fn_cavalry_subscription_poll", addon)
                continue
            self.assertIn(f"static napi_value fn_{fn.name}(napi_env env, napi_callback_info info)", addon)
            self.assertIn(f'{{ "{fn.name}", NULL, fn_{fn.name}, NULL, NULL, NULL, napi_default, NULL }},', addon)
        self.assertEqual(addon.count("cavalry_subscription_poll("), 2)  # the nested refusal and the poll itself
        for own in ("registerSubscription", "poll", "headerField", "view", "detach", "stringAt"):
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
        self.assertIn("never reached the fragment handler", body)
        self.assertIn("The first failure, ${describe(cause)}, is this ` +\n      `error's cause and says what to do.", body)
        self.assertIn("super(`${what} See clients/node/README.md#the-exception`, { cause });", body)
        failure = files["src/addon.c"].split("static napi_value failure_error(", 1)[1].split("\n}\n", 1)[0]
        self.assertIn("run the client on a Node built", failure)
        self.assertIn("polling again does not deliver it. See clients/node/README.md#the-exception", failure)
        with open(os.path.join(gen.ROOT, "clients", "node", "README.md"), encoding="utf-8") as f:
            slugs = {re.sub(r"[^a-z0-9 -]", "", line.lstrip("#").strip().lower()).replace(" ", "-")
                     for line in f if line.startswith("#")}
        self.assertIn("the-exception", slugs)

    def test_the_trampoline_counts_and_reports_every_fragment_the_handler_did_not_see(self):
        """Invariant 1: the ABI has consumed a fragment before the trampoline runs, so a
        fragment the handler does not see, because N-API refused a step of the call, is
        counted and its failure kept for poll to report, never dropped. The generated
        trampoline and its helpers are compiled with the runner's `cc` against
        TRAMPOLINE_HARNESS, a stand-in for N-API that fails each call of a fragment in turn,
        for a handler that returns and one that throws: a fragment the handler returned from
        is counted nowhere, every other is counted once, in `raised` or `undelivered`, with
        the first failure naming the call that failed, its status and the frame; every handle
        scope opened is closed and every body made is detached; and a later fragment does not
        replace the poll's first failure."""
        cc = shutil.which("cc")
        if cc is None:
            self.skipTest("no C compiler on PATH; the Node client's install needs one, and both CI runners have it")
        addon = gen_node.write_node(self.header)["src/addon.c"]
        start = addon.index("/* N-API's statuses by number")
        brace = addon.index("{", addon.index("static void trampoline("))
        end = lint.block_end(lint.blank(addon), brace)
        with tempfile.TemporaryDirectory() as tmp:
            source = os.path.join(tmp, "harness.c")
            with open(source, "w", encoding="utf-8") as f:
                f.write(TRAMPOLINE_HARNESS.replace("@TRAMPOLINE@", addon[start:end]))
            binary = os.path.join(tmp, "harness")
            include = os.path.join(gen.ROOT, "crates", "cavalry-ffi", "include")
            built = subprocess.run([cc, "-std=c11", "-Wall", "-Wextra", "-I", include, source, "-o", binary],
                                   capture_output=True, text=True)
            self.assertEqual(built.returncode, 0, built.stderr)
            ran = subprocess.run([binary], capture_output=True, text=True)
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertIn("napi_create_external_buffer", ran.stdout)
        self.assertIn("napi_call_function", ran.stdout)
        self.assertIn("0 failure(s)", ran.stdout)

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


if __name__ == "__main__":
    unittest.main()
