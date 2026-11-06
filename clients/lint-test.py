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
and a missing `index.js` says to generate. `just test` runs this through `test-scripts`.
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

    def edit(self, old, new, count=1, file=None):
        file = file or self.file
        with open(file, encoding="utf-8") as f:
            text = f.read()
        self.assertEqual(text.count(old), count, old)
        with open(file, "w", encoding="utf-8") as f:
            f.write(text.replace(old, new))

    def edit_node(self, old, new, count=1):
        self.edit(old, new, count, file=self.node)

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


if __name__ == "__main__":
    unittest.main()
