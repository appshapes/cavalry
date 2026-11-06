#!/usr/bin/env python3
"""The tests of clients/lint.py (ADR 0026): the committed Python package passes; a generated
per-message function carrying each forbidden form, a `threading` lock, `async def`, `await`
or `raise`, fails naming the function and the form; a raise in `poll` after the ABI's call
passes and one before it fails; a package with none of the per-message functions fails; a
language with no rules fails. `just test` runs this through `test-scripts`.
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

    def edit(self, old, new, count=1):
        with open(self.file, encoding="utf-8") as f:
            text = f.read()
        self.assertEqual(text.count(old), count, old)
        with open(self.file, "w", encoding="utf-8") as f:
            f.write(text.replace(old, new))

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

    def test_a_raise_in_the_trampoline_fails(self):
        self.edit("            self._pending = exc\n", "            self._pending = exc\n            raise\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription._on_fragment", finding)

    def test_a_package_without_the_per_message_functions_fails(self):
        for name in ("try_claim", "offer", "commit", "abort", "poll", "_on_fragment"):
            self.edit(f"    def {name}(self", f"    def renamed_{name}(self")
        [finding] = lint.findings(self.tmp)
        self.assertIn("none of the per-message functions", finding)

    def test_one_missing_per_message_function_fails(self):
        self.edit("    def offer(self, message):", "    def send(self, message):")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.offer is not in", finding)

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
