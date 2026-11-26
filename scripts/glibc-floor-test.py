#!/usr/bin/env python3
"""Tests of scripts/glibc-floor.py on what `objdump` prints, and of the one floor that
three files name (ADR 0178). `just test-scripts` runs them. No file is linked here: the
listings are objdump 2.41's of the library built at the floor, cut down, with version
names changed where a test says so. Python 3.9 standard library only (ADR 0003).
"""

import importlib.util
import os
import re
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, os.path.join(ROOT, path))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


floor = load("glibc_floor", "scripts/glibc-floor.py")


def listing(versions=("GLIBC_2.28", "GLIBC_2.25", "GLIBC_2.18", "GLIBC_2.17"),
            libraries=("libgcc_s.so.1", "libpthread.so.0", "libdl.so.2", "libc.so.6")):
    """What `objdump -p` prints of a library, with these version names asked of libc."""
    needed = "".join(f"  NEEDED               {library}\n" for library in libraries)
    asked = "".join(f"    0x0696918{index} 0x00 {index + 2:02} {version}\n" for index, version in enumerate(versions))
    return ("\ntarget/release/libcavalry_ffi.so:     file format elf64-littleaarch64\n\n"
            "Program Header:\n    LOAD off    0x0000000000000000 vaddr 0x0000000000000000 paddr 0x0000000000000000 align 2**16\n\n"
            "Dynamic Section:\n" + needed +
            "  INIT                 0x0000000000010000\n  VERNEED              0x0000000000002818\n\n"
            "Version References:\n"
            "  required from libdl.so.2:\n    0x06969197 0x00 09 GLIBC_2.17\n"
            "  required from libgcc_s.so.1:\n    0x09276060 0x00 08 GCC_4.2.0\n    0x0b792650 0x00 03 GCC_3.0\n"
            "  required from libc.so.6:\n" + asked +
            "private flags = 0x0:\n\n")


SYMBOLS = ("0000000000000000      DF *UND*\t0000000000000000 (GLIBC_2.34) pthread_create\n"
           "0000000000000000      DF *UND*\t0000000000000000 (GLIBC_2.34) dlsym\n"
           "0000000000000000  w   DF *UND*\t0000000000000000 (GLIBC_2.28) statx\n"
           "0000000000000000      DF *UND*\t0000000000000000 (GLIBC_2.17) memcpy\n")


class Reading(unittest.TestCase):
    def test_the_libraries_and_the_version_names_are_read_from_the_listing(self):
        libraries, versions = floor.read(listing())
        self.assertEqual(libraries, ["libgcc_s.so.1", "libpthread.so.0", "libdl.so.2", "libc.so.6"])
        self.assertEqual(versions, ["GLIBC_2.17", "GCC_4.2.0", "GCC_3.0", "GLIBC_2.28", "GLIBC_2.25", "GLIBC_2.18",
                                    "GLIBC_2.17"])
        self.assertEqual(floor.newest(versions), (2, 28))
        # A file with no version references, as a static program has none.
        self.assertEqual(floor.read("Dynamic Section:\n  NEEDED               libc.so.6\n"), (["libc.so.6"], []))

    def test_a_version_name_is_a_glibc_or_none(self):
        for name, version in (("GLIBC_2.28", (2, 28)), ("GLIBC_2.2.5", (2, 3)), ("GLIBC_2.3.4", (2, 4)),
                              ("GLIBC_2.17.0", (2, 17)), ("GLIBC_2.4", (2, 4)), ("GLIBC_ABI_DT_RELR", (2, 36)),
                              ("GCC_3.0", None), ("GLIBC_PRIVATE", None), ("GLIBC_ABI_OTHER", None),
                              ("XGLIBC_2.99", None)):
            self.assertEqual(floor.glibc(name), version, name)
        # Versions compare as numbers: 2.4 is below 2.17.
        self.assertEqual(floor.newest(["GLIBC_2.4", "GLIBC_2.17", "GCC_9.0"]), (2, 17))
        self.assertIsNone(floor.newest(["GCC_3.0"]))


