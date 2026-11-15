#!/usr/bin/env python3
"""Tests of gen-counters.py's reading of the counters that a sentence names: a `doc` or
a `doctor` sentence of counters.toml that names a counter the file does not hold is
refused, and a path, a file's name and a family written with a star are not read as
counters. The repository's own counters.toml passes. No network. Standard library only;
`just test-scripts` runs it.

    python3 scripts/gen-counters-test.py
"""
import importlib.util
import sys
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("gen_counters", HERE / "gen-counters.py")
gen_counters = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gen_counters)


def counter(name, doc="What a change in the value means.", doctor=""):
    return {"id": 0, "name": name, "kind": "counter", "scope": "static", "doc": doc,
            "doctor": doctor}


class Named(unittest.TestCase):
    def refused(self, counters):
        with self.assertRaises(gen_counters.Fail) as raised:
            gen_counters.check_named(counters)
        return str(raised.exception)

    def test_a_sentence_that_names_counters_of_the_file_passes(self):
        gen_counters.check_named([
            counter("recording.write_failed"),
            counter("driver.registry_full"),
            counter("cluster.snapshots_failed",
                    doc="A snapshot given up; see recording.write_failed.",
                    doctor="Check driver.registry_full, then recording.write_failed."),
        ])

    def test_a_doctor_sentence_that_names_no_counter_is_refused_with_the_name(self):
        message = self.refused([
            counter("recording.write_failed"),
            counter("cluster.snapshots_failed",
                    doctor="Check recording.write_failures for a disk that refuses writes."),
        ])
        self.assertIn("[[counter]] 2 ('cluster.snapshots_failed')", message)
        self.assertIn("its doctor names `recording.write_failures`", message)
        self.assertIn("no [[counter]] has that name", message)

    def test_a_doc_sentence_is_read_as_a_doctor_sentence_is(self):
        message = self.refused([
            counter("recording.write_failed", doc="Counted with recording.sync_failures."),
        ])
        self.assertIn("its doc names `recording.sync_failures`", message)

    def test_a_name_at_a_sentences_end_is_read_without_the_full_stop(self):
        gen_counters.check_named([
            counter("recording.write_failed"),
            counter("cluster.snapshots_failed", doctor="See recording.write_failed."),
        ])
        message = self.refused([
            counter("recording.write_failed"),
            counter("cluster.snapshots_failed", doctor="See recording.write_failures."),
        ])
        self.assertIn("`recording.write_failures`", message)

    def test_a_longer_name_is_not_passed_for_the_counter_it_begins_with(self):
        message = self.refused([
            counter("recording.write_failed"),
            counter("cluster.snapshots_failed", doctor="See recording.write_failed_twice."),
        ])
        self.assertIn("`recording.write_failed_twice`", message)

    def test_a_path_a_files_name_and_a_family_are_no_counters(self):
        gen_counters.check_named([
            counter("cluster.elections",
                    doc="Elections begun (spec/cluster.md, Counters; docs/reference/counters.md)."),
            counter("counters.allocated", doc="Read from counters.toml, as counters.md says."),
            counter("receiver.dropped_runt",
                    doctor="The driver's receiver.dropped_* counters say which."),
        ])

    def test_a_word_of_another_component_is_no_counter(self):
        # `i.e`, a method of a type, a version: nothing a counter's name begins with.
        gen_counters.check_named([
            counter("cluster.elections",
                    doc="An election, i.e. a ballot, of version 1.2 and module.rs."),
        ])

    def test_the_repositorys_own_file_passes(self):
        doc = gen_counters.load(gen_counters.SOURCE.read_text(encoding="utf-8"))
        gen_counters.check_named(doc["counter"])
        self.assertGreater(len(doc["counter"]), 100)


if __name__ == "__main__":
    unittest.main()
