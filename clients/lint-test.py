#!/usr/bin/env python3
"""The tests of clients/lint.py (ADR 0026): the committed Python package passes; a generated
per-message function carrying each forbidden form, a `threading` lock, `async def`, `await`
or `raise`, fails naming the function and the form; so does one that calls the package's
own raising function, `_check`, `_error` or a raising method, and the wrappers of the
reserved value and the header's values with its properties are read as the others are; a
raise in `poll` after the ABI's last call passes, and one before it or between its two
calls fails; a package with none of the per-message functions fails; a language with no
rules fails. The same for the Node client's `index.js`: a per-message function that is
`async`, or that awaits, throws, builds `HandlerError`, uses `Promise` or `.then`, calls
`Atomics.wait` or calls the module's throwing `check`, fails naming the function and the
form; a throw in `poll` after the addon's `poll` call passes and one before it fails; a
header getter that throws fails; a module with none of the per-message functions fails,
and a missing `index.js` says to generate. The same for the Node client's addon,
`src/addon.c`: one test per C form, `pthread_mutex_*` in the trampoline,
`pthread_rwlock_*` in the view, C11's `mtx_*` in the poll, libuv's `uv_mutex_*` in a helper
the trampoline calls, `uv_rwlock_*` in the header's field, an N-API thread-safe function in
the offer, `napi_throw*` in the commit and `napi_fatal_*` in the Buffers' finalizer, each
failing and naming the function and the form; an argument reader's throw passes and the
same throw in a helper the poll calls fails; a missing per-message function, an addon with
none, and a missing addon fail. `just test` runs this through `test-scripts`. The same for the Go client's `cavalry.go` and
`trampoline.go`: a per-message function that starts a goroutine, sends or receives on a
channel, uses `sync`, takes a lock through a variable or a field (`registryMu.Lock()`,
and `.Lock()`, `.RLock()`, `.TryLock()` and `.TryRLock()` on a field), waits with
`.Wait()`, panics, allocates with `make`, uses `select`, or calls the package's
`unregister`, which takes `registryMu`, or its `lastError`, which builds an `*Error`,
fails naming the function and the form;
building the `*HandlerError` in `Poll` after the shim's poll call passes and before it
fails; a panic in the exported trampoline fails naming `trampoline.go`; a lock in
`shim.c`'s trampoline fails, and one in the helper that keeps a per-message call's error
fails each shim call that keeps one; a function that reads the thread's error through
`check` or `lastError` without `runtime.LockOSThread()` and its deferred unlock before
its first ABI call fails, whether it lost the lock, takes it after the call, has no
deferred unlock or is new; a module with none of the per-message functions fails, and a
missing `cavalry.go` says to generate. The same for
the JVM client's package under `src/com/appshapes/cavalry`: a per-message method that is
`synchronized` or holds a `synchronized` block, takes a lock on any receiver, uses a lock
class of `java.util.concurrent`, a future or a blocking wait, starts a thread, throws
outside the compiler's `catch (Throwable)`, builds an exception, allocates with `new`, or
calls the package's throwing `Abi.check` or `Abi.lastError`, fails naming the method and
the form; a throw in the trampoline's catch, which holds no `invokeExact`, fails; building
the `HandlerException` in `poll` before its ABI call fails; a header getter that throws
fails while `Header.ensure`'s own throw, the guard, passes; a package with none of the
per-message methods fails, one method renamed fails, and a missing file says to generate. The same for
the .NET client's package: a per-message member that uses `lock`, `Monitor` or
`System.Threading.Lock`, is `async`, awaits, uses a task, blocks, starts a thread, throws,
allocates with `new` or an interpolated string, or calls the package's throwing `Abi.Check`
or `Abi.LastError`, fails naming the member and the form; the `Span` the claim's getter
makes over the ABI's bytes passes while a throw in that getter fails; a throw in the
trampoline's catch fails; building the `HandlerException` in `Poll` before its ABI call
fails; a header getter that throws fails while `Header.Ensure`'s own throw, the guard,
passes; a package with none of the per-message members fails, one member renamed fails,
and a missing file says to generate.
"""

import os
import shutil
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lint  # noqa: E402

PACKAGE = os.path.join(lint.CLIENTS, "python", "cavalry", "__init__.py")