class Checking(unittest.TestCase):
    def test_a_file_at_the_floor_or_below_it_passes_and_says_what_it_needs(self):
        line = floor.check("target/release/cavalry", listing())
        self.assertEqual(line, "glibc-floor: target/release/cavalry needs glibc 2.28 at most, and the floor is 2.28; "
                               "it loads libgcc_s.so.1, libpthread.so.0, libdl.so.2, libc.so.6")
        self.assertIn("needs glibc 2.17 at most", floor.check("lib", listing(versions=("GLIBC_2.17",))))

    def test_a_file_above_the_floor_fails_and_names_the_versions_the_functions_and_the_way_out(self):
        with self.assertRaises(floor.FloorError) as raised:
            floor.check("target/release/libcavalry_ffi.so", listing(versions=("GLIBC_2.34", "GLIBC_2.28", "GLIBC_2.32")),
                        SYMBOLS)
        said = str(raised.exception)
        for part in ("target/release/libcavalry_ffi.so needs glibc 2.34", "it asks for GLIBC_2.32, GLIBC_2.34, for "
                     "dlsym, pthread_create.", "The floor is glibc 2.28", "refuses to load", "scripts/floor.sh cargo "
                     "build --release", "docs/how-to/build-a-native-artefact-at-the-floor.md"):
            self.assertIn(part, said)
        self.assertNotIn("statx", said)
        # One minor above is above; with no symbol listing the functions are left out.
        with self.assertRaises(floor.FloorError) as raised:
            floor.check("lib", listing(versions=("GLIBC_2.29",)))
        self.assertIn("it asks for GLIBC_2.29. The floor", str(raised.exception))

    def test_packed_relative_relocations_are_above_the_floor(self):
        """`objdump -T` shows no symbol for this name, and a reader of symbols alone would
        pass the file; the version references have it."""
        with self.assertRaises(floor.FloorError) as raised:
            floor.check("lib", listing(versions=("GLIBC_2.28", "GLIBC_ABI_DT_RELR")), SYMBOLS)
        self.assertIn("needs glibc 2.36: it asks for GLIBC_ABI_DT_RELR.", str(raised.exception))

    def test_a_library_the_floor_may_not_have_and_a_file_not_of_glibc_fail(self):
        with self.assertRaises(floor.FloorError) as raised:
            floor.check("lib", listing(libraries=("libc.so.6", "libssl.so.3", "libatomic.so.1")))
        self.assertIn("lib loads libatomic.so.1, libssl.so.3, and a system of the floor is sure to have",
                      str(raised.exception))
        with self.assertRaises(floor.FloorError) as raised:
            floor.check("lib", "Dynamic Section:\n  NEEDED               libc.musl-x86_64.so.1\n")
        self.assertIn("asks for no GLIBC version", str(raised.exception))

    def test_the_program_fails_for_any_file_that_fails_and_for_none(self):
        self.assertEqual(floor.main([]), 2)


class OneFloor(unittest.TestCase):
    def test_the_check_the_image_and_the_packages_tag_name_one_floor(self):
        """The floor is written in three files that cannot import one another: this
        check, the script that names the image, and the Python package's backend, which
        is generated into a package that stands alone."""
        major, minor = floor.FLOOR
        with open(os.path.join(ROOT, "scripts", "floor.sh"), encoding="utf-8") as script:
            script = script.read()
        self.assertIn(f'FLOOR="{major}.{minor}"', script)
        images = re.findall(r'IMAGE="([^"]+)"', script)
        self.assertEqual(len(images), 2)
        for image, machine in zip(images, ("x86_64", "aarch64")):
            self.assertRegex(image, rf"^quay\.io/pypa/manylinux_{major}_{minor}_{machine}@sha256:[0-9a-f]{{64}}$")
        backend = load("cavalry_package", "clients/python/cavalry_package.py")
        self.assertEqual(backend._GLIBC_FLOOR, floor.FLOOR)
        self.assertEqual(backend._GLIBC_NAMES, {name.encode(): version for name, version in floor.NAMES.items()})


if __name__ == "__main__":
    unittest.main(argv=sys.argv[:1])
