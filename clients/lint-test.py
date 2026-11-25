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
header getter that throws fails; a cluster session's `poll` and a cluster service's are
held as the subscription's is, a throw in the service's `reply` or in a getter of a
reply's values or an entry's fails, and so does the error for a value that is no action
built before the service's addon call (ADR 0124); a module with none of the per-message
functions fails,
and a missing `index.js` says to generate. The same for the Node client's addon,
`src/addon.c`: one test per C form, `pthread_mutex_*` in the trampoline,
`pthread_rwlock_*` in the view, C11's `mtx_*` in the poll, libuv's `uv_mutex_*` in a helper
the trampoline calls, `uv_rwlock_*` in the header's field, an N-API thread-safe function in
the offer, `napi_throw*` in the commit and `napi_fatal_*` in the Buffers' finalizer, each
failing and naming the function and the form; an argument reader's throw passes and the
same throw in a helper the poll calls fails; a lock in the cluster polls' trampoline, in
either poll, in the reader of a reply's values or an entry's, a throw in the helper that
reads a service's handler's action, in what a cluster poll reports, or in the wrapper of
the session's offer or of the service's reply or action, fails; a missing per-message
function, an addon with
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
fails each shim call that keeps one; a cluster session's `Poll` and a cluster service's
are held as the subscription's is, a lock in the service's `Poll`, which is a call on its
client, a panic in either cluster trampoline or an allocation in the function that calls
a service's handler fails, and so does a lock in either cluster trampoline of `shim.c`
(ADR 0124); a function that reads the thread's error through
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
fails while `Header.ensure`'s own throw, the guard, passes; a cluster session's `poll` and
a cluster service's are held as the subscription's is, a `synchronized` service's `poll`
or one that calls the client's `synchronized` method fails, and so does a throw in either
cluster upcall's catch, an exception built where a service's upcall keeps the ABI's
refusal, a throw in the session's `offer` or the service's `reply`, and an allocation in
a getter of a reply's values or an entry's (ADR 0124); a package with none of the
per-message methods fails, one method renamed fails, and a missing file says to generate. The same for
the .NET client's package: a per-message member that uses `lock`, `Monitor` or
`System.Threading.Lock`, is `async`, awaits, uses a task, blocks, starts a thread, throws,
allocates with `new` or an interpolated string, or calls the package's throwing `Abi.Check`
or `Abi.LastError`, fails naming the member and the form; the `Span` the claim's getter
makes over the ABI's bytes passes while a throw in that getter fails; a throw in the
trampoline's catch fails; building the `HandlerException` in `Poll` before its ABI call
fails; a header getter that throws fails while `Header.Ensure`'s own throw, the guard,
passes; a cluster session's `Poll` and a cluster service's are held as the subscription's
is, a service's `Poll` under the client's lock or one that calls the client's locking
method fails, and so does a throw in either cluster catch, an exception built where a
service's trampoline keeps the ABI's refusal, a throw in the session's `Offer` or the
service's `Reply`, and an allocation in a getter of a reply's values or an entry's (ADR
0124); a package with none of the per-message members fails, one member renamed fails,
and a missing file says to generate.
"""

import os
import re
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
        handler = "(void *clientd, const uint8_t *buffer, size_t length, const cavalry_header_t *header) {\n"
        openings = {"trampoline": "static void trampoline" + handler,
                    "cluster_trampoline": "static void cluster_trampoline" + handler,
                    "buffer_freed": "  (void) env; (void) data; (void) hint;\n",
                    "keep_failure": "  if (s->first != FIRST_NONE) return;\n  s->first = FIRST_FAILURE;\n",
                    "report": "  napi_value tuple, first = NULL, holder = NULL;\n",
                    "cluster_action": "static void cluster_action(subscription_state_t *s, napi_value result) {\n",
                    "cluster_report": "  napi_value first = NULL, holder = NULL, tuple;\n"}
        opening = openings.get(function, f"static napi_value {function}(napi_env env, napi_callback_info info) {{\n")
        self.edit(opening, opening + f"  {line}\n", file=self.addon)

    def assert_addon_finding(self, function, form):
        found = lint.findings(self.tmp)
        self.assertTrue(any(f.startswith(f"node: {function} (") and form in f for f in found), found)
        return found

    def test_the_committed_package_passes(self):
        self.assertEqual(lint.findings(lint.CLIENTS), [])

    def test_a_lock_in_a_per_message_function_fails(self):
        """Both offers, the publication's and the cluster session's."""
        self.edit("    def offer(self, message):\n", "    def offer(self, message):\n        with threading.Lock():\n            pass\n",
                  count=2)
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for offer in ("Publication.offer", "ClusterSession.offer"):
            self.assertTrue(any(offer in f and "uses `threading`" in f for f in found), found)

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
        """The four polls, the subscription's, the committed log's, the cluster session's
        and the cluster service's: the line is in each."""
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit("        self._handler = handler\n", "        self._handler = handler\n        raise RuntimeError('too early')\n",
                  count=4)
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 4, found)
        for poll in ("Subscription.poll", "CommittedLog.poll", "ClusterSession.poll", "ClusterService.poll"):
            self.assertTrue(any(poll in f and "raises" in f for f in found), found)

    def test_a_raise_between_polls_two_abi_calls_fails(self):
        """`poll` calls the ABI on two lines, the refused nested poll and its own; the raise
        allowed is after the last of them."""
        self.edit("        try:\n            fragments = self._abi.cavalry_subscription_poll(",
                  "        raise RuntimeError('between')\n        try:\n"
                  "            fragments = self._abi.cavalry_subscription_poll(")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription.poll", finding)
        self.assertIn("raises", finding)

    def test_a_raise_between_the_committed_logs_two_abi_calls_fails(self):
        """The committed log's `poll` calls the ABI on two lines as the subscription's
        does, and the raise allowed is after the last of them."""
        self.edit("        try:\n            entries = self._abi.cavalry_committed_log_poll(",
                  "        raise RuntimeError('between')\n        try:\n"
                  "            entries = self._abi.cavalry_committed_log_poll(")
        [finding] = lint.findings(self.tmp)
        self.assertIn("CommittedLog.poll", finding)
        self.assertIn("raises", finding)

    def test_a_raise_between_a_cluster_polls_two_abi_calls_fails(self):
        """A cluster session's `poll` and a cluster service's call the ABI on two lines as
        the subscription's does, and the raise allowed is after the last of them (ADR
        0124)."""
        self.edit("        try:\n            replies = self._abi.cavalry_cluster_session_poll(",
                  "        raise RuntimeError('between')\n        try:\n"
                  "            replies = self._abi.cavalry_cluster_session_poll(")
        self.edit("        try:\n            entries = self._abi.cavalry_cluster_service_poll(",
                  "        raise RuntimeError('between')\n        try:\n"
                  "            entries = self._abi.cavalry_cluster_service_poll(")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for poll in ("ClusterSession.poll", "ClusterService.poll"):
            self.assertTrue(any(poll in f and "raises" in f for f in found), found)

    def test_a_raise_in_a_cluster_services_reply_fails(self):
        """The reply is a per-message function: a code is returned, never raised."""
        self.edit("    def reply(self, session_id, message):\n",
                  "    def reply(self, session_id, message):\n        raise RuntimeError('no')\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("ClusterService.reply", finding)
        self.assertIn("raises", finding)

    def test_a_raise_in_a_cluster_services_timer_calls_fails(self):
        """The schedule and the cancel of a timer are per-message functions (ADR 0157): a
        code is returned, never raised, in their own bodies."""
        self.edit("    def schedule_timer(self, correlation_id, deadline):\n",
                  "    def schedule_timer(self, correlation_id, deadline):\n        raise RuntimeError('no')\n")
        self.edit("    def cancel_timer(self, correlation_id):\n",
                  "    def cancel_timer(self, correlation_id):\n        raise RuntimeError('no')\n")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for name in ("ClusterService.schedule_timer", "ClusterService.cancel_timer"):
            self.assertTrue(any(name in f and "raises" in f for f in found), found)

    CANCEL_TIMER = "        return self._abi.cavalry_cluster_service_cancel_timer(self._handle, correlation_id)\n"
    TRY_CLAIM = "        result = self._abi.cavalry_publication_try_claim(self._handle, length, self._claim_ref)\n"
    SUBSCRIPTION_POLL = ("            fragments = self._abi.cavalry_subscription_poll(self._handle, self._trampoline, "
                         "None, fragment_limit)\n")
    INT64 = "def _int64(value, name, next=_RANGE_NEXT):\n"
    SIZE = "def _size(value, name):\n"

    def test_the_argument_reader_may_raise_and_no_other_helper_of_a_timer_call_may(self):
        """`_int64` refuses a timer's id or deadline that is no int64 before the ABI is
        called, and the `raise` in its own body is none on the path, as the Node addon's
        readers' throw is none. Any other function of the package that a timer call
        reaches is held to the rule, `_check` among them."""
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit(self.CANCEL_TIMER,
                  "        _check(self._abi, 'cavalry_cluster_service_cancel_timer', 0)\n" + self.CANCEL_TIMER)
        [finding] = lint.findings(self.tmp)
        self.assertIn("ClusterService.cancel_timer", finding)
        self.assertIn("calls _check", finding)

    def test_the_argument_reader_is_held_to_every_other_form(self):
        """A reader's own `raise` and nothing else: a lock in it, an exception of the
        package's built in it, and a function it calls that raises, are each on the path
        of the functions that call it."""
        for line, said in (("    threading.Lock()\n", "threading"),
                           ("    CavalryError('cavalry_x', 1, 'no')\n", "builds CavalryError"),
                           ("    _check(library(), 'cavalry_x', 0)\n", "calls _check")):
            with self.subTest(line=line):
                self.setUp()
                self.edit(self.INT64, self.INT64 + line)
                found = lint.findings(self.tmp)
                for name in ("ClusterService.schedule_timer", "ClusterService.cancel_timer", "ClusterService.reply",
                             "Publication.set_reserved_value"):
                    self.assertTrue(any(name in f and "_int64" in f and said in f for f in found), found)
                self.assertTrue(all("_int64" in f for f in found), found)
                self.setUp()
                self.edit(self.SIZE, self.SIZE + line)
                found = lint.findings(self.tmp)
                for name in ("Publication.try_claim", "Subscription.poll", "CommittedLog.poll", "ClusterSession.poll",
                             "ClusterService.poll"):
                    self.assertTrue(any(name in f and "_size" in f and said in f for f in found), found)
                self.assertTrue(all("_size" in f for f in found), found)

    def test_the_argument_reader_is_called_before_the_abi_or_as_its_argument(self):
        """A reader refuses an argument before the ABI is called: a call of either
        reader on a line after its function's ABI call, on the ABI's own answer here, is
        a finding. That holds in a poll too, which may raise its handler's exception
        once the ABI has returned: a reader is not that raise, whether the rule names it
        for the poll, as `_size`, or does not, as `_int64`. As an argument of the ABI
        call a reader is none."""
        for name, old, new, said in (
                ("ClusterService.cancel_timer", self.CANCEL_TIMER,
                 "        answered = self._abi.cavalry_cluster_service_cancel_timer(self._handle, correlation_id)\n"
                 "        return _int64(answered, \"correlation_id\")\n", "after its call of the ABI"),
                ("Publication.try_claim", self.TRY_CLAIM, self.TRY_CLAIM + "        _size(result, 'length')\n",
                 "after its call of the ABI"),
                ("Subscription.poll", self.SUBSCRIPTION_POLL,
                 self.SUBSCRIPTION_POLL + "            _size(fragments, 'fragment_limit')\n", "after its call of the ABI"),
                ("Subscription.poll", self.SUBSCRIPTION_POLL,
                 self.SUBSCRIPTION_POLL + "            _int64(fragments, 'fragment_limit')\n", "calls _int64")):
            with self.subTest(name=name, new=new):
                self.setUp()
                self.edit(old, new)
                found = lint.findings(self.tmp)  # one for each ABI call the reader comes after: a poll has two
                self.assertTrue(found and all(name in f and said in f for f in found), found)
        self.setUp()
        self.edit(self.CANCEL_TIMER, "        return self._abi.cavalry_cluster_service_cancel_timer("
                                     "self._handle, _int64(correlation_id, \"correlation_id\"))\n")
        self.edit(self.TRY_CLAIM, "        result = self._abi.cavalry_publication_try_claim("
                                  "self._handle, _size(length, 'length'), self._claim_ref)\n")
        self.assertEqual(lint.findings(self.tmp), [])

    def test_an_argument_reader_is_for_the_functions_the_rule_names_and_no_trampoline(self):
        """Only the functions the rule names beside a reader may call it. In a trampoline
        its raise would be lost in the call-back; a commit that called the int64's reader,
        and a publication's offer that called the count's, would raise where the rule has
        named no reader for them; and a function named for one reader may not call the
        other, a reply the count's or a claim the int64's."""
        edits = (("ClusterService._on_entry", "_int64", "    def _on_entry(self, clientd, buffer, length, header):\n",
                  "        _int64(length, 'length')\n"),
                 ("Subscription._on_fragment", "_size", "    def _on_fragment(self, clientd, buffer, length, header):\n",
                  "        _size(length, 'length')\n"),
                 ("Publication.commit", "_int64", "    def commit(self):\n", "        _int64(0, 'position')\n"),
                 ("ClusterService.reply", "_size", "    def reply(self, session_id, message):\n",
                  "        _size(session_id, 'session_id')\n"),
                 ("Publication.try_claim", "_int64", "    def try_claim(self, length):\n",
                  "        _int64(length, 'length')\n"))
        for name, reader, signature, call in edits:
            with self.subTest(name=name):
                self.setUp()
                with open(self.file, encoding="utf-8") as f:
                    text = f.read()
                at = text.index(signature, text.index(f"\nclass {name.split('.')[0]}"))
                # After the method's docstring, where it has one: its first statement.
                end = at + len(signature)
                if text[end:].lstrip().startswith('"""'):
                    end = text.index('"""\n', text.index('"""', end) + 3) + 4
                with open(self.file, "w", encoding="utf-8") as f:
                    f.write(text[:end] + call + text[end:])
                found = lint.findings(self.tmp)
                self.assertEqual(len(found), 1, found)
                self.assertTrue(name in found[0] and f"calls {reader}" in found[0] and "raises" in found[0], found)
        self.setUp()
        self.edit("        return self._abi.cavalry_publication_offer(self._handle, message, len(message))\n",
                  "        return self._abi.cavalry_publication_offer(self._handle, message, "
                  "_size(len(message), 'length'))\n")
        [finding] = lint.findings(self.tmp)
        self.assertTrue("Publication.offer" in finding and "calls _size" in finding and "raises" in finding, finding)

    def test_each_per_message_function_reads_the_integer_its_caller_gives(self):
        """The readers the committed package calls, in the function that calls each: a
        claim's length and each poll's fragment limit through `_size`, a reply's session,
        the reserved value and a timer's id and deadline through `_int64`. Each function
        tests its argument in line and calls its reader, on the next line, only for a
        value that fails the test; the reader's answer is what it passes on, and the
        ABI's call comes after. A wrapper that dropped its reader would pass ctypes an
        int that it wraps."""
        with open(self.file, encoding="utf-8") as f:
            text = f.read()
        size = "0 <= {0} <= _SIZE_MAX", '_size({0}, "{0}")'
        int64 = "_INT64_MIN <= {0} <= _INT64_MAX", '_int64({0}, "{0}")'
        timer = "_INT64_MIN <= {0} <= _INT64_MAX", '_int64({0}, "{0}", _TIMER_NEXT)'
        for cls, method, parameter, (held, read) in (
                ("Publication", "try_claim", "length", size),
                ("Publication", "set_reserved_value", "value", int64),
                ("Subscription", "poll", "fragment_limit", size),
                ("CommittedLog", "poll", "fragment_limit", size),
                ("ClusterSession", "poll", "fragment_limit", size),
                ("ClusterService", "poll", "fragment_limit", size),
                ("ClusterService", "reply", "session_id", int64),
                ("ClusterService", "schedule_timer", "correlation_id", timer),
                ("ClusterService", "schedule_timer", "deadline", timer),
                ("ClusterService", "cancel_timer", "correlation_id", timer)):
            with self.subTest(method=f"{cls}.{method}", parameter=parameter):
                body = re.split(rf"\nclass {cls}\b", text, maxsplit=1)[1].split("\nclass ", 1)[0]
                body = body.split(f"\n    def {method}(", 1)[1].split("\n    def ", 1)[0]
                guard = (f"        if type({parameter}) is not int or not {held.format(parameter)}:\n"
                         f"            {parameter} = {read.format(parameter)}\n")
                self.assertEqual(body.count(guard), 1, body)
                self.assertEqual(body.count(f"{read.split('(')[0]}({parameter}, "), 1)
                self.assertLess(body.index(guard), body.index("self._abi."))

    def test_the_argument_reader_reached_through_a_helper_is_a_raise(self):
        """A function between a per-message function and its reader is no reader: its
        call is a raise on the path, for either reader."""
        self.edit(self.INT64, "def _id(value):\n    return _int64(value, 'correlation_id')\n\n\n" + self.INT64)
        self.edit(self.CANCEL_TIMER,
                  "        return self._abi.cavalry_cluster_service_cancel_timer(self._handle, _id(correlation_id))\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("ClusterService.cancel_timer", finding)
        self.assertIn("calls _id", finding)
        self.setUp()
        self.edit(self.SIZE, "def _count(value):\n    return _size(value, 'length')\n\n\n" + self.SIZE)
        self.edit(self.TRY_CLAIM, "        result = self._abi.cavalry_publication_try_claim("
                                  "self._handle, _count(length), self._claim_ref)\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.try_claim", finding)
        self.assertIn("calls _count", finding)

    def test_an_argument_reader_the_package_does_not_have_is_a_stale_rule(self):
        self.edit(self.INT64, "def _int64_argument(value, name, next=_RANGE_NEXT):\n")
        self.edit("_int64(correlation_id, \"correlation_id\", _TIMER_NEXT)",
                  "_int64_argument(correlation_id, \"correlation_id\", _TIMER_NEXT)", count=2)
        self.edit("_int64(deadline, \"deadline\", _TIMER_NEXT)", "_int64_argument(deadline, \"deadline\", _TIMER_NEXT)")
        self.edit(self.SIZE, "def _count(value, name):\n")
        found = lint.findings(self.tmp)
        self.assertTrue(any("the argument reader _int64 is not in" in f for f in found), found)
        self.assertTrue(any("the argument reader _size is not in" in f for f in found), found)
        # The renamed function is no reader, so its raise is one on the path again.
        self.assertTrue(any("ClusterService.schedule_timer" in f and "calls _int64_argument" in f for f in found), found)

    def test_an_exception_built_in_a_cluster_services_trampoline_fails(self):
        """A value that is no action is kept for `poll` to raise: the trampoline builds no
        exception of the package's for it."""
        self.edit("                self._refused = action\n",
                  "                self._refused = CavalryError('cavalry_cluster_service_action', 146, 'no action')\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("ClusterService._on_entry", finding)
        self.assertIn("builds CavalryError", finding)

    def test_a_raise_in_the_trampoline_fails(self):
        """The four trampolines: the subscription's, the committed log's, the cluster
        session's and the cluster service's."""
        self.edit("            self._keep(exc)\n", "            self._keep(exc)\n            raise\n", count=4)
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 4, found)
        for trampoline in ("Subscription._on_fragment", "CommittedLog._on_fragment", "ClusterSession._on_reply",
                           "ClusterService._on_entry"):
            self.assertTrue(any(trampoline in f for f in found), found)

    def test_a_raise_in_the_helper_the_trampoline_calls_fails(self):
        """`_keep`, in each of the four classes that poll with a handler."""
        self.edit("a `KeyboardInterrupt` or a `SystemExit`.\"\"\"\n        self._raised += 1\n",
                  "a `KeyboardInterrupt` or a `SystemExit`.\"\"\"\n        self._raised += 1\n        raise exc\n", count=4)
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 4, found)
        for cls, trampoline in (("Subscription", "_on_fragment"), ("CommittedLog", "_on_fragment"),
                                ("ClusterSession", "_on_reply"), ("ClusterService", "_on_entry")):
            self.assertTrue(any(f"{cls}.{trampoline}" in f and f"calls {cls}._keep" in f for f in found), found)

    def test_a_raise_in_set_reserved_value_fails(self):
        self.edit("    def set_reserved_value(self, value):\n",
                  "    def set_reserved_value(self, value):\n        raise RuntimeError('no')\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.set_reserved_value", finding)
        self.assertIn("raises", finding)

    def test_a_call_to_check_fails(self):
        self.edit("    def offer(self, message):\n",
                  "    def offer(self, message):\n        _check(self._abi, 'cavalry_publication_offer', -1)\n", count=2)
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for offer in ("Publication.offer", "ClusterSession.offer"):
            self.assertTrue(any(offer in f and "calls _check" in f and "_check raises at line" in f for f in found), found)

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
        # The poll is in four classes, the subscription, the committed log, the cluster
        # session and the cluster service; `_on_fragment` in the first two; the offer in
        # the publication and the cluster session.
        counts = {"poll": 4, "_on_fragment": 2, "offer": 2}
        for name in ("try_claim", "set_reserved_value", "offer", "commit", "abort", "poll", "_on_fragment", "values",
                     "session_id", "stream_id", "term_id", "term_offset", "frame_length", "flags", "type", "version",
                     "reserved_value", "_on_reply", "_on_entry", "reply", "schedule_timer", "cancel_timer"):
            self.edit(f"    def {name}(self", f"    def renamed_{name}(self", count=counts.get(name, 1))
        [finding] = lint.findings(self.tmp)
        self.assertIn("none of the per-message functions", finding)

    def test_one_missing_per_message_function_fails(self):
        self.edit("    def try_claim(self, length):", "    def claim_slice(self, length):")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Publication.try_claim is not in", finding)

    def test_a_missing_cluster_function_fails(self):
        """A cluster session's and a cluster service's per-message functions are held to
        the rules by name, as the others are (ADR 0124)."""
        self.edit("    def reply(self, session_id, message):", "    def answer(self, session_id, message):")
        self.edit("    def _on_reply(self", "    def _trampoline_of(self")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for name in ("ClusterService.reply", "ClusterSession._on_reply"):
            self.assertTrue(any(f"{name} is not in" in f for f in found), found)

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

    def test_the_committed_logs_throw_after_its_addon_call_passes_and_before_it_fails(self):
        """The committed log's `poll` in the Node client is held as the subscription's is."""
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit_node("    const result = addon.committedLogPoll(this._state, this.handle, handler, fragmentLimit);\n",
                       "    if (handler === null) throw new TypeError(\"a handler\");\n"
                       "    const result = addon.committedLogPoll(this._state, this.handle, handler, fragmentLimit);\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("CommittedLog.poll", finding)
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

    def test_the_cluster_polls_throw_after_their_addon_calls_passes_and_before_them_fails(self):
        """A cluster session's `poll` and a cluster service's in the Node client are held as
        the subscription's is (ADR 0124)."""
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit_node("    const result = addon.clusterSessionPoll(this._state, this.handle, handler, fragmentLimit);\n",
                       "    if (handler === null) throw new TypeError(\"a handler\");\n"
                       "    const result = addon.clusterSessionPoll(this._state, this.handle, handler, fragmentLimit);\n")
        service = ("    const result = addon.clusterServicePoll(this._state, this.handle, this._client.handle, handler, "
                   "fragmentLimit, this);\n")
        self.edit_node(service, "    if (handler === null) throw new TypeError(\"a handler\");\n" + service)
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for poll in ("ClusterSession.poll", "ClusterService.poll"):
            self.assertTrue(any(poll in f and "throws" in f for f in found), found)

    def test_the_no_action_error_built_before_a_cluster_services_addon_call_fails(self):
        """A value that is no action is kept by the addon and its `CavalryError` built once
        the poll has returned: `noAction` before the addon's call is a raise on the path."""
        service = ("    const result = addon.clusterServicePoll(this._state, this.handle, this._client.handle, handler, "
                   "fragmentLimit, this);\n")
        self.edit_node(service, "    const early = noAction(3);\n" + service)
        [finding] = lint.findings(self.tmp)
        self.assertIn("ClusterService.poll", finding)
        self.assertIn("calls noAction", finding)
        self.assertIn("noAction builds CavalryError", finding)

    def test_a_throw_in_a_node_cluster_per_message_function_fails(self):
        """The session's offer and the service's reply return a code, never throw; and the
        getters of a reply's values and of an entry's read through the addon alone."""
        self.edit_node("  offer(message) { return addon.cavalry_cluster_session_offer(this.handle, message); }\n",
                       "  offer(message) { throw new Error(\"no\"); }\n")
        self.edit_node("  reply(sessionId, message) { return addon.cavalry_cluster_service_reply(this.handle, sessionId, "
                       "message); }\n", "  reply(sessionId, message) { check(\"cavalry_cluster_service_reply\", -1); }\n")
        self.edit_node("  get timestamp() { return addon.replyField(this._state, 1); }\n",
                       "  get timestamp() { throw new Error(\"no\"); }\n")
        self.edit_node("  get timedOut() { return addon.entryField(this._state, 6); }\n",
                       "  get timedOut() { throw new Error(\"no\"); }\n")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 4, found)
        for name, form in (("ClusterSession.offer", "throws"), ("ClusterService.reply", "calls check"),
                           ("ClusterReplyValues.timestamp", "throws"), ("ClusterEntryValues.timedOut", "throws")):
            self.assertTrue(any(f.startswith(f"node: {name} (") and form in f for f in found), found)

    def test_a_throw_in_a_node_services_timer_calls_fails(self):
        """The schedule and the cancel of a timer return a code, never throw, in their own
        bodies (ADR 0157): what refuses an id or a deadline that is no int64 is the
        addon's argument reader, before the ABI is called."""
        self.edit_node("  scheduleTimer(correlationId, deadline) {\n",
                       "  scheduleTimer(correlationId, deadline) {\n    if (deadline < 0) throw new RangeError(\"no\");\n")
        self.edit_node("  cancelTimer(correlationId) { return addon.cavalry_cluster_service_cancel_timer(this.handle, "
                       "correlationId); }\n",
                       "  cancelTimer(correlationId) { check(\"cavalry_cluster_service_cancel_timer\", -1); }\n")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for name, form in (("ClusterService.scheduleTimer", "throws"), ("ClusterService.cancelTimer", "calls check")):
            self.assertTrue(any(f.startswith(f"node: {name} (") and form in f for f in found), found)

    def test_a_missing_node_timer_function_fails(self):
        self.edit_node("  scheduleTimer(correlationId, deadline) {", "  schedule(correlationId, deadline) {")
        self.edit_node("  cancelTimer(correlationId) {", "  cancel(correlationId) {")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for name in ("ClusterService.scheduleTimer", "ClusterService.cancelTimer"):
            self.assertTrue(any(f"{name} is not in" in f for f in found), found)

    def test_a_missing_node_cluster_function_fails(self):
        """A cluster session's and a cluster service's per-message functions are held to the
        rules by name, as the others are (ADR 0124)."""
        self.edit_node("  reply(sessionId, message) {", "  answer(sessionId, message) {")
        self.edit_node("  get leaderMemberId() {", "  get leader() {")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for name in ("ClusterService.reply", "ClusterEntryValues.leaderMemberId"):
            self.assertTrue(any(f"{name} is not in" in f for f in found), found)

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

    def test_a_c11_mtx_in_the_committed_logs_poll_fails(self):
        self.add_to_addon("fn_committedLogPoll", "mtx_lock(&lock);")
        [finding] = lint.findings(self.tmp)
        self.assertIn("node: fn_committedLogPoll (", finding)
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

    # A cluster session's poll and a cluster service's in the addon (ADR 0124).

    def test_a_pthread_mutex_in_the_cluster_trampoline_fails(self):
        self.add_to_addon("cluster_trampoline", "pthread_mutex_lock(&lock);")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 1, found)
        self.assertIn("node: cluster_trampoline (", found[0])
        self.assertIn("calls `pthread_mutex_*`", found[0])

    def test_a_throw_where_the_cluster_trampoline_reads_the_action_fails(self):
        """A value that is no action is kept for the poll to report, never thrown from inside
        the ABI's call."""
        self.add_to_addon("cluster_action", "napi_throw_error(s->env, NULL, \"no action\");")
        self.assert_addon_finding("cluster_trampoline", "calls cluster_action")
        self.assert_addon_finding("cluster_trampoline", "cluster_action throws, `napi_throw*`")

    def test_a_c11_mtx_in_either_cluster_poll_fails(self):
        self.add_to_addon("fn_clusterSessionPoll", "mtx_lock(&lock);")
        self.add_to_addon("fn_clusterServicePoll", "mtx_lock(&lock);")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for poll in ("fn_clusterSessionPoll", "fn_clusterServicePoll"):
            self.assertTrue(any(f.startswith(f"node: {poll} (") and "calls C11's `mtx_*`" in f for f in found), found)

    def test_a_uv_rwlock_in_the_reply_or_the_entry_field_fails(self):
        self.add_to_addon("fn_replyField", "uv_rwlock_rdlock(&lock);")
        self.add_to_addon("fn_entryField", "uv_rwlock_rdlock(&lock);")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for field in ("fn_replyField", "fn_entryField"):
            self.assertTrue(any(f.startswith(f"node: {field} (") and "calls libuv's `uv_rwlock_*`" in f for f in found),
                            found)

    def test_a_throw_in_what_a_cluster_poll_reports_fails(self):
        """cluster_report is what both cluster polls call once the ABI has returned; it
        reads no argument, so its throw is a raise on the path of each."""
        self.add_to_addon("cluster_report", "napi_throw_type_error(env, NULL, \"no\");")
        for poll in ("fn_clusterSessionPoll", "fn_clusterServicePoll"):
            self.assert_addon_finding(poll, "calls cluster_report")
            self.assert_addon_finding(poll, "cluster_report throws, `napi_throw*`")

    def test_a_throw_in_a_cluster_wrapper_fails(self):
        """The wrappers of the session's offer and of the service's reply and action."""
        wrappers = ("fn_cavalry_cluster_session_offer", "fn_cavalry_cluster_service_reply",
                    "fn_cavalry_cluster_service_action")
        for wrapper in wrappers:
            self.add_to_addon(wrapper, "napi_throw_error(env, NULL, \"no\");")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 3, found)
        for wrapper in wrappers:
            self.assertTrue(any(f.startswith(f"node: {wrapper} (") and "throws, `napi_throw*`" in f for f in found), found)

    def test_an_addon_reader_is_for_an_entry_function_before_its_abi_call(self):
        """The addon's argument readers throw, and their throw is none on the path where
        it refuses an argument: in a function N-API calls with the caller's arguments,
        one the addon's table of properties names, itself and before the ABI call it goes
        on from (ADR 0162). A reader called by a trampoline, by a function a trampoline
        calls whatever its name, through a helper of an entry function, or by an entry
        function after its ABI call, is a finding, for each kind of reader. The poll from inside a handler,
        which returns the ABI's refusal, is no call the function goes on from: the
        handler's type is read after it in the text, and the committed addon has no
        finding."""
        self.assertEqual(lint.findings(self.tmp), [])
        for function, line, said, why in (
                ("trampoline", "(void) int_arg(s->env, NULL, \"length\", 0, 1);", "trampoline calls the argument reader int_arg",
                 "raised into a call-back"),
                ("cluster_trampoline", "(void) size_arg(s->env, NULL, \"length\");",
                 "cluster_trampoline calls the argument reader size_arg", "raised into a call-back"),
                # A function a trampoline calls, one call down and two.
                ("cluster_action", "(void) int32_arg(s->env, result, \"action\");",
                 "cluster_action calls the argument reader int32_arg", "by no function between"),
                ("keep_failure", "(void) int64_arg(s->env, NULL, \"position\");",
                 "keep_failure calls the argument reader int64_arg", "by no function between")):
            with self.subTest(function=function):
                self.setUp()
                self.add_to_addon(function, line)
                found = lint.findings(self.tmp)
                self.assertTrue(found, function)
                self.assertTrue(all(said in f and why in f for f in found), found)
        # After the ABI's call, in the entry functions: a poll's own, and a wrapper's.
        for old, new, said in (
                ("  int count = cavalry_subscription_poll(subscription, trampoline, s, limit);\n",
                 "  int count = cavalry_subscription_poll(subscription, trampoline, s, limit);\n"
                 "  (void) size_arg(env, argv[3], \"fragmentLimit\");\n",
                 "fn_poll calls the argument reader size_arg"),
                ("  int result = cavalry_cluster_service_cancel_timer(service, correlation_id);\n",
                 "  int result = cavalry_cluster_service_cancel_timer(service, correlation_id);\n"
                 "  (void) int64_exact_arg(env, argv[1], \"correlation_id\");\n",
                 "fn_cavalry_cluster_service_cancel_timer calls the argument reader int64_exact_arg")):
            with self.subTest(said=said):
                self.setUp()
                self.edit(old, new, file=self.addon)
                [finding] = lint.findings(self.tmp)
                self.assertIn(said, finding)
                self.assertIn("after its call of the ABI", finding)
        # What a function is named does not make it an entry: one that the addon does not
        # give N-API, called by a trampoline, is a function a trampoline calls.
        handler = "static void trampoline(void *clientd, const uint8_t *buffer, size_t length, const cavalry_header_t *header) {\n"
        self.setUp()
        self.edit(handler, "static void fn_check(napi_env env) { (void) int_arg(env, NULL, \"length\", 0, 1); }\n\n"
                  + handler + "  fn_check(NULL);\n", file=self.addon)
        found = lint.findings(self.tmp)
        self.assertTrue(found and all("trampoline calls fn_check at line" in f and "fn_check calls the argument reader "
                                      "int_arg" in f and "by no function between" in f for f in found), found)
        # An entry function reads its arguments itself: a helper between it and a reader,
        # before the ABI's call or after it, is a finding.
        poll = "  int count = cavalry_subscription_poll(subscription, trampoline, s, limit);\n"
        entry = "static napi_value fn_poll(napi_env env, napi_callback_info info) {\n"
        self.setUp()
        self.edit(entry, "static size_t limit_of(napi_env env, napi_value v) { return size_arg(env, v, \"fragmentLimit\"); }\n\n"
                  + entry + "  (void) limit_of(env, NULL);\n", file=self.addon)
        [finding] = lint.findings(self.tmp)
        self.assertIn("fn_poll calls limit_of at line", finding)
        self.assertIn("limit_of calls the argument reader size_arg", finding)
        # The ABI's call is the ABI's wherever it is made: through a function of the
        # addon, on the same line as the reader, and the first of two.
        for old, new in (
                (poll, "  int count = poll_once(subscription, s, limit);\n  (void) size_arg(env, argv[3], \"fragmentLimit\");\n"),
                (poll, poll.rstrip("\n") + " (void) size_arg(env, argv[3], \"fragmentLimit\");\n"),
                (poll, poll + "  (void) size_arg(env, argv[3], \"fragmentLimit\");\n" + poll.replace("int count", "count"))):
            with self.subTest(new=new):
                self.setUp()
                self.edit(old, new, file=self.addon)
                if "poll_once" in new:
                    self.edit(entry, "static int poll_once(cavalry_subscription_t *subscription, subscription_state_t *s, "
                              "size_t limit) {\n  return cavalry_subscription_poll(subscription, trampoline, s, limit);\n}\n\n"
                              + entry, file=self.addon)
                [finding] = lint.findings(self.tmp)
                self.assertIn("fn_poll calls the argument reader size_arg", finding)
                self.assertIn("after its call of the ABI", finding)
        # A reader's own call of another reader is its own, and a reader that is no
        # reader by the rule is a throw wherever it is called.
        self.setUp()
        with open(self.addon, encoding="utf-8") as f:
            text = f.read()
        self.assertEqual(text.count("int_arg("), 2)  # its definition, and `int32_arg`'s call
        self.assertEqual(text.count("int64_read("), 4)  # its definition, and the three int64 readers' calls

    def test_a_throw_in_a_timer_wrapper_fails_and_its_exact_reader_may_throw(self):
        """The addon's wrappers of the schedule and the cancel of a timer are on the path
        (ADR 0157). `int64_exact_arg` reads a timer's id and its deadline through
        `int64_read`, which refuses a number that is no integer a number carries exactly
        and is an argument reader: its throw, before the ABI is called, is none on the
        path, and the committed addon has no finding. The same reader with another name
        is no reader, and its throw is one."""
        self.assertEqual(lint.findings(self.tmp), [])
        wrappers = ("fn_cavalry_cluster_service_schedule_timer", "fn_cavalry_cluster_service_cancel_timer")
        for wrapper in wrappers:
            self.add_to_addon(wrapper, "napi_throw_error(env, NULL, \"no\");")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for wrapper in wrappers:
            self.assertTrue(any(f.startswith(f"node: {wrapper} (") and "throws, `napi_throw*`" in f for f in found), found)
        self.setUp()
        with open(self.addon, encoding="utf-8") as f:
            text = f.read()
        self.assertEqual(text.count("int64_exact_arg("), 4)  # its definition and the three arguments it reads
        self.assertEqual(text.count("int64_read("), 4)  # its definition, and the call of each of the three int64 readers
        with open(self.addon, "w", encoding="utf-8") as f:
            f.write(text.replace("int64_read(", "int64_strict("))
        found = lint.findings(self.tmp)
        for wrapper in wrappers:
            self.assertTrue(any(f.startswith(f"node: {wrapper} (") and "calls int64_strict" in f for f in found), found)

    def test_a_missing_addon_cluster_function_fails(self):
        self.edit("static void cluster_trampoline(", "static void cluster_handler(", file=self.addon)
        found = lint.findings(self.tmp)
        self.assertTrue(any("the addon's per-message function cluster_trampoline is not in" in f for f in found), found)

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

    def test_an_allocation_in_the_go_committed_logs_poll_fails(self):
        """The committed log's Poll in the Go client is held as the subscription's is, and
        the *HandlerError it builds after its ABI call is the one error it may build."""
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit_go("func (l *CommittedLog) Poll(handler EntryHandler, fragmentLimit int) (int, error) {\n",
                     "func (l *CommittedLog) Poll(handler EntryHandler, fragmentLimit int) (int, error) {\n"
                     "\t_ = make([]byte, 1)\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("CommittedLog.Poll", finding)
        self.assertIn("allocates", finding)

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

    def test_a_panic_in_the_go_entry_trampoline_fails(self):
        self.edit_go("\ts.deliverEntry(body, int64(position))\n",
                     "\tpanic(\"x\")\n\ts.deliverEntry(body, int64(position))\n", file=self.go_trampoline)
        [finding] = lint.findings(self.tmp)
        self.assertIn("cavalryGoEntry", finding)
        self.assertIn("trampoline.go", finding)
        self.assertIn("panics", finding)

    def test_a_lock_in_the_go_shims_entry_trampoline_fails(self):
        call = "  cavalryGoEntry((uintptr_t) clientd, (uint8_t *) buffer, length, (cavalry_header_t *) header,\n"
        self.edit_go(call, "  pthread_mutex_lock(&lock);\n" + call, file=self.go_shim)
        [finding] = lint.findings(self.tmp)
        self.assertIn("cavalry_go_entry_trampoline", finding)
        self.assertIn("pthread_mutex_*", finding)

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
                     "cavalry_go_offer", "cavalry_go_poll", "cavalry_go_committed_log_poll",
                     "cavalry_go_cluster_session_offer", "cavalry_go_cluster_session_poll",
                     "cavalry_go_cluster_service_reply", "cavalry_go_cluster_service_poll",
                     "cavalry_go_cluster_service_schedule_timer", "cavalry_go_cluster_service_cancel_timer",
                     "cavalry_go_service_trampoline"):
            self.assertTrue(any(f"go: {name} " in f and "calls cavalry_go_keep" in f and "pthread_mutex_*" in f
                                for f in found), (name, found))

    # A cluster session and a cluster service in the Go client (ADR 0124).

    def test_the_handler_error_before_a_go_cluster_polls_shim_call_fails(self):
        """A cluster session's Poll and a cluster service's are held as the subscription's
        is: the *HandlerError each builds after its shim call is the one error it may build."""
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit_go("\ts.reply = handler\n\ts.polling = true\n",
                     "\t_ = &HandlerError{}\n\ts.reply = handler\n\ts.polling = true\n")
        self.edit_go("\ts.service = handler\n\ts.polling = true\n",
                     "\t_ = keptError(&s.err)\n\ts.service = handler\n\ts.polling = true\n")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        self.assertTrue(any("ClusterSession.Poll" in f and "builds HandlerError" in f for f in found), found)
        self.assertTrue(any("ClusterService.Poll" in f and "calls keptError" in f and "builds Error" in f for f in found),
                        found)

    def test_the_clients_mutex_in_a_go_cluster_services_poll_fails(self):
        """A service's Poll is a call on its client and is on every entry's path: it takes
        no lock, the client's own among them (clients/go/README.md, "Threads")."""
        self.edit_go("\ts.service = handler\n\ts.polling = true\n",
                     "\tv.client.mu.Lock()\n\tdefer v.client.mu.Unlock()\n\ts.service = handler\n\ts.polling = true\n")
        [finding] = lint.findings(self.tmp)
        self.assertIn("ClusterService.Poll", finding)
        self.assertIn("takes a lock with `.Lock()`", finding)
        self.assertIn("no lock on the per-message path", finding)

    def test_a_panic_in_a_go_cluster_per_message_function_fails(self):
        """The session's Offer and the service's Reply return a code, and neither exported
        trampoline panics: a handler's own panic is recovered in the function it calls."""
        self.edit_go("func (n *ClusterSession) Offer(message []byte) int64 {\n",
                     "func (n *ClusterSession) Offer(message []byte) int64 {\n\tpanic(\"no\")\n")
        self.edit_go("func (v *ClusterService) Reply(sessionID int64, message []byte) int32 {\n",
                     "func (v *ClusterService) Reply(sessionID int64, message []byte) int32 {\n\tpanic(\"no\")\n")
        self.edit_go("\ts.deliverReply(body)\n", "\tpanic(\"x\")\n\ts.deliverReply(body)\n", file=self.go_trampoline)
        self.edit_go("\te := &s.entryValues\n", "\tpanic(\"x\")\n\te := &s.entryValues\n", file=self.go_trampoline)
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 4, found)
        for name in ("ClusterSession.Offer", "ClusterService.Reply", "cavalryGoReply", "cavalryGoClusterEntry"):
            self.assertTrue(any(f"go: {name} (" in f and "panics" in f for f in found), (name, found))

    def test_a_panic_in_a_go_services_timer_calls_fails_and_so_does_a_missing_one(self):
        """ScheduleTimer and CancelTimer return a code (ADR 0157): neither panics, and
        both are held to the rules by name."""
        self.edit_go("func (v *ClusterService) ScheduleTimer(correlationID, deadline int64) int32 {\n",
                     "func (v *ClusterService) ScheduleTimer(correlationID, deadline int64) int32 {\n\tpanic(\"no\")\n")
        self.edit_go("func (v *ClusterService) CancelTimer(correlationID int64) int32 {\n",
                     "func (v *ClusterService) CancelTimer(correlationID int64) int32 {\n\tpanic(\"no\")\n")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for name in ("ClusterService.ScheduleTimer", "ClusterService.CancelTimer"):
            self.assertTrue(any(f"go: {name} (" in f and "panics" in f for f in found), (name, found))
        self.setUp()
        self.edit_go("func (v *ClusterService) ScheduleTimer(", "func (v *ClusterService) Schedule(")
        self.edit_go("func (v *ClusterService) CancelTimer(", "func (v *ClusterService) Cancel(")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for name in ("ClusterService.ScheduleTimer is not in", "ClusterService.CancelTimer is not in"):
            self.assertTrue(any(name in f for f in found), (name, found))

    def test_an_allocation_where_go_calls_a_cluster_handler_fails(self):
        """deliverReply and deliverClusterEntry call the handlers, once per reply and per
        entry: an allocation there fails them and the exported trampoline that calls each."""
        self.edit_go("\tdefer s.recovered()\n\ts.reply(body, &s.replyValues)\n",
                     "\tdefer s.recovered()\n\t_ = make([]byte, 1)\n\ts.reply(body, &s.replyValues)\n")
        self.edit_go("\taction = ActionAbort\n", "\taction = ActionAbort\n\t_ = new(int)\n")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 4, found)
        for name in ("Subscription.deliverReply", "Subscription.deliverClusterEntry"):
            self.assertTrue(any(f"go: {name} (" in f and "allocates" in f for f in found), (name, found))
        for trampoline, calls in (("cavalryGoReply", "Subscription.deliverReply"),
                                  ("cavalryGoClusterEntry", "Subscription.deliverClusterEntry")):
            self.assertTrue(any(f"go: {trampoline} (" in f and f"calls {calls}" in f for f in found), (trampoline, found))

    def test_a_lock_in_the_go_shims_cluster_trampolines_fails(self):
        reply = "  cavalry_cluster_reply_values(header, &values);\n"
        self.edit_go(reply, "  pthread_mutex_lock(&lock);\n" + reply, file=self.go_shim)
        entry = "  if (cavalry_cluster_entry_values(header, &values) != 0) {\n"
        self.edit_go(entry, "  pthread_mutex_lock(&lock);\n" + entry, file=self.go_shim)
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for trampoline in ("cavalry_go_reply_trampoline", "cavalry_go_service_trampoline"):
            self.assertTrue(any(f"go: {trampoline} (" in f and "pthread_mutex_*" in f for f in found), (trampoline, found))

    def test_a_missing_go_cluster_function_fails(self):
        """A cluster session's and a cluster service's per-message functions are held to the
        rules by name, as the others are (ADR 0124)."""
        self.edit_go("func (v *ClusterService) Reply(", "func (v *ClusterService) Answer(")
        self.edit_go("func cavalryGoClusterEntry(", "func cavalryGoServiceEntry(", file=self.go_trampoline)
        self.edit_go("static void cavalry_go_service_trampoline(", "static void cavalry_go_service_handler(",
                     file=self.go_shim)
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 3, found)
        for name in ("ClusterService.Reply is not in", "cavalryGoClusterEntry is not in",
                     "cavalry_go_service_trampoline is not in"):
            self.assertTrue(any(name in f for f in found), (name, found))

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
        self.edit_jvm("    public long tryClaim(int length) {\n        if (length < 0) {\n            Abi.negative(\"length\", length);\n        }\n        try {\n",
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

    def test_a_throw_in_the_committed_logs_catch_in_jvm_fails(self):
        """The committed log's upcall is held as the subscription's is: the entry's position
        is read in a helper with a try of its own, so the catch around the handler's call
        holds no invokeExact, and a throw there is a raise."""
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit_jvm("        } catch (Throwable t) {\n            raised++;\n",
                      "        } catch (Throwable t) {\n            raised++;\n            throw t;\n", file="CommittedLog.java")
        [finding] = lint.findings(self.tmp)
        self.assertIn("CommittedLog.onEntry", finding)
        self.assertIn("throws", finding)

    def test_building_handler_exception_before_the_committed_logs_call_in_jvm_fails(self):
        self.edit_jvm("        int count;\n        try {\n            count = (int) Abi.cavalry_committed_log_poll.invokeExact(",
                      "        int count;\n        RuntimeException early = new HandlerException(0, 0, first);\n        try {\n"
                      "            count = (int) Abi.cavalry_committed_log_poll.invokeExact(", file="CommittedLog.java")
        found = lint.findings(self.tmp)
        self.assertTrue(any("CommittedLog.poll" in f and "HandlerException" in f for f in found), found)

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
        self.edit_jvm("    public long tryClaim(int length) {\n        if (length < 0) {\n            Abi.negative(\"length\", length);\n        }\n        try {\n",
                      "    public long tryClaim(int length) {\n        if (length < 0) {\n            Abi.negative(\"length\", length);\n        }\n        Abi.check(\"cavalry_publication_try_claim\", 1);\n        try {\n")
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

    def test_the_jvm_reader_of_a_count_is_for_the_functions_named_before_the_abi_call(self):
        """`Abi.negative` throws for a count or a length below zero, before the ABI is
        called, and its throw is none on the path of the claim and the four polls, which
        the rule names with it (ADR 0164). The allowance is that and no more: another
        per-message method that calls it, a call of it after the method's ABI call, a
        poll's among them, and any other form in the reader, are findings; and a rule
        that names a reader the package does not have is stale."""
        self.assertEqual(lint.findings(self.tmp), [])
        # Another per-message method, and a trampoline: no reader is named for them.
        self.edit_jvm("    public long offer(MemorySegment message) {\n        try {\n",
                      "    public long offer(MemorySegment message) {\n        Abi.negative(\"length\", 0);\n        try {\n")
        [finding] = lint.findings(self.tmp)
        self.assertTrue("Publication.offer" in finding and "calls Abi.negative" in finding and "throws" in finding, finding)
        self.setUp()
        upcall = "    void onFragment(MemorySegment clientd, MemorySegment buffer, long length, MemorySegment frame) {\n"
        self.edit_jvm(upcall, upcall + "        Abi.negative(\"length\", 0);\n", file="Subscription.java")
        found = lint.findings(self.tmp)
        self.assertTrue(found and all("calls Abi.negative" in f and "throws" in f for f in found), found)
        # After the ABI's call: in the claim, and in a poll, which may raise there for its
        # handler and for nothing else.
        self.setUp()
        claim = "            long position = (long) Abi.cavalry_publication_try_claim.invokeExact(handle, (long) length, claimStruct);\n"
        self.edit_jvm(claim, claim + "            Abi.negative(\"length\", length);\n")
        [finding] = lint.findings(self.tmp)
        self.assertTrue("Publication.tryClaim" in finding and "calls the argument reader Abi.negative" in finding
                        and "not before its call of the ABI" in finding, finding)
        self.setUp()
        poll = "            count = (int) Abi.cavalry_subscription_poll.invokeExact(handle, stub, MemorySegment.NULL, (long) fragmentLimit);\n"
        self.edit_jvm(poll, poll + "            Abi.negative(\"fragmentLimit\", fragmentLimit);\n", file="Subscription.java")
        [finding] = lint.findings(self.tmp)
        self.assertTrue("Subscription.poll" in finding and "not before its call of the ABI" in finding, finding)
        # The reader's own throw and nothing else.
        self.setUp()
        reader = "    static void negative(String name, int value) {\n"
        self.edit_jvm(reader, reader + "        synchronized (Abi.class) {\n        }\n", file="Abi.java")
        found = lint.findings(self.tmp)
        for name in ("Publication.tryClaim", "Subscription.poll", "CommittedLog.poll", "ClusterSession.poll",
                     "ClusterService.poll"):
            self.assertTrue(any(name in f and "Abi.negative" in f and "synchronized" in f for f in found), found)
        self.assertEqual(len(found), 5, found)
        # A reader the package does not have.
        self.setUp()
        self.edit_jvm(reader, "    static void belowZero(String name, int value) {\n", file="Abi.java")
        found = lint.findings(self.tmp)
        self.assertTrue(any("the argument reader Abi.negative is not in" in f for f in found), found)
        # On the ABI call's own line, after it: not before it.
        self.setUp()
        self.edit_jvm(claim, claim.rstrip("\n") + " Abi.negative(\"length\", length);\n")
        [finding] = lint.findings(self.tmp)
        self.assertTrue("Publication.tryClaim" in finding and "not before its call of the ABI" in finding, finding)
        # What a function the reader calls does is on the path of the reader's callers.
        self.setUp()
        self.edit_jvm(reader, "    static void held() {\n        synchronized (Abi.class) {\n        }\n    }\n\n" + reader
                      + "        held();\n", file="Abi.java")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 5, found)
        self.assertTrue(all("Abi.negative calls Abi.held" in f and "synchronized" in f for f in found), found)
        # A poll calls the ABI in two places, the nested poll's refusal and its own: a reader
        # between them is after the method's first call of the ABI.
        self.setUp()
        nested = ("                return (int) Abi.cavalry_subscription_poll.invokeExact(handle, stub, MemorySegment.NULL, "
                  "(long) fragmentLimit);\n            } catch (Throwable t) {\n                throw "
                  "Abi.failed(\"cavalry_subscription_poll\", t);\n            }\n        }\n")
        self.edit_jvm(nested, nested + "        Abi.negative(\"fragmentLimit\", fragmentLimit);\n", file="Subscription.java")
        [finding] = lint.findings(self.tmp)
        self.assertTrue("Subscription.poll" in finding and "not before its call of the ABI" in finding, finding)
        # And the rule names the claim and the four polls, and no other method.
        self.assertEqual(lint.RULES["jvm"]["readers"], {"Abi.negative": [
            "Publication.tryClaim", "Subscription.poll", "CommittedLog.poll", "ClusterSession.poll", "ClusterService.poll"]})

    # A cluster session and a cluster service in the JVM client (ADR 0124).

    def test_building_handler_exception_before_a_cluster_polls_call_in_jvm_fails(self):
        """A cluster session's poll and a cluster service's are held as the subscription's
        is: the exception each builds after its ABI call is the one it may build."""
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit_jvm("        int count;\n        try {\n            count = (int) Abi.cavalry_cluster_session_poll.invokeExact(",
                      "        int count;\n        RuntimeException early = new HandlerException(0, 0, first);\n        try {\n"
                      "            count = (int) Abi.cavalry_cluster_session_poll.invokeExact(", file="ClusterSession.java")
        self.edit_jvm("        int count;\n        try {\n            count = (int) Abi.cavalry_cluster_service_poll.invokeExact(",
                      "        int count;\n        RuntimeException early = new CavalryException(\"x\", 0, \"y\");\n        try {\n"
                      "            count = (int) Abi.cavalry_cluster_service_poll.invokeExact(", file="ClusterService.java")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        self.assertTrue(any("ClusterSession.poll" in f and "builds HandlerException" in f for f in found), found)
        self.assertTrue(any("ClusterService.poll" in f and "builds CavalryException" in f for f in found), found)

    def test_a_lock_in_a_jvm_cluster_services_poll_fails(self):
        """A service's poll is a call on its client and is on every entry's path: it is
        not `synchronized`, and it calls no `synchronized` method of the client, whose
        handle it reads through the one accessor that takes no lock
        (clients/jvm/README.md, "Threads")."""
        self.edit_jvm("    public int poll(ServiceHandler handler, int fragmentLimit) {\n",
                      "    public synchronized int poll(ServiceHandler handler, int fragmentLimit) {\n", file="ClusterService.java")
        [finding] = lint.findings(self.tmp)
        self.assertIn("ClusterService.poll", finding)
        self.assertIn("is `synchronized`", finding)
        self.setUp()
        self.edit_jvm("        this.handler = handler;\n        polling = true;\n        raised = 0;\n        first = null;\n"
                      "        refused = false;\n",
                      "        boolean closed = client.isClosed();\n        this.handler = handler;\n        polling = true;\n"
                      "        raised = 0;\n        first = null;\n        refused = false;\n", file="ClusterService.java")
        [finding] = lint.findings(self.tmp)
        self.assertIn("ClusterService.poll", finding)
        self.assertIn("calls Client.isClosed", finding)
        self.assertIn("is `synchronized`", finding)

    def test_a_throw_in_a_jvm_cluster_upcalls_catch_fails(self):
        """Each cluster upcall reads its values in a helper with a try of its own, so the
        catch around the handler's call holds no invokeExact, and a throw there is a raise."""
        self.assertEqual(lint.findings(self.tmp), [])
        for file in ("ClusterSession.java", "ClusterService.java"):
            self.edit_jvm("        } catch (Throwable t) {\n            raised++;\n",
                          "        } catch (Throwable t) {\n            raised++;\n            throw t;\n", file=file)
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for upcall in ("ClusterSession.onReply", "ClusterService.onClusterEntry"):
            self.assertTrue(any(f"jvm: {upcall} (" in f and "throws" in f for f in found), (upcall, found))

    def test_an_exception_built_in_a_jvm_cluster_services_upcall_fails(self):
        """An action the ABI refused is kept as its code and its message for `poll` to
        throw: the upcall builds no exception for it."""
        self.edit_jvm("            refusedMessage = Abi.errmsg();\n",
                      "            first = Abi.lastError(\"cavalry_cluster_service_action\");\n", file="ClusterService.java")
        [finding] = lint.findings(self.tmp)
        self.assertIn("ClusterService.onClusterEntry", finding)
        self.assertIn("calls Abi.lastError", finding)
        self.assertIn("builds CavalryException", finding)

    def test_a_throw_or_an_allocation_in_a_jvm_cluster_per_message_method_fails(self):
        """The session's offer and the service's reply return a code, and the getters of a
        reply's values and of an entry's read the one struct their object keeps."""
        self.edit_jvm("    public long offer(MemorySegment message) {\n",
                      "    public long offer(MemorySegment message) {\n        Abi.check(\"x\", -1);\n", file="ClusterSession.java")
        self.edit_jvm("    public int reply(long sessionId, MemorySegment message) {\n",
                      "    public int reply(long sessionId, MemorySegment message) {\n        Abi.check(\"x\", -1);\n",
                      file="ClusterService.java")
        self.edit_jvm("    public long timestamp() {\n", "    public long timestamp() {\n        byte[] copy = new byte[8];\n",
                      file="ClusterReplyValues.java")
        self.edit_jvm("    public boolean timedOut() {\n", "    public boolean timedOut() {\n        byte[] copy = new byte[8];\n",
                      file="ClusterEntryValues.java")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 4, found)
        for name, form in (("ClusterSession.offer", "calls Abi.check"), ("ClusterService.reply", "calls Abi.check"),
                           ("ClusterReplyValues.timestamp", "allocates byte with `new`"),
                           ("ClusterEntryValues.timedOut", "allocates byte with `new`")):
            self.assertTrue(any(f"jvm: {name} (" in f and form in f for f in found), (name, found))

    def test_a_throw_in_a_jvm_services_timer_methods_fails_and_so_does_a_missing_one(self):
        """scheduleTimer and cancelTimer return a code (ADR 0157): a check that throws in
        the one and an allocation in the other are findings, and both are held to the
        rules by name."""
        self.edit_jvm("    public int scheduleTimer(long correlationId, long deadline) {\n",
                      "    public int scheduleTimer(long correlationId, long deadline) {\n        Abi.check(\"x\", -1);\n",
                      file="ClusterService.java")
        self.edit_jvm("    public int cancelTimer(long correlationId) {\n",
                      "    public int cancelTimer(long correlationId) {\n        long[] copy = new long[1];\n",
                      file="ClusterService.java")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for name, form in (("ClusterService.scheduleTimer", "calls Abi.check"),
                           ("ClusterService.cancelTimer", "allocates long with `new`")):
            self.assertTrue(any(f"jvm: {name} (" in f and form in f for f in found), (name, found))
        self.setUp()
        self.edit_jvm("    public int scheduleTimer(long correlationId, long deadline) {\n",
                      "    public int schedule(long correlationId, long deadline) {\n", file="ClusterService.java")
        self.edit_jvm("    public int cancelTimer(long correlationId) {\n",
                      "    public int cancel(long correlationId) {\n", file="ClusterService.java")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for name in ("ClusterService.scheduleTimer", "ClusterService.cancelTimer"):
            self.assertTrue(any(f"{name} is not in" in f for f in found), (name, found))

    def test_a_missing_jvm_cluster_method_fails(self):
        """A cluster session's and a cluster service's per-message methods are held to the
        rules by name, as the others are (ADR 0124)."""
        self.edit_jvm("    public int reply(long sessionId, MemorySegment message) {\n",
                      "    public int answer(long sessionId, MemorySegment message) {\n", file="ClusterService.java")
        self.edit_jvm("    void onReply(MemorySegment clientd,", "    void onAnswer(MemorySegment clientd,", file="ClusterSession.java")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for name in ("ClusterService.reply", "ClusterSession.onReply"):
            self.assertTrue(any(f"{name} is not in" in f for f in found), (name, found))

    def test_a_jvm_package_without_the_per_message_methods_fails(self):
        for name in ("Publication", "Subscription", "CommittedLog", "Header", "ClusterSession", "ClusterService",
                     "ClusterReplyValues", "ClusterEntryValues"):
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

    def test_the_dotnet_reader_of_a_count_is_for_the_functions_named_before_the_abi_call(self):
        """`Abi.Negative` throws for a count or a length below zero, before the ABI is
        called, and its throw is none on the path of the claim and the four polls, which
        the rule names with it (ADR 0164), as the JVM client's `Abi.negative`. Another
        per-message member that calls it, a call of it after the member's ABI call, a
        poll's among them, and any other form in the reader, are findings; and a rule
        that names a reader the package does not have is stale."""
        self.assertEqual(lint.findings(self.tmp), [])
        offer = "    public long Offer(ReadOnlySpan<byte> message)\n    {\n"
        self.edit_dotnet(offer, offer + "        Abi.Negative(\"length\", 0);\n")
        [finding] = lint.findings(self.tmp)
        self.assertTrue("Publication.Offer" in finding and "calls Abi.Negative" in finding and "throws" in finding, finding)
        self.setUp()
        deliver = "    private void Deliver(byte* buffer, nuint length, void* frame)\n    {\n"
        self.edit_dotnet(deliver, deliver + "        Abi.Negative(\"length\", 0);\n", file="Subscription.cs")
        found = lint.findings(self.tmp)
        self.assertTrue(found and all("calls Abi.Negative" in f and "throws" in f for f in found), found)
        self.setUp()
        claim = "        long position = Abi.cavalry_publication_try_claim(handle, (nuint) length, ref claim);\n"
        self.edit_dotnet(claim, claim + "        Abi.Negative(nameof(length), length);\n")
        [finding] = lint.findings(self.tmp)
        self.assertTrue("Publication.TryClaim" in finding and "calls the argument reader Abi.Negative" in finding
                        and "not before its call of the ABI" in finding, finding)
        self.setUp()
        poll = ("        int count = Abi.cavalry_subscription_poll(handle, &OnFragment, GCHandle.ToIntPtr(self), "
                "(nuint) fragmentLimit);\n")
        self.edit_dotnet(poll, poll + "        Abi.Negative(nameof(fragmentLimit), fragmentLimit);\n", file="Subscription.cs")
        [finding] = lint.findings(self.tmp)
        self.assertTrue("Subscription.Poll" in finding and "not before its call of the ABI" in finding, finding)
        self.setUp()
        reader = "    internal static void Negative(string name, int value)\n    {\n"
        self.edit_dotnet(reader, reader + "        lock (typeof(Abi))\n        {\n        }\n", file="Abi.cs")
        found = lint.findings(self.tmp)
        for name in ("Publication.TryClaim", "Subscription.Poll", "CommittedLog.Poll", "ClusterSession.Poll",
                     "ClusterService.Poll"):
            self.assertTrue(any(name in f and "Abi.Negative" in f and "lock" in f for f in found), found)
        self.assertEqual(len(found), 5, found)
        self.setUp()
        self.edit_dotnet(reader, "    internal static void BelowZero(string name, int value)\n    {\n", file="Abi.cs")
        found = lint.findings(self.tmp)
        self.assertTrue(any("the argument reader Abi.Negative is not in" in f for f in found), found)
        # On the ABI call's own line, after it: not before it.
        self.setUp()
        self.edit_dotnet(claim, claim.rstrip("\n") + " Abi.Negative(nameof(length), length);\n")
        [finding] = lint.findings(self.tmp)
        self.assertTrue("Publication.TryClaim" in finding and "not before its call of the ABI" in finding, finding)
        # What a function the reader calls does is on the path of the reader's callers.
        self.setUp()
        self.edit_dotnet(reader, "    internal static void Held()\n    {\n        lock (typeof(Abi))\n        {\n        }\n    }\n\n"
                         + reader + "        Held();\n", file="Abi.cs")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 5, found)
        self.assertTrue(all("Abi.Negative calls Abi.Held" in f and "lock" in f for f in found), found)
        # A poll calls the ABI in two places, the nested poll's refusal and its own: a reader
        # between them is after the member's first call of the ABI.
        self.setUp()
        nested = ("            return Abi.cavalry_subscription_poll(handle, &OnFragment, GCHandle.ToIntPtr(self), "
                  "(nuint) fragmentLimit);\n        }\n")
        self.edit_dotnet(nested, nested + "        Abi.Negative(nameof(fragmentLimit), fragmentLimit);\n",
                         file="Subscription.cs")
        [finding] = lint.findings(self.tmp)
        self.assertTrue("Subscription.Poll" in finding and "not before its call of the ABI" in finding, finding)
        # And the rule names the claim and the four polls, and no other member.
        self.assertEqual(lint.RULES["dotnet"]["readers"], {"Abi.Negative": [
            "Publication.TryClaim", "Subscription.Poll", "CommittedLog.Poll", "ClusterSession.Poll", "ClusterService.Poll"]})

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

    def test_building_handler_exception_before_the_polls_call_in_dotnet_fails(self):
        self.edit_dotnet("        int count = Abi.cavalry_subscription_poll(handle, &OnFragment, GCHandle.ToIntPtr(self), (nuint) fragmentLimit);\n",
                         "        HandlerException early = new HandlerException(0, 0, first!);\n"
                         "        int count = Abi.cavalry_subscription_poll(handle, &OnFragment, GCHandle.ToIntPtr(self), (nuint) fragmentLimit);\n",
                         file="Subscription.cs")
        [finding] = lint.findings(self.tmp)
        self.assertIn("Subscription.Poll", finding)
        self.assertIn("builds HandlerException", finding)

    def test_building_handler_exception_before_the_committed_logs_call_in_dotnet_fails(self):
        """The committed log's Poll in the .NET client is held as the subscription's is."""
        self.edit_dotnet("        int count = Abi.cavalry_committed_log_poll(handle, &OnEntry, GCHandle.ToIntPtr(self), (nuint) fragmentLimit);\n",
                         "        HandlerException early = new HandlerException(0, 0, first!);\n"
                         "        int count = Abi.cavalry_committed_log_poll(handle, &OnEntry, GCHandle.ToIntPtr(self), (nuint) fragmentLimit);\n",
                         file="CommittedLog.cs")
        [finding] = lint.findings(self.tmp)
        self.assertIn("CommittedLog.Poll", finding)
        self.assertIn("builds HandlerException", finding)

    def test_a_throw_in_the_committed_logs_catch_fails(self):
        self.edit_dotnet("        catch (Exception e)\n        {\n            raised++;\n",
                         "        catch (Exception e)\n        {\n            raised++;\n            throw;\n", file="CommittedLog.cs")
        found = lint.findings(self.tmp)
        self.assertTrue(any(f.startswith("dotnet: CommittedLog.Deliver ") and "throws" in f for f in found), found)
        self.assertTrue(any(f.startswith("dotnet: CommittedLog.OnEntry ") and "calls CommittedLog.Deliver" in f for f in found), found)

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

    def test_the_dotnet_guards_throw_passes_and_a_raising_helper_a_getter_calls_fails(self):
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

    # A cluster session and a cluster service in the .NET client (ADR 0124).

    def test_building_handler_exception_before_a_cluster_polls_call_in_dotnet_fails(self):
        """A cluster session's Poll and a cluster service's are held as the subscription's
        is: the exception each builds after its ABI call is the one it may build."""
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit_dotnet("        first = null;\n        int count = Abi.cavalry_cluster_session_poll(",
                         "        first = null;\n        Exception early = new HandlerException(0, 0, first!);\n"
                         "        int count = Abi.cavalry_cluster_session_poll(", file="ClusterSession.cs")
        self.edit_dotnet("        refused = false;\n        int count = Abi.cavalry_cluster_service_poll(",
                         "        refused = false;\n        Exception early = new CavalryException(\"x\", 0, \"y\");\n"
                         "        int count = Abi.cavalry_cluster_service_poll(", file="ClusterService.cs")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        self.assertTrue(any("ClusterSession.Poll" in f and "builds HandlerException" in f for f in found), found)
        self.assertTrue(any("ClusterService.Poll" in f and "builds CavalryException" in f for f in found), found)

    def test_a_lock_in_a_dotnet_cluster_services_poll_fails(self):
        """A service's Poll is a call on its client and is on every entry's path: it takes
        no lock, and it calls no method of the client that takes the client's, whose
        handle it reads through the one property that takes none
        (clients/dotnet/README.md, "Threads")."""
        begins = "        this.handler = handler;\n        polling = true;\n        raised = 0;\n        first = null;\n" \
                 "        refused = false;\n"
        self.edit_dotnet(begins, "        lock (client)\n        {\n        }\n" + begins, file="ClusterService.cs")
        [finding] = lint.findings(self.tmp)
        self.assertIn("ClusterService.Poll", finding)
        self.assertIn("uses `lock`", finding)
        self.setUp()
        self.edit_dotnet(begins, "        bool closed = client.IsClosed();\n" + begins, file="ClusterService.cs")
        [finding] = lint.findings(self.tmp)
        self.assertIn("ClusterService.Poll", finding)
        self.assertIn("calls Client.IsClosed", finding)
        self.assertIn("uses `lock`", finding)

    def test_a_throw_in_a_dotnet_cluster_catch_fails(self):
        """Each cluster trampoline calls a method that catches what the handler threw: a
        throw from that catch would unwind through the ABI's frames. The lint finds a
        method by its name, and four classes have a Deliver, so each trampoline is found
        for each Deliver that throws: the findings are looked for, and not counted."""
        self.assertEqual(lint.findings(self.tmp), [])
        for file in ("ClusterSession.cs", "ClusterService.cs"):
            self.edit_dotnet("        catch (Exception e)\n        {\n            raised++;\n",
                             "        catch (Exception e)\n        {\n            raised++;\n            throw;\n", file=file)
        found = lint.findings(self.tmp)
        for owner, trampoline in (("ClusterSession", "OnReply"), ("ClusterService", "OnClusterEntry")):
            self.assertTrue(any(f.startswith(f"dotnet: {owner}.Deliver ") and "throws" in f for f in found), found)
            self.assertTrue(any(f.startswith(f"dotnet: {owner}.{trampoline} ") and f"calls {owner}.Deliver" in f
                                for f in found), found)

    def test_an_exception_built_where_a_dotnet_cluster_service_keeps_a_refusal_fails(self):
        """An action the ABI refused is kept as its code and its message for Poll to throw:
        the method the trampoline calls builds no exception for it."""
        self.assertEqual(lint.findings(self.tmp), [])
        self.edit_dotnet("            refusedMessage = Abi.Errmsg();\n",
                         "            first = Abi.LastError(\"cavalry_cluster_service_action\");\n", file="ClusterService.cs")
        found = lint.findings(self.tmp)
        self.assertTrue(any(f.startswith("dotnet: ClusterService.Deliver ") and "calls Abi.LastError" in f
                            and "builds CavalryException" in f for f in found), found)
        self.assertTrue(any(f.startswith("dotnet: ClusterService.OnClusterEntry ") and "calls ClusterService.Deliver" in f
                            for f in found), found)

    def test_a_throw_or_an_allocation_in_a_dotnet_cluster_per_message_member_fails(self):
        """The session's Offer and the service's Reply return a code, and the getters of a
        reply's values and of an entry's read the one struct their object keeps."""
        self.edit_dotnet("    public long Offer(ReadOnlySpan<byte> message)\n    {\n",
                         "    public long Offer(ReadOnlySpan<byte> message)\n    {\n        Abi.Check(\"x\", -1);\n",
                         file="ClusterSession.cs")
        self.edit_dotnet("    public int Reply(long sessionId, ReadOnlySpan<byte> message)\n    {\n",
                         "    public int Reply(long sessionId, ReadOnlySpan<byte> message)\n    {\n        Abi.Check(\"x\", -1);\n",
                         file="ClusterService.cs")
        self.edit_dotnet("    public long Timestamp => values.timestamp;\n",
                         "    public long Timestamp => new long[1][0] + values.timestamp;\n", file="ClusterReplyValues.cs")
        self.edit_dotnet("    public bool TimedOut => values.timed_out != 0;\n",
                         "    public bool TimedOut => new byte[1][0] != values.timed_out;\n", file="ClusterEntryValues.cs")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 4, found)
        for name, form in (("ClusterSession.Offer", "calls Abi.Check"), ("ClusterService.Reply", "calls Abi.Check"),
                           ("ClusterReplyValues.Timestamp", "allocates long with `new`"),
                           ("ClusterEntryValues.TimedOut", "allocates byte with `new`")):
            self.assertTrue(any(f.startswith(f"dotnet: {name} ") and form in f for f in found), (name, found))

    def test_a_throw_in_a_dotnet_services_timer_members_fails_and_so_does_a_missing_one(self):
        """ScheduleTimer and CancelTimer return a code (ADR 0157): neither checks and
        throws, and both are held to the rules by name."""
        self.edit_dotnet("    public int ScheduleTimer(long correlationId, long deadline)\n    {\n",
                         "    public int ScheduleTimer(long correlationId, long deadline)\n    {\n        Abi.Check(\"x\", -1);\n",
                         file="ClusterService.cs")
        self.edit_dotnet("    public int CancelTimer(long correlationId)\n    {\n",
                         "    public int CancelTimer(long correlationId)\n    {\n        Abi.Check(\"x\", -1);\n",
                         file="ClusterService.cs")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for name in ("ClusterService.ScheduleTimer", "ClusterService.CancelTimer"):
            self.assertTrue(any(f.startswith(f"dotnet: {name} ") and "calls Abi.Check" in f for f in found), (name, found))
        self.setUp()
        self.edit_dotnet("    public int ScheduleTimer(long correlationId, long deadline)\n",
                         "    public int Schedule(long correlationId, long deadline)\n", file="ClusterService.cs")
        self.edit_dotnet("    public int CancelTimer(long correlationId)\n",
                         "    public int Cancel(long correlationId)\n", file="ClusterService.cs")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for name in ("ClusterService.ScheduleTimer", "ClusterService.CancelTimer"):
            self.assertTrue(any(f"{name} is not in" in f for f in found), (name, found))

    def test_a_missing_dotnet_cluster_member_fails(self):
        """A cluster session's and a cluster service's per-message members are held to the
        rules by name, as the others are (ADR 0124)."""
        self.edit_dotnet("    public int Reply(long sessionId, ReadOnlySpan<byte> message)\n",
                         "    public int Answer(long sessionId, ReadOnlySpan<byte> message)\n", file="ClusterService.cs")
        self.edit_dotnet("    private static void OnReply(void* clientd,", "    private static void OnAnswer(void* clientd,",
                         file="ClusterSession.cs")
        found = lint.findings(self.tmp)
        self.assertEqual(len(found), 2, found)
        for name in ("ClusterService.Reply", "ClusterSession.OnReply"):
            self.assertTrue(any(f"{name} is not in" in f for f in found), (name, found))

    def test_a_dotnet_package_without_the_per_message_members_fails(self):
        for name in ("Publication", "Subscription", "CommittedLog", "Header", "ClusterSession", "ClusterService",
                     "ClusterReplyValues", "ClusterEntryValues"):
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