class Clients(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp)
        shutil.copytree(os.path.join(lint.CLIENTS, "python"), os.path.join(self.tmp, "python"))
        self.file = os.path.join(self.tmp, "python", "cavalry", "__init__.py")
        shutil.copytree(os.path.join(lint.CLIENTS, "node"), os.path.join(self.tmp, "node"),
                        ignore=shutil.ignore_patterns("build", "node_modules", "package-lock.json"))
        self.node = os.path.join(self.tmp, "node", "index.js")
        shutil.copytree(os.path.join(lint.CLIENTS, "go"), os.path.join(self.tmp, "go"))
        self.go = os.path.join(self.tmp, "go", "cavalry.go")
        self.go_trampoline = os.path.join(self.tmp, "go", "trampoline.go")
        self.go_shim = os.path.join(self.tmp, "go", "shim.c")
        self.addon = os.path.join(self.tmp, "node", "src", "addon.c")
        shutil.copytree(os.path.join(lint.CLIENTS, "jvm"), os.path.join(self.tmp, "jvm"),
                        ignore=shutil.ignore_patterns("build"))
        self.jvm = os.path.join(self.tmp, "jvm", "src", "com", "appshapes", "cavalry")
        shutil.copytree(os.path.join(lint.CLIENTS, "dotnet"), os.path.join(self.tmp, "dotnet"),
                        ignore=shutil.ignore_patterns("bin", "obj"))
        self.dotnet = os.path.join(self.tmp, "dotnet")

    def edit(self, old, new, count=1, file=None):
        file = file or self.file
        with open(file, encoding="utf-8") as f:
            text = f.read()
        self.assertEqual(text.count(old), count, old)
        with open(file, "w", encoding="utf-8") as f:
            f.write(text.replace(old, new))

    def edit_node(self, old, new, count=1):
        self.edit(old, new, count, file=self.node)

    def edit_go(self, old, new, count=1, file=None):
        self.edit(old, new, count, file=file or self.go)
    def edit_jvm(self, old, new, count=1, file="Publication.java"):
        self.edit(old, new, count, file=os.path.join(self.jvm, file))

    def edit_dotnet(self, old, new, count=1, file="Publication.cs"):
        self.edit(old, new, count, file=os.path.join(self.dotnet, file))

    def add_to_addon(self, function, line):
        """`line` as the first statement of the addon's C function `function`."""
        opening = f"static napi_value {function}(napi_env env, napi_callback_info info) {{\n"
        if function in ("trampoline", "buffer_freed", "keep_failure", "report"):
            opening = {"trampoline": "  subscription_state_t *s = clientd;\n",
                       "buffer_freed": "  (void) env; (void) data; (void) hint;\n",
                       "keep_failure": "  if (s->first != FIRST_NONE) return;\n  s->first = FIRST_FAILURE;\n",
                       "report": "  napi_value tuple, first = NULL, holder = NULL;\n"}[function]
        self.edit(opening, opening + f"  {line}\n", file=self.addon)

    def assert_addon_finding(self, function, form):
        found = lint.findings(self.tmp)
        self.assertTrue(any(f.startswith(f"node: {function} (") and form in f for f in found), found)
        return found

    def test_the_committed_package_passes(self):
        self.assertEqual(lint.findings(lint.CLIENTS), [])

    def test_a_lock_in_a_per_message_function_fails(self):
        self.edit("    def offer(self, message):\n", "    def offer(self, message):\n        with threading.Lock():\n            pass\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.offer", finding)
        self.assertIn("uses `threading`", finding)

    def test_an_async_per_message_function_fails(self):
        self.edit("    def try_claim(self, length):", "    async def try_claim(self, length):")
        found = lint.findings(self.tmp)
        self.assertTrue(any("Publication.try_claim" in f and "`async def`" in f for f in found), found)

    def test_an_await_fails(self):
        self.edit("    def commit(self):\n", "    async def commit(self):\n        await nothing()\n")
        found = lint.findings(self.tmp)
        self.assertTrue(any("Publication.commit" in f and "awaits" in f for f in found), found)

    def test_a_raise_in_a_per_message_function_fails(self):
        self.edit("    def abort(self):\n", "    def abort(self):\n        raise RuntimeError('no')\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.abort", finding)
        self.assertIn("raises", finding)

    def test_the_handlers_raise_after_the_poll_passes_and_before_it_fails(self):
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit("        self._handler = handler\n", "        self._handler = handler\n        raise RuntimeError('too early')\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription.poll", finding)
        self.assertIn("raises", finding)

    def test_a_raise_between_polls_two_abi_calls_fails(self):
        """`poll` calls the ABI on two lines, the refused nested poll and its own; the raise
        allowed is after the last of them."""
        self.edit("        try:\n            fragments = self._abi.cavalry_subscription_poll(",
                  "        raise RuntimeError('between')\n        try:\n"
                  "            fragments = self._abi.cavalry_subscription_poll(")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription.poll", finding)
        self.assertIn("raises", finding)

    def test_a_raise_in_the_trampoline_fails(self):
        self.edit("            self._keep(exc)\n", "            self._keep(exc)\n            raise\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription._on_fragment", finding)

    def test_a_raise_in_the_helper_the_trampoline_calls_fails(self):
        self.edit("        self._raised += 1\n", "        self._raised += 1\n        raise exc\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription._on_fragment", finding)
        self.assertIn("calls Subscription._keep", finding)

    def test_a_raise_in_set_reserved_value_fails(self):
        self.edit("    def set_reserved_value(self, value):\n",
                  "    def set_reserved_value(self, value):\n        raise RuntimeError('no')\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.set_reserved_value", finding)
        self.assertIn("raises", finding)

    def test_a_call_to_check_fails(self):
        self.edit("    def offer(self, message):\n",
                  "    def offer(self, message):\n        _check(self._abi, 'cavalry_publication_offer', -1)\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.offer", finding)
        self.assertIn("calls _check", finding)
        self.assertIn("_check raises at line", finding)

    def test_a_call_to_error_fails(self):
        self.edit("    def abort(self):\n", "    def abort(self):\n        _error(self._abi, 'cavalry_buffer_claim_abort')\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.abort", finding)
        self.assertIn("calls _error", finding)
        self.assertIn("_error builds CavalryError", finding)

    def test_a_call_to_a_raising_method_fails(self):
        self.edit("    def commit(self):\n", "    def commit(self):\n        self.position()\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.commit", finding)
        self.assertIn("calls Publication.position", finding)

    def test_header_values_raising_through_check_fails_and_so_do_its_properties(self):
        self.edit("        if self._abi.cavalry_header_values(self._pointer, self._values_ref) != 0:\n"
                  "            return None\n",
                  "        _check(self._abi, 'cavalry_header_values',"
                  " self._abi.cavalry_header_values(self._pointer, self._values_ref))\n")
        found = lint.findings(self.tmp)
        self.assertTrue(any(f.startswith("python: Header.values ") and "calls _check" in f for f in found), found)
        self.assertTrue(any(f.startswith("python: Header.session_id ") and "calls Header.values" in f for f in found),
                        found)
        self.assertEqual(len(found), 10, found)  # values and its nine properties

    def test_a_package_without_the_per_message_functions_fails(self):
        for name in ("try_claim", "set_reserved_value", "offer", "commit", "abort", "poll", "_on_fragment", "values",
                     "session_id", "stream_id", "term_id", "term_offset", "frame_length", "flags", "type", "version",
                     "reserved_value"):
            self.edit(f"    def {name}(self", f"    def renamed_{name}(self")
        [finding] = lint.findings(self.tmp)
        self.assertIn("none of the per-message functions", finding)

    def test_one_missing_per_message_function_fails(self):
        self.edit("    def offer(self, message):", "    def send(self, message):")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.offer is not in", finding)

    def test_a_missing_header_values_fails(self):
        self.edit("    def values(self):", "    def read(self):")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Header.values is not in", finding)


    # The Node client's index.js, read by the tokenizer (clients/node/README.md).

    def test_an_async_node_per_message_function_fails(self):
        self.edit_node("  tryClaim(length) {\n", "  async tryClaim(length) {\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.tryClaim", finding)
        self.assertIn("is `async`", finding)

    def test_an_await_in_node_fails(self):
        self.edit_node("  commit() { this._release(); return addon.cavalry_buffer_claim_commit(this._claim.buffer); }\n",
                       "  async commit() { await nothing(); this._release(); "
                       "return addon.cavalry_buffer_claim_commit(this._claim.buffer); }\n")
        found = lint.findings(self.tmp)
        self.assertTrue(any("Publication.commit" in f and "awaits" in f for f in found), found)
        self.assertTrue(any("Publication.commit" in f and "is `async`" in f for f in found), found)

    def test_a_throw_in_node_fails(self):
        self.edit_node("  abort() { this._release(); return addon.cavalry_buffer_claim_abort(this._claim.buffer); }\n",
                       "  abort() { throw new Error(\"no\"); }\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.abort", finding)
        self.assertIn("throws", finding)

    def test_a_promise_in_node_fails(self):
        self.edit_node("  offer(message) { return addon.cavalry_publication_offer(this.handle, message); }\n",
                       "  offer(message) { return Promise.resolve(addon.cavalry_publication_offer(this.handle, message)); }\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.offer", finding)
        self.assertIn("uses `Promise`", finding)

    def test_a_then_in_node_fails(self):
        self.edit_node("  offer(message) { return addon.cavalry_publication_offer(this.handle, message); }\n",
                       "  offer(message) { return later(message).then((p) => p); }\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.offer", finding)
        self.assertIn("calls `.then`", finding)

    def test_atomics_wait_in_node_fails(self):
        self.edit_node("  setReservedValue(value) { return addon.cavalry_buffer_claim_set_reserved_value(this._claim.buffer, value); }\n",
                       "  setReservedValue(value) { Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 1); "
                       "return addon.cavalry_buffer_claim_set_reserved_value(this._claim.buffer, value); }\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.setReservedValue", finding)
        self.assertIn("calls `Atomics.wait`", finding)

    def test_the_handlers_throw_after_the_polls_addon_call_passes_and_before_it_fails(self):
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit_node("    const result = addon.poll(this._state, this.handle, handler, fragmentLimit);\n",
                       "    if (handler === null) throw new TypeError(\"a handler\");\n"
                       "    const result = addon.poll(this._state, this.handle, handler, fragmentLimit);\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription.poll", finding)
        self.assertIn("throws", finding)

    def test_building_handler_error_before_the_polls_addon_call_fails(self):
        self.edit_node("    const result = addon.poll(this._state, this.handle, handler, fragmentLimit);\n",
                       "    const early = new HandlerError(0, 0, null);\n"
                       "    const result = addon.poll(this._state, this.handle, handler, fragmentLimit);\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription.poll", finding)
        self.assertIn("builds HandlerError", finding)

    def test_a_call_to_check_in_node_fails(self):
        self.edit_node("  tryClaim(length) {\n", "  tryClaim(length) {\n    check(\"cavalry_publication_try_claim\", 0);\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.tryClaim", finding)
        self.assertIn("calls check", finding)
        self.assertIn("throws", finding)

    def test_a_throw_in_a_header_getter_fails(self):
        self.edit_node("  get sessionId() { return addon.headerField(this._state, 5); }\n",
                       "  get sessionId() { throw new Error(\"no\"); }\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Header.sessionId", finding)
        self.assertIn("throws", finding)

    def test_a_node_module_without_the_per_message_functions_fails(self):
        with open(self.node, "w", encoding="utf-8") as f:
            f.write("\"use strict\";\nclass Nothing {\n  here() { return 1; }\n}\nmodule.exports = { Nothing };\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("none of the per-message functions", finding)
        self.assertIn("Publication.tryClaim", finding)

    def test_one_missing_node_per_message_function_fails(self):
        self.edit_node("  get reservedValue() { return addon.headerField(this._state, 8); }\n",
                       "  get reserved() { return addon.headerField(this._state, 8); }\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Header.reservedValue is not in", finding)

    def test_a_missing_node_index_says_to_generate(self):
        os.remove(self.node)
        [finding] = lint.findings(self.tmp)
        self.assertIn("index.js is missing", finding)
        self.assertIn("python3 clients/generate.py node", finding)

    # The Node client's addon, src/addon.c, read by the same tokenizer for C's forms.

    def test_a_pthread_mutex_in_the_trampoline_fails(self):
        self.add_to_addon("trampoline", "pthread_mutex_lock(&lock);")
        [finding] = lint.findings(self.tmp)
        self.assertIn("node: trampoline (", finding)
        self.assertIn("calls `pthread_mutex_*`", finding)

    def test_a_pthread_rwlock_in_the_view_fails(self):
        self.add_to_addon("fn_view", "pthread_rwlock_rdlock(&lock);")
        [finding] = lint.findings(self.tmp)
        self.assertIn("node: fn_view (", finding)
        self.assertIn("calls `pthread_rwlock_*`", finding)

    def test_a_c11_mtx_in_the_poll_fails(self):
        self.add_to_addon("fn_poll", "mtx_lock(&lock);")
        [finding] = lint.findings(self.tmp)
        self.assertIn("node: fn_poll (", finding)
        self.assertIn("calls C11's `mtx_*`", finding)

    def test_a_uv_mutex_in_a_helper_the_trampoline_calls_fails(self):
        self.add_to_addon("keep_failure", "uv_mutex_lock(&lock);")
        self.assert_addon_finding("trampoline", "calls keep_failure")
        self.assert_addon_finding("trampoline", "keep_failure calls libuv's `uv_mutex_*`")

    def test_a_uv_rwlock_in_the_header_field_fails(self):
        self.add_to_addon("fn_headerField", "uv_rwlock_rdlock(&lock);")
        [finding] = lint.findings(self.tmp)
        self.assertIn("node: fn_headerField (", finding)
        self.assertIn("calls libuv's `uv_rwlock_*`", finding)

    def test_a_threadsafe_function_in_the_offer_fails(self):
        self.add_to_addon("fn_cavalry_publication_offer", "napi_call_threadsafe_function(tsfn, NULL, napi_tsfn_blocking);")
        [finding] = lint.findings(self.tmp)
        self.assertIn("node: fn_cavalry_publication_offer (", finding)
        self.assertIn("`napi_*threadsafe_function*`", finding)

    def test_a_throw_in_the_commit_fails(self):
        self.add_to_addon("fn_cavalry_buffer_claim_commit", "napi_throw_error(env, NULL, \"no\");")
        [finding] = lint.findings(self.tmp)
        self.assertIn("node: fn_cavalry_buffer_claim_commit (", finding)
        self.assertIn("throws, `napi_throw*`", finding)

    def test_a_fatal_error_in_the_buffers_finalizer_fails(self):
        self.add_to_addon("buffer_freed", "napi_fatal_error(\"here\", NAPI_AUTO_LENGTH, \"no\", NAPI_AUTO_LENGTH);")
        [finding] = lint.findings(self.tmp)
        self.assertIn("node: buffer_freed (", finding)
        self.assertIn("calls `napi_fatal_*`", finding)

    def test_an_argument_readers_throw_passes_and_the_same_throw_elsewhere_fails(self):
        """The readers throw a TypeError for an argument of the wrong type, and the committed
        addon passes with them; the same throw in report, which poll calls once the ABI has
        returned and which reads no argument, is a raise on the path."""
        self.assertEqual(lint.findings(self.tmp), [])
        self.add_to_addon("report", "napi_throw_type_error(env, NULL, \"no\");")
        self.assert_addon_finding("fn_poll", "calls report")
        self.assert_addon_finding("fn_poll", "report throws, `napi_throw*`")

    def test_a_missing_addon_per_message_function_fails(self):
        self.edit("static napi_value fn_view(", "static napi_value fn_view_renamed(", file=self.addon)
        found = lint.findings(self.tmp)
        self.assertTrue(any("the addon's per-message function fn_view is not in" in f for f in found), found)

    def test_a_missing_addon_says_to_generate(self):
        os.remove(self.addon)
        [finding] = lint.findings(self.tmp)
        self.assertIn("src/addon.c is missing", finding)
        self.assertIn("python3 clients/generate.py node", finding)

    def test_an_addon_without_the_per_message_functions_fails(self):
        with open(self.addon, "w", encoding="utf-8") as f:
            f.write("static int nothing(void) {\n  return 1;\n}\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("none of the addon's per-message functions", finding)
        self.assertIn("trampoline", finding)

    def test_the_tokenizer_blanks_comments_and_strings_and_keeps_lines(self):
        source = "const a = \"th//row\"; // throw\nconst b = `aw${x}ait`; /* await\n */ const c = '\\''; throw a;\n"
        blanked = lint.blank(source)
        self.assertEqual(blanked.count("\n"), source.count("\n"))
        self.assertEqual(len(blanked), len(source))
        self.assertNotIn("//", blanked)
        self.assertNotIn("await", blanked)
        self.assertNotIn("x", blanked)
        self.assertIn("throw a;", blanked)
        self.assertEqual(lint.line_of(blanked, blanked.index("throw a;")), 3)


    # The Go client's cavalry.go and trampoline.go, read by the tokenizer (clients/go/README.md).

    def test_a_goroutine_in_a_go_per_message_function_fails(self):
        self.edit_go("func (p *Publication) TryClaim(length int) int64 {\n",
                     "func (p *Publication) TryClaim(length int) int64 {\n\tgo func() {}()\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.TryClaim", finding)
        self.assertIn("starts a goroutine", finding)

    def test_a_channel_in_go_fails(self):
        self.edit_go("func (p *Publication) Commit() int64 {\n", "func (p *Publication) Commit() int64 {\n\tvar done chan int\n\t<-done\n")
        found = lint.findings(self.tmp)
        self.assertTrue(any("Publication.Commit" in f and "sends or receives on a channel" in f for f in found), found)

    def test_sync_in_go_fails(self):
        self.edit_go("func (p *Publication) Offer(message []byte) int64 {\n",
                     "func (p *Publication) Offer(message []byte) int64 {\n\tvar mu sync.Mutex\n\tmu.Lock()\n")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        self.assertTrue(all("Publication.Offer" in f for f in found), found)
        self.assertTrue(any("uses `sync`" in f for f in found), found)
        self.assertTrue(any("takes a lock with `.Lock()`" in f for f in found), found)

    def test_a_lock_through_a_package_variable_in_go_fails(self):
        """Go takes a lock through a variable or a field, which names no `sync.`."""
        self.edit_go("\ts.handler = handler\n\ts.polling = true\n",
                     "\tregistryMu.Lock()\n\tdefer registryMu.Unlock()\n\ts.handler = handler\n\ts.polling = true\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription.Poll", finding)
        self.assertIn("takes a lock with `.Lock()`", finding)
        self.assertIn("no lock on the per-message path", finding)

    def test_each_lock_method_on_a_field_in_go_fails(self):
        for method in ("Lock", "RLock", "TryLock", "TryRLock"):
            with self.subTest(method=method):
                self.setUp()
                self.edit_go("func (p *Publication) Commit() int64 {\n",
                             f"func (p *Publication) Commit() int64 {{\n\tp.mu.{method}()\n")
                [finding] = lint.findings(self.tmp)
                self.assertIn("Publication.Commit", finding)
                self.assertIn(f"takes a lock with `.{method}()`", finding)

    def test_a_wait_on_a_field_in_go_fails(self):
        self.edit_go("func (p *Publication) Abort() int64 {\n", "func (p *Publication) Abort() int64 {\n\tp.done.Wait()\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.Abort", finding)
        self.assertIn("waits with `.Wait()`", finding)
        self.assertIn("no blocking wait on the per-message path", finding)

    def test_a_call_to_unregister_in_go_fails(self):
        """`unregister` takes `registryMu`, so a per-message function that calls it takes a
        lock per message, though neither function names `sync.`."""
        self.edit_go("\ts.handler = handler\n\ts.polling = true\n",
                     "\tunregister(s)\n\ts.handler = handler\n\ts.polling = true\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription.Poll", finding)
        self.assertIn("calls unregister", finding)
        self.assertIn("unregister takes a lock with `.Lock()`", finding)

    def test_a_panic_in_go_fails(self):
        self.edit_go("func (p *Publication) Abort() int64 {\n", "func (p *Publication) Abort() int64 {\n\tpanic(\"no\")\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.Abort", finding)
        self.assertIn("panics", finding)

    def test_an_allocation_in_go_fails(self):
        self.edit_go("func (p *Publication) SetReservedValue(value int64) int32 {\n",
                     "func (p *Publication) SetReservedValue(value int64) int32 {\n\t_ = make([]byte, 1)\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.SetReservedValue", finding)
        self.assertIn("allocates", finding)

    def test_a_select_in_go_fails(self):
        self.edit_go("func (h *Header) Values() (Frame, bool) {\n",
                     "func (h *Header) Values() (Frame, bool) {\n\tselect {\n\tdefault:\n\t}\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Header.Values", finding)
        self.assertIn("uses `select`", finding)

    def test_the_handler_error_after_the_polls_shim_call_passes_and_before_it_fails(self):
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit_go("\ts.handler = handler\n\ts.polling = true\n",
                     "\t_ = &HandlerError{}\n\ts.handler = handler\n\ts.polling = true\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription.Poll", finding)
        self.assertIn("builds HandlerError", finding)

    def test_a_call_to_last_error_in_go_fails(self):
        self.edit_go("func (p *Publication) TryClaim(length int) int64 {\n",
                     "func (p *Publication) TryClaim(length int) int64 {\n\t_ = lastError(\"x\")\n")
        builds, thread = lint.findings(self.tmp)
        self.assertIn("Publication.TryClaim", builds)
        self.assertIn("calls lastError", builds)
        self.assertIn("builds Error", builds)
        # It reads the thread's error too, without the lock a per-message call never takes.
        self.assertIn("Publication.TryClaim", thread)
        self.assertIn("reads the thread's error through lastError", thread)

    # The thread's error (clients/go/README.md, "The thread's error"): a function that reads
    # it through check or lastError holds the goroutine to its OS thread first.

    def test_a_go_add_without_the_thread_lock_fails(self):
        self.edit_go("func (c *Client) AddPublication(channel string, streamID int32) (*Publication, error) {\n"
                     "\tc.mu.Lock()\n\tdefer c.mu.Unlock()\n\truntime.LockOSThread()\n\tdefer runtime.UnlockOSThread()\n",
                     "func (c *Client) AddPublication(channel string, streamID int32) (*Publication, error) {\n"
                     "\tc.mu.Lock()\n\tdefer c.mu.Unlock()\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Client.AddPublication", finding)
        self.assertIn("reads the thread's error through check", finding)
        self.assertIn("runtime.LockOSThread()", finding)
        self.assertIn("clients/go/README.md#the-threads-error", finding)

    def test_a_go_close_that_locks_after_its_abi_call_fails(self):
        self.edit_go("\truntime.LockOSThread()\n\tdefer runtime.UnlockOSThread()\n"
                     "\tif result := C.cavalry_subscription_close(s.handle); result != 0 {\n",
                     "\tresult := C.cavalry_subscription_close(s.handle)\n"
                     "\truntime.LockOSThread()\n\tdefer runtime.UnlockOSThread()\n\tif result != 0 {\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription.Close", finding)
        self.assertIn("reads the thread's error through lastError", finding)

    def test_a_new_go_function_that_reads_the_error_without_the_lock_fails(self):
        with open(self.go, "a", encoding="utf-8") as f:
            f.write("\nfunc (c *Client) Probe() error {\n"
                    "\treturn check(\"cavalry_probe\", C.cavalry_probe(c.handle))\n}\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Client.Probe", finding)
        self.assertIn("through check", finding)

    def test_the_go_thread_lock_needs_its_deferred_unlock(self):
        self.edit_go("\truntime.LockOSThread()\n\tdefer runtime.UnlockOSThread()\n"
                     "\tvar k C.cavalry_replay_constants_t\n",
                     "\truntime.LockOSThread()\n\tvar k C.cavalry_replay_constants_t\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("newReplay", finding)

    def test_a_panic_in_the_go_trampoline_fails(self):
        self.edit_go("\ts.deliver(body)\n", "\tpanic(\"x\")\n\ts.deliver(body)\n", file=self.go_trampoline)
        [finding] = lint.findings(self.tmp)
        self.assertIn("cavalryGoFragment", finding)
        self.assertIn("trampoline.go", finding)
        self.assertIn("panics", finding)

    def test_a_lock_in_the_go_shim_fails(self):
        call = "  cavalryGoFragment((uintptr_t) clientd, (uint8_t *) buffer, length, (cavalry_header_t *) header);\n"
        self.edit_go(call, "  pthread_mutex_lock(&lock);\n" + call, file=self.go_shim)
        [finding] = lint.findings(self.tmp)
        self.assertIn("cavalry_go_trampoline", finding)
        self.assertIn("pthread_mutex_*", finding)

    def test_a_lock_where_the_go_shim_keeps_an_error_fails_every_call_that_keeps_one(self):
        self.edit_go("  error->code = (int32_t) cavalry_errcode();\n",
                     "  pthread_mutex_lock(&lock);\n  error->code = (int32_t) cavalry_errcode();\n", file=self.go_shim)
        found = lint.findings(self.tmp)
        for name in ("cavalry_go_try_claim", "cavalry_go_set_reserved_value", "cavalry_go_commit", "cavalry_go_abort",
                     "cavalry_go_offer", "cavalry_go_poll"):
            self.assertTrue(any(f"go: {name} " in f and "calls cavalry_go_keep" in f and "pthread_mutex_*" in f
                                for f in found), (name, found))

    def test_a_go_module_without_the_per_message_functions_fails(self):
        with open(self.go, "w", encoding="utf-8") as f:
            f.write("package cavalry\n\nfunc nothing() int { return 1 }\n")
        with open(self.go_trampoline, "w", encoding="utf-8") as f:
            f.write("package cavalry\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("none of the per-message functions", finding)
        self.assertIn("Publication.TryClaim", finding)

    def test_a_missing_go_file_says_to_generate(self):
        os.remove(self.go)
        found = lint.findings(self.tmp)
        self.assertTrue(any("cavalry.go is missing" in f and "python3 clients/generate.py go" in f for f in found), found)

    def test_a_language_without_rules_fails(self):
        os.makedirs(os.path.join(self.tmp, "ruby", "cavalry"))
        with open(os.path.join(self.tmp, "ruby", "cavalry", "client.rb"), "w", encoding="utf-8") as f:
            f.write("# generated\n")
        found = lint.findings(self.tmp)
        self.assertTrue(any("ruby" in f and "no rules" in f for f in found), found)

    def test_a_missing_generated_file_says_to_generate(self):
        os.remove(self.file)
        [finding] = lint.findings(self.tmp)
        self.assertIn("python3 clients/generate.py python", finding)

    # The JVM client's package, read by the tokenizer (clients/jvm/README.md).

    def test_a_synchronized_jvm_per_message_method_fails(self):
        self.edit_jvm("    public long tryClaim(int length) {\n", "    public synchronized long tryClaim(int length) {\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.tryClaim", finding)
        self.assertIn("is `synchronized`", finding)

    def test_a_synchronized_block_in_jvm_fails(self):
        self.edit_jvm("    public long commit() {\n        claim = null;\n",
                      "    public long commit() {\n        synchronized (this) {\n            claim = null;\n        }\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.commit", finding)
        self.assertIn("uses `synchronized`", finding)

    def test_a_lock_taken_on_any_receiver_in_jvm_fails(self):
        self.edit_jvm("    public long offer(MemorySegment message) {\n        try {\n",
                      "    public long offer(MemorySegment message) {\n        guard.lock();\n        try {\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.offer", finding)
        self.assertIn("takes a lock", finding)

    def test_a_lock_class_and_a_park_in_jvm_fail(self):
        self.edit_jvm("    public long abort() {\n        claim = null;\n",
                      "    public long abort() {\n        LockSupport.parkNanos(1);\n        claim = null;\n")
        found = lint.findings(self.tmp)
        self.assertTrue(any("Publication.abort" in f and "uses a lock class" in f for f in found), found)
        self.assertTrue(any("Publication.abort" in f and "blocks" in f for f in found), found)

    def test_a_future_in_jvm_fails(self):
        self.edit_jvm("    public int setReservedValue(long value) {\n        try {\n",
                      "    public int setReservedValue(long value) {\n        CompletableFuture.completedFuture(0);\n        try {\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.setReservedValue", finding)
        self.assertIn("uses a future", finding)

    def test_a_blocking_wait_in_jvm_fails(self):
        self.edit_jvm("    public long commit() {\n        claim = null;\n",
                      "    public long commit() {\n        Thread.sleep(1);\n        claim = null;\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.commit", finding)
        self.assertIn("blocks", finding)

    def test_a_thread_started_in_jvm_fails(self):
        self.edit_jvm("    public long offer(MemorySegment message) {\n        try {\n",
                      "    public long offer(MemorySegment message) {\n        Thread.ofPlatform().start(() -> {});\n        try {\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.offer", finding)
        self.assertIn("starts a thread", finding)

    def test_a_throw_outside_the_compilers_catch_in_jvm_fails(self):
        self.edit_jvm("    public long tryClaim(int length) {\n        try {\n",
                      "    public long tryClaim(int length) {\n        if (length < 0) throw new IllegalArgumentException(\"no\");\n"
                      "        try {\n")
        found = lint.findings(self.tmp)
        self.assertTrue(any("Publication.tryClaim" in f and "throws" in f for f in found), found)
        self.assertTrue(any("Publication.tryClaim" in f and "builds IllegalArgumentException" in f for f in found), found)

    def test_a_throw_in_a_catch_without_invoke_exact_in_jvm_fails(self):
        """The compiler's catch is the one after a try holding invokeExact; the trampoline's
        catch around the handler's call holds none, so a throw there is a raise."""
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit_jvm("        } catch (Throwable t) {\n            raised++;\n",
                      "        } catch (Throwable t) {\n            raised++;\n            throw t;\n", file="Subscription.java")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription.onFragment", finding)
        self.assertIn("throws", finding)

    def test_an_allocation_in_jvm_fails(self):
        self.edit_jvm("    public long commit() {\n        claim = null;\n",
                      "    public long commit() {\n        byte[] copy = new byte[8];\n        claim = null;\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.commit", finding)
        self.assertIn("allocates byte with `new`", finding)

    def test_building_handler_exception_before_the_polls_call_fails(self):
        self.edit_jvm("        int count;\n        try {\n            count = (int) Abi.cavalry_subscription_poll.invokeExact(",
                      "        HandlerException early = new HandlerException(0, 0, null);\n        int count;\n        try {\n"
                      "            count = (int) Abi.cavalry_subscription_poll.invokeExact(", file="Subscription.java")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription.poll", finding)
        self.assertIn("builds HandlerException", finding)

    def test_a_throw_in_a_jvm_header_getter_fails(self):
        self.edit_jvm("    public int sessionId() {\n        ensure();\n",
                      "    public int sessionId() {\n        throw new IllegalStateException(\"no\");\n", file="Header.java")
        found = lint.findings(self.tmp)
        self.assertTrue(any(f.startswith("jvm: Header.sessionId ") and "throws" in f for f in found), found)

    def test_a_call_to_check_outside_the_compilers_catch_in_jvm_fails(self):
        self.edit_jvm("    public long tryClaim(int length) {\n        try {\n",
                      "    public long tryClaim(int length) {\n        Abi.check(\"cavalry_publication_try_claim\", 1);\n        try {\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.tryClaim", finding)
        self.assertIn("calls Abi.check", finding)
        self.assertIn("Abi.check throws", finding)

    def test_the_guards_throw_passes_and_a_raising_helper_a_getter_calls_fails(self):
        """Header.ensure throws for a read outside the handler call, and the committed
        package passes with it; a getter that calls Abi.lastError, which builds the
        exception, is a raise on the path."""
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit_jvm("    public int sessionId() {\n        ensure();\n",
                      "    public int sessionId() {\n        ensure();\n        Abi.lastError(\"cavalry_header_values\");\n",
                      file="Header.java")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Header.sessionId", finding)
        self.assertIn("calls Abi.lastError", finding)
        self.assertIn("builds CavalryException", finding)

    def test_a_jvm_package_without_the_per_message_methods_fails(self):
        for name in ("Publication", "Subscription", "Header"):
            with open(os.path.join(self.jvm, name + ".java"), "w", encoding="utf-8") as f:
                f.write(f"package com.appshapes.cavalry;\n\npublic final class {name} {{\n    int here() {{\n"
                        f"        return 1;\n    }}\n}}\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("none of the per-message functions", finding)
        self.assertIn("Publication.tryClaim", finding)

    def test_one_missing_jvm_per_message_method_fails(self):
        self.edit_jvm("    public long offer(MemorySegment message) {\n", "    public long send(MemorySegment message) {\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.offer is not in", finding)

    def test_a_missing_jvm_file_says_to_generate(self):
        os.remove(os.path.join(self.jvm, "Header.java"))
        found = lint.findings(self.tmp)
        self.assertTrue(any("Header.java is missing" in f and "python3 clients/generate.py jvm" in f for f in found), found)


    # The .NET client's package, read by the tokenizer (clients/dotnet/README.md).

    def test_a_lock_in_a_dotnet_per_message_method_fails(self):
        self.edit_dotnet("    public long TryClaim(int length)\n    {\n",
                         "    public long TryClaim(int length)\n    {\n        lock (this)\n        {\n        }\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.TryClaim", finding)
        self.assertIn("uses `lock`", finding)

    def test_a_monitor_in_dotnet_fails(self):
        self.edit_dotnet("    public long Commit()\n    {\n        data = null;\n",
                         "    public long Commit()\n    {\n        Monitor.Enter(this);\n        data = null;\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.Commit", finding)
        self.assertIn("uses a lock or a wait of `System.Threading`", finding)

    def test_a_system_threading_lock_in_dotnet_fails(self):
        self.edit_dotnet("    public long Commit()\n    {\n        data = null;\n",
                         "    public long Commit()\n    {\n        Lock gate = new();\n        data = null;\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.Commit", finding)
        self.assertIn("uses `System.Threading.Lock`", finding)

    def test_an_async_dotnet_per_message_method_fails(self):
        self.edit_dotnet("    public long Abort()\n", "    public async long Abort()\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.Abort", finding)
        self.assertIn("is `async`", finding)

    def test_an_await_in_dotnet_fails(self):
        self.edit_dotnet("    public long Abort()\n    {\n        data = null;\n",
                         "    public long Abort()\n    {\n        await System.Threading.Tasks.Task.Yield();\n        data = null;\n")
        found = lint.findings(self.tmp)
        self.assertTrue(any("Publication.Abort" in f and "awaits" in f for f in found), found)
        self.assertTrue(any("Publication.Abort" in f and "uses a task" in f for f in found), found)

    def test_a_blocking_wait_in_dotnet_fails(self):
        self.edit_dotnet("    public int SetReservedValue(long value)\n    {\n",
                         "    public int SetReservedValue(long value)\n    {\n        Thread.Sleep(1);\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.SetReservedValue", finding)
        self.assertIn("blocks", finding)

    def test_a_thread_started_in_dotnet_fails(self):
        self.edit_dotnet("    public long Offer(ReadOnlySpan<byte> message)\n    {\n",
                         "    public long Offer(ReadOnlySpan<byte> message)\n    {\n        new Thread(() => { }).Start();\n")
        found = lint.findings(self.tmp)
        self.assertTrue(any("Publication.Offer" in f and "starts a thread" in f for f in found), found)

    def test_a_throw_in_dotnet_fails(self):
        self.edit_dotnet("    public long TryClaim(int length)\n    {\n",
                         "    public long TryClaim(int length)\n    {\n        if (length < 0) throw new ArgumentException(\"no\");\n")
        found = lint.findings(self.tmp)
        self.assertTrue(any("Publication.TryClaim" in f and "throws" in f for f in found), found)
        self.assertTrue(any("Publication.TryClaim" in f and "builds ArgumentException" in f for f in found), found)

    def test_an_allocation_in_dotnet_fails(self):
        self.edit_dotnet("    public long Commit()\n    {\n        data = null;\n",
                         "    public long Commit()\n    {\n        byte[] copy = new byte[8];\n        data = null;\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.Commit", finding)
        self.assertIn("allocates byte with `new`", finding)

    def test_an_interpolated_string_in_dotnet_fails(self):
        self.edit_dotnet("    public long Abort()\n    {\n        data = null;\n",
                         "    public long Abort()\n    {\n        string s = $\"{length}\";\n        data = null;\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.Abort", finding)
        self.assertIn("allocates an interpolated string", finding)

    def test_the_claims_span_passes_and_a_throw_in_its_getter_fails(self):
        """The getter makes a Span over the bytes the ABI handed over, a struct on the stack,
        and the committed package passes with it; a throw there is a raise on the path."""
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit_dotnet("    public Span<byte> Claim => data == null ? default : new Span<byte>(data, length);\n",
                         "    public Span<byte> Claim => throw new InvalidOperationException(\"no\");\n")
        found = lint.findings(self.tmp)
        self.assertTrue(any(f.startswith("dotnet: Publication.Claim ") and "throws" in f for f in found), found)

    def test_building_handler_exception_before_the_polls_call_fails(self):
        self.edit_dotnet("        int count = Abi.cavalry_subscription_poll(handle, &OnFragment, GCHandle.ToIntPtr(self), (nuint) fragmentLimit);\n",
                         "        HandlerException early = new HandlerException(0, 0, first!);\n"
                         "        int count = Abi.cavalry_subscription_poll(handle, &OnFragment, GCHandle.ToIntPtr(self), (nuint) fragmentLimit);\n",
                         file="Subscription.cs")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription.Poll", finding)
        self.assertIn("builds HandlerException", finding)

    def test_a_throw_in_the_trampolines_catch_fails(self):
        self.edit_dotnet("        catch (Exception e)\n        {\n            raised++;\n",
                         "        catch (Exception e)\n        {\n            raised++;\n            throw;\n", file="Subscription.cs")
        found = lint.findings(self.tmp)
        self.assertTrue(any(f.startswith("dotnet: Subscription.Deliver ") and "throws" in f for f in found), found)
        self.assertTrue(any(f.startswith("dotnet: Subscription.OnFragment ") and "calls Subscription.Deliver" in f for f in found), found)

    def test_a_throw_in_a_dotnet_header_getter_fails(self):
        self.edit_dotnet("    public int SessionId\n    {\n        get\n        {\n            Ensure();\n",
                         "    public int SessionId\n    {\n        get\n        {\n            throw new InvalidOperationException(\"no\");\n",
                         file="Header.cs")
        found = lint.findings(self.tmp)
        self.assertTrue(any(f.startswith("dotnet: Header.SessionId ") and "throws" in f for f in found), found)

    def test_a_call_to_check_in_dotnet_fails(self):
        self.edit_dotnet("    public long TryClaim(int length)\n    {\n",
                         "    public long TryClaim(int length)\n    {\n        Abi.Check(\"cavalry_publication_try_claim\", 1);\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.TryClaim", finding)
        self.assertIn("calls Abi.Check", finding)
        self.assertIn("Abi.Check throws", finding)

    def test_the_guards_throw_passes_and_a_raising_helper_a_getter_calls_fails(self):
        """Header.Ensure throws for a read outside the handler call, and the committed
        package passes with it; a getter that calls Abi.LastError, which builds the
        exception, is a raise on the path."""
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit_dotnet("    public int SessionId\n    {\n        get\n        {\n            Ensure();\n",
                         "    public int SessionId\n    {\n        get\n        {\n            Ensure();\n"
                         "            Abi.LastError(\"cavalry_header_values\");\n", file="Header.cs")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Header.SessionId", finding)
        self.assertIn("calls Abi.LastError", finding)
        self.assertIn("builds CavalryException", finding)

    def test_a_dotnet_package_without_the_per_message_members_fails(self):
        for name in ("Publication", "Subscription", "Header"):
            with open(os.path.join(self.dotnet, name + ".cs"), "w", encoding="utf-8") as f:
                f.write(f"namespace Cavalry;\n\npublic sealed class {name}\n{{\n    internal int Here()\n    {{\n"
                        f"        return 1;\n    }}\n}}\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("none of the per-message functions", finding)
        self.assertIn("Publication.TryClaim", finding)

    def test_one_missing_dotnet_per_message_method_fails(self):
        self.edit_dotnet("    public long Offer(ReadOnlySpan<byte> message)\n", "    public long Send(ReadOnlySpan<byte> message)\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.Offer is not in", finding)

    def test_a_missing_dotnet_file_says_to_generate(self):
        os.remove(os.path.join(self.dotnet, "Header.cs"))
        found = lint.findings(self.tmp)
        self.assertTrue(any("Header.cs is missing" in f and "python3 clients/generate.py dotnet" in f for f in found), found)


if __name__ == "__main__":
    unittest.main()
