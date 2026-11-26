#!/usr/bin/env python3
"""The tests of the Python client's package (work item 469; ADR 0166; ADR 0026): the
manifest and the build backend that `generate_python_package.py` writes, and the search
for the library that the generated client makes.

- The committed `pyproject.toml`, `cavalry_package.py` and `LICENSE` are what the
  generator gives, with the workspace's version in the first two and the repository's
  licence in the third, and a version that is not three numbers is refused when the
  package is generated.
- The backend reads the wheel's platform tag from the library: the least macOS of a
  made-up Mach-O library, and the newest glibc symbol version a made-up ELF library
  needs. It refuses what it has no tag for, and what it cannot read, with what to do.
- The wheel holds the two generated files and the library, each with the hash and the
  length its RECORD gives; two builds of one input are the same bytes, from the
  package's directory in a checkout and from a copy of it alone; a library that is not
  there, a directory without its licence, and a source distribution are refused.
- `cavalry.find_library` looks in the package's own directory first, then at
  `CAVALRY_FFI_LIBRARY`, then in a checkout around it, and its error names every path.

No test here loads a library or starts a driver: `just package` does both, with the
library `cargo` builds (scripts/package.sh). `just test` runs this through
`test-scripts`. Python 3.9 standard library only.
"""

import base64
import contextlib
import hashlib
import importlib.util
import io
import os
import shutil
import struct
import sys
import tempfile
import time
import unittest
import zipfile
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import generate_python_package as gen_package  # noqa: E402


def load(name, path, package=False):
    """A module from a file, under a name of its own, so that a test's copy of the
    client is never the `cavalry` another test imported."""
    spec = importlib.util.spec_from_file_location(
        name, path, submodule_search_locations=[os.path.dirname(path)] if package else None)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


backend = load("cavalry_package_under_test", os.path.join(HERE, "python", "cavalry_package.py"))


def macho(cpu=0x0100000C, commands=((0x32, struct.pack("<IIII", 1, 11 << 16, 15 << 16, 0)),), kind=6):
    """A 64-bit Mach-O header, a dynamic library's unless `kind` says otherwise, and its
    load commands, each `(command, body)`."""
    body = b"".join(struct.pack("<II", command, 8 + len(content)) + content for command, content in commands)
    return struct.pack("<IiiIIIII", 0xFEEDFACF, cpu - (1 << 32) if cpu >= 1 << 31 else cpu, 0, kind, len(commands),
                       len(body), 0, 0) + body


def elf(machine=62, needs=(("libc.so.6", ("GLIBC_2.2.5", "GLIBC_2.34", "GLIBC_2.17")),), identity=b"\x02\x01",
        kind=3, count=None, last_offset=0, versions_count=None):
    """A 64-bit ELF file, a shared library's unless `kind` says otherwise, of four
    sections: the empty first one, a string table that is not the version needs' own, a
    version-needs section that asks each library of `needs` for its versions, and that
    section's string table, which it names by its link. The entries of the version-needs
    section lie 8 bytes apart, so that a reader must follow each entry's offset to the
    next. `count` is the section's count of libraries when it is not the true one, and
    `last_offset` what the last library's entry gives as the offset to the next.
    `versions_count` is each library's count of versions when it is not the true one."""
    decoy = b"\0" + b"GLIBC_9.99\0" * 8
    strings = b"\0"
    offsets = {}
    for library, versions in needs:
        for text in (library,) + tuple(versions):
            offsets[text] = len(strings)
            strings += text.encode() + b"\0"
    gap = b"\xff" * 8
    verneed = b""
    for index, (library, versions) in enumerate(needs):
        size = 16 + len(gap) + (16 + len(gap)) * len(versions)
        following = size if index + 1 < len(needs) else last_offset
        verneed += struct.pack("<HHIII", 1, len(versions) if versions_count is None else versions_count,
                               offsets[library], 16 + len(gap), following) + gap
        for at, version in enumerate(versions):
            verneed += struct.pack("<IHHII", 0, 0, 2 + at, offsets[version],
                                   16 + len(gap) if at + 1 < len(versions) else 0) + gap
    header_size, section_size = 64, 64
    decoy_at = header_size
    verneed_at = decoy_at + len(decoy)
    strings_at = verneed_at + len(verneed)
    sections_at = strings_at + len(strings)
    header = (b"\x7fELF" + identity + b"\x01" + b"\0" * 9
              + struct.pack("<HHIQQQIHHHHHH", kind, machine, 1, 0, 0, sections_at, 0, header_size, 0, 0, section_size, 4, 1))
    sections = (struct.pack("<IIQQQQIIQQ", 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
                + struct.pack("<IIQQQQIIQQ", 0, 3, 0, 0, decoy_at, len(decoy), 0, 0, 1, 0)
                + struct.pack("<IIQQQQIIQQ", 0, 0x6FFFFFFE, 0, 0, verneed_at, len(verneed), 3,
                              len(needs) if count is None else count, 8, 0)
                + struct.pack("<IIQQQQIIQQ", 0, 3, 0, 0, strings_at, len(strings), 0, 0, 1, 0))
    return header + decoy + verneed + strings + sections


class Manifest(unittest.TestCase):
    def test_the_committed_files_are_what_the_generator_gives(self):
        files = gen_package.write_python_package()
        self.assertEqual(sorted(files), ["LICENSE", "cavalry_package.py", "pyproject.toml"])
        with open(os.path.join(HERE, "..", "LICENSE"), encoding="utf-8") as f:
            self.assertEqual(files["LICENSE"], f.read())
        for name, content in files.items():
            with open(os.path.join(HERE, "python", name), encoding="utf-8") as f:
                self.assertEqual(f.read(), content, f"run `python3 clients/generate.py python` and commit {name}")

    def test_the_manifest_and_the_backend_carry_the_workspaces_version_and_the_ruled_name(self):
        with open(os.path.join(HERE, "..", "Cargo.toml"), encoding="utf-8") as f:
            version = gen_package.workspace_version(f.read())
        files = gen_package.write_python_package()
        self.assertIn(f'\nname = "cavalry-client"\nversion = "{version}"\n', files["pyproject.toml"])
        self.assertIn('\nrequires = []\nbuild-backend = "cavalry_package"\nbackend-path = ["."]\n',
                      files["pyproject.toml"])
        self.assertEqual((backend.NAME, backend.VERSION), ("cavalry-client", version))
        self.assertNotIn("@", files["pyproject.toml"].replace("@NAME@", ""))
        for placeholder in ("@NAME@", "@VERSION@", "@SUMMARY@", "@REQUIRES_PYTHON@", "@LICENSE@", "@REPOSITORY@"):
            self.assertNotIn(placeholder, files["pyproject.toml"] + files["cavalry_package.py"])

    def test_the_version_is_the_workspace_tables_and_a_version_that_is_not_three_numbers_is_refused(self):
        manifest = '[package]\nversion = "9.9.9"\n\n[workspace.package]\nedition = "2024"\nversion = "1.2.3"\n\n[x]\n'
        self.assertEqual(gen_package.workspace_version(manifest), "1.2.3")
        with self.assertRaisesRegex(ValueError, r"under \[workspace\.package\]"):
            gen_package.workspace_version('[package]\nversion = "9.9.9"\n')
        with self.assertRaisesRegex(ValueError, "is not three numbers"):
            gen_package.workspace_version('[workspace.package]\nversion = "0.1.0-rc.1"\n')


class PlatformTag(unittest.TestCase):
    def test_a_mach_o_librarys_tag_is_its_least_macos_and_its_cpu(self):
        self.assertEqual(backend.library_platform(macho(), "lib"), ("libcavalry_ffi.dylib", "macosx_11_0_arm64"))
        old = macho(cpu=0x01000007, commands=((0x24, struct.pack("<II", (10 << 16) | (12 << 8), 0)),))
        self.assertEqual(backend.library_platform(old, "lib")[1], "macosx_10_12_x86_64")
        # The command the tag is read from is found among others, whatever their order.
        among = macho(commands=((0x19, b"\0" * 64), (0x32, struct.pack("<IIII", 1, 14 << 16, 0, 0)), (0x1B, b"\0" * 16)))
        self.assertEqual(backend.library_platform(among, "lib")[1], "macosx_14_0_arm64")

    def test_a_least_version_that_a_tag_cannot_say_is_the_next_one_it_can(self):
        """An installer knows `macosx_<major>_0` alone from macOS 11 on, and no tag has a
        third number: a library that needs 11.3 tagged 11_0 would install on 11.0, where
        it does not load."""
        arm64, x86_64 = 0x0100000C, 0x01000007
        for cpu, least, tag in ((arm64, (11, 3, 0), "macosx_12_0_arm64"), (arm64, (11, 0, 1), "macosx_12_0_arm64"),
                                (arm64, (14, 0, 0), "macosx_14_0_arm64"), (x86_64, (10, 9, 5), "macosx_10_10_x86_64"),
                                (x86_64, (10, 9, 0), "macosx_10_9_x86_64"), (x86_64, (10, 15, 7), "macosx_10_16_x86_64"),
                                (x86_64, (11, 0, 0), "macosx_11_0_x86_64"),
                                # macOS on arm64 begins at 11.0: an older least version there is 11_0,
                                # the oldest tag an installer on arm64 has.
                                (arm64, (10, 9, 0), "macosx_11_0_arm64"), (arm64, (10, 15, 7), "macosx_11_0_arm64")):
            version = (least[0] << 16) | (least[1] << 8) | least[2]
            library = macho(cpu=cpu, commands=((0x32, struct.pack("<IIII", 1, version, 0, 0)),))
            self.assertEqual(backend.library_platform(library, "lib")[1], tag, least)

    def test_a_mach_o_library_with_no_tag_is_refused(self):
        for library, said in ((macho(cpu=7), "cpu type 0x7"),
                              (macho(kind=2), "a Mach-O file of type 2, not a dynamic library (6)"),
                              (macho(commands=((0x32, struct.pack("<IIII", 2, 17 << 16, 0, 0)),)), "names no least macOS version"),
                              (macho(commands=()), "names no least macOS version"),
                              (macho(commands=((0x32, struct.pack("<IIII", 1, 0, 0, 0)),)), "macOS 0.0.0 as the least"),
                              (macho(cpu=0x01000007, commands=((0x24, struct.pack("<II", 5 << 8, 0)),)),
                               "macOS 0.5.0 as the least"),
                              (macho()[:36], "could not be read as a Mach-O library"),
                              (macho(commands=((0x32, b""),))[:-1] + b"\0", "could not be read as a Mach-O library")):
            with self.assertRaises(backend.PackageError) as raised:
                backend.library_platform(library, "target/lib")
            self.assertIn(said, str(raised.exception))
            self.assertIn("target/lib", str(raised.exception))
            self.assertIn("clients/python/README.md#the-package", str(raised.exception))

    def test_an_elf_librarys_tag_is_the_newest_glibc_it_needs_and_its_machine(self):
        self.assertEqual(backend.library_platform(elf(), "lib"), ("libcavalry_ffi.so", "manylinux_2_34_x86_64"))
        # Versions compare as numbers, 2.4 below 2.17, and across every library asked;
        # a version of another library's naming, GCC_3.0, is no glibc version.
        several = elf(machine=183, needs=(("libgcc_s.so.1", ("GCC_3.0", "GCC_4.2.0")),
                                          ("libm.so.6", ("GLIBC_2.31",)),
                                          ("libc.so.6", ("GLIBC_2.4", "GLIBC_2.17", "GLIBC_2.2.5"))))
        self.assertEqual(backend.library_platform(several, "lib")[1], "manylinux_2_31_aarch64")
        # A tag has no third number: a library that needs 2.30.1 does not run on 2.30, so
        # its tag is 2_31. glibc has given no version a third number since 2.3.4, which
        # is below the floor now, so the versions here are made up to be above it.
        for versions, tag in ((("GLIBC_2.30.1", "GLIBC_2.2.5"), "manylinux_2_31_x86_64"),
                              (("GLIBC_2.30",), "manylinux_2_30_x86_64"),
                              (("GLIBC_2.30.1", "GLIBC_2.31"), "manylinux_2_31_x86_64"),
                              (("GLIBC_2.30.1", "GLIBC_2.32"), "manylinux_2_32_x86_64"),
                              (("GLIBC_2.29.0",), "manylinux_2_29_x86_64")):
            self.assertEqual(backend.library_platform(elf(needs=(("libc.so.6", versions),)), "lib")[1], tag, versions)

    def test_an_elf_librarys_tag_is_never_below_the_floor(self):
        """ADR 0178: Cavalry's packages are built for glibc 2.28 and run on it in the
        package's gate, and on nothing older. A library that asks for less gets the
        floor's tag, and none of the tags that no installer matches, `manylinux_2_3` and
        `2_4` on x86_64 and anything below `2_17` on aarch64. A library that asks for
        more keeps what it asks for: the floor never lowers a tag."""
        self.assertEqual(backend._GLIBC_FLOOR, (2, 28))
        for machine, name in ((62, "x86_64"), (183, "aarch64")):
            for versions in (("GLIBC_2.2.5",), ("GLIBC_2.3.4", "GLIBC_2.2.5"), ("GLIBC_2.17",),
                             ("GLIBC_2.17", "GLIBC_2.18", "GLIBC_2.25"), ("GLIBC_2.27",), ("GLIBC_2.27.1",),
                             ("GLIBC_2.28",), ("GLIBC_2.17", "GLIBC_2.28")):
                library = elf(machine=machine, needs=(("libc.so.6", versions),))
                self.assertEqual(backend.library_platform(library, "lib")[1], f"manylinux_2_28_{name}", versions)
            for versions, tag in ((("GLIBC_2.28.1",), "2_29"), (("GLIBC_2.17", "GLIBC_2.29"), "2_29"),
                                  (("GLIBC_2.34", "GLIBC_2.2.5"), "2_34")):
                library = elf(machine=machine, needs=(("libc.so.6", versions),))
                self.assertEqual(backend.library_platform(library, "lib")[1], f"manylinux_{tag}_{name}", versions)

    def test_a_library_with_packed_relative_relocations_needs_glibc_2_36(self):
        """A library linked with `-z pack-relative-relocs` asks its loader for the version
        name `GLIBC_ABI_DT_RELR`, which glibc has from 2.36 and which has no number in
        it. A reader that skipped the name would tag such a library for the newest
        numbered version it asks for, 2.34 here, where it does not load."""
        packed = elf(needs=(("libc.so.6", ("GLIBC_2.2.5", "GLIBC_ABI_DT_RELR", "GLIBC_2.34")),))
        self.assertEqual(backend.library_platform(packed, "lib")[1], "manylinux_2_36_x86_64")
        alone = elf(machine=183, needs=(("libc.so.6", ("GLIBC_ABI_DT_RELR",)),))
        self.assertEqual(backend.library_platform(alone, "lib")[1], "manylinux_2_36_aarch64")
        newer = elf(needs=(("libc.so.6", ("GLIBC_ABI_DT_RELR", "GLIBC_2.38")),))
        self.assertEqual(backend.library_platform(newer, "lib")[1], "manylinux_2_38_x86_64")
        # Another name that begins the same way is no glibc version, as before.
        other = elf(needs=(("libc.so.6", ("GLIBC_ABI_OTHER", "GLIBC_PRIVATE", "GLIBC_2.30")),))
        self.assertEqual(backend.library_platform(other, "lib")[1], "manylinux_2_30_x86_64")

    def test_an_elf_library_with_no_tag_is_refused(self):
        whole = elf()
        for library, said in ((elf(machine=40), "machine 40"),
                              (elf(kind=2), "an ELF file of type 2, not a shared library (3)"),
                              (elf(identity=b"\x01\x01"), "not 64-bit little-endian"),
                              (elf(identity=b"\x02\x02"), "not 64-bit little-endian"),
                              (elf(needs=(("libgcc_s.so.1", ("GCC_3.0",)),)), "asks for no GLIBC symbol version"),
                              (elf(needs=()), "asks for no GLIBC symbol version"),
                              (whole[:len(whole) - 70], "could not be read as an ELF library"),
                              (elf(count=2, last_offset=1 << 30), "could not be read as an ELF library"),
                              (whole[:30], "an ELF file of 30 bytes, fewer than an ELF header's 64: it is cut short"),
                              (b"\x7fELF", "an ELF file of 4 bytes"),
                              # No section headers: their count, their size or their place is zero.
                              (whole[:60] + b"\0\0" + whole[62:], "an ELF library with no section headers"),
                              (whole[:58] + b"\0\0" + whole[60:], "an ELF library with no section headers"),
                              (whole[:40] + b"\0" * 8 + whole[48:], "an ELF library with no section headers")):
            with self.assertRaises(backend.PackageError) as raised:
                backend.library_platform(library, "target/lib")
            self.assertIn(said, str(raised.exception))
            self.assertIn("target/lib", str(raised.exception))
            self.assertIn("clients/python/README.md#the-package", str(raised.exception))

    def test_a_count_that_is_wrong_does_not_keep_the_reader_past_the_lists_end(self):
        """An entry whose offset to the next is zero is the last, whatever the section's
        count says: a count of four thousand million must not be read that many times.
        Two things hold that, and each alone would: the zero offset ends the list, and
        no list is read past the entries its section has room for."""
        self.assertEqual(backend.library_platform(elf(count=0xFFFFFFFF), "lib")[1], "manylinux_2_34_x86_64")
        started = time.monotonic()
        self.assertEqual(backend.library_platform(elf(count=0xFFFFFFFF, versions_count=0xFFFF), "lib")[1],
                         "manylinux_2_34_x86_64")
        self.assertLess(time.monotonic() - started, 5)
        # And a count that is too small ends the list early: the reader trusts neither alone.
        two = (("libm.so.6", ("GLIBC_2.31",)), ("libc.so.6", ("GLIBC_2.34",)))
        self.assertEqual(backend.library_platform(elf(needs=two, count=1), "lib")[1], "manylinux_2_31_x86_64")

    def test_a_file_that_is_neither_is_refused(self):
        for library in (b"", b"MZ\x90\0" + b"\0" * 60, struct.pack(">I", 0xCAFEBABE) + b"\0" * 60):
            with self.assertRaisesRegex(backend.PackageError, "neither an ELF library nor a 64-bit Mach-O library"):
                backend.library_platform(library, "lib")


class Wheel(unittest.TestCase):
    def build(self, library, directory):
        path = os.path.join(directory, "made-up-library")
        with open(path, "wb") as f:
            f.write(library)
        with mock.patch.dict(os.environ, {"CAVALRY_FFI_LIBRARY": path}):
            return backend.build_wheel(os.path.join(directory, "out"))

    def test_the_wheel_holds_the_package_and_the_library_as_its_record_says(self):
        with tempfile.TemporaryDirectory() as directory:
            library = elf()
            name = self.build(library, directory)
            self.assertEqual(name, f"cavalry_client-{backend.VERSION}-py3-none-manylinux_2_34_x86_64.whl")
            info = f"cavalry_client-{backend.VERSION}.dist-info"
            with zipfile.ZipFile(os.path.join(directory, "out", name)) as wheel:
                self.assertEqual(wheel.namelist(), ["cavalry/__init__.py", "cavalry/_abi.py", "cavalry/libcavalry_ffi.so",
                                                    f"{info}/LICENSE", f"{info}/METADATA", f"{info}/WHEEL", f"{info}/RECORD"])
                self.assertEqual(wheel.read("cavalry/libcavalry_ffi.so"), library)
                self.assertEqual(wheel.getinfo("cavalry/libcavalry_ffi.so").external_attr >> 16, 0o100755)
                # No member carries the time of its build: two builds are the same bytes on any day.
                self.assertEqual({member.date_time for member in wheel.infolist()}, {(1980, 1, 1, 0, 0, 0)})
                for source in ("__init__.py", "_abi.py"):
                    with open(os.path.join(HERE, "python", "cavalry", source), "rb") as f:
                        self.assertEqual(wheel.read(f"cavalry/{source}"), f.read())
                with open(os.path.join(HERE, "..", "LICENSE"), "rb") as f:
                    self.assertEqual(wheel.read(f"{info}/LICENSE"), f.read())
                self.assertEqual(wheel.read(f"{info}/WHEEL").decode().splitlines(),
                                 ["Wheel-Version: 1.0", f"Generator: cavalry_package {backend.VERSION}",
                                  "Root-Is-Purelib: false", "Tag: py3-none-manylinux_2_34_x86_64"])
                metadata = wheel.read(f"{info}/METADATA").decode()
                for line in ("Metadata-Version: 2.1", "Name: cavalry-client", f"Version: {backend.VERSION}",
                             "Requires-Python: >=3.9", "License: Apache-2.0"):
                    self.assertIn(line + "\n", metadata)
                record = wheel.read(f"{info}/RECORD").decode().splitlines()
                self.assertEqual(record[-1], f"{info}/RECORD,,")
                self.assertEqual([row.split(",")[0] for row in record], wheel.namelist())
                for row in record[:-1]:
                    member, digest, length = row.split(",")
                    content = wheel.read(member)
                    self.assertEqual(int(length), len(content))
                    self.assertEqual(digest, "sha256=" + base64.urlsafe_b64encode(
                        hashlib.sha256(content).digest()).rstrip(b"=").decode())

    def test_a_mach_o_library_is_named_dylib_and_two_builds_are_the_same_bytes(self):
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            name = self.build(macho(), first)
            self.assertEqual(name, f"cavalry_client-{backend.VERSION}-py3-none-macosx_11_0_arm64.whl")
            self.assertEqual(self.build(macho(), second), name)
            with zipfile.ZipFile(os.path.join(first, "out", name)) as wheel:
                self.assertIn("cavalry/libcavalry_ffi.dylib", wheel.namelist())
            with open(os.path.join(first, "out", name), "rb") as one, open(os.path.join(second, "out", name), "rb") as two:
                self.assertEqual(one.read(), two.read())

    def test_no_wheel_is_written_for_a_library_that_is_not_there_or_has_no_tag(self):
        with tempfile.TemporaryDirectory() as directory:
            missing = os.path.join(directory, "nothing-here")
            with mock.patch.dict(os.environ, {"CAVALRY_FFI_LIBRARY": missing}):
                with self.assertRaisesRegex(backend.PackageError, "CAVALRY_FFI_LIBRARY is set to .*nothing-here, and no "
                                                                  "file is there; set it to the path"):
                    backend.build_wheel(os.path.join(directory, "out"))
                with contextlib.redirect_stderr(io.StringIO()) as said:
                    self.assertEqual(backend.main(["cavalry_package.py", os.path.join(directory, "out")]), 1)
                self.assertIn("cavalry_package: CAVALRY_FFI_LIBRARY is set to", said.getvalue())
            with self.assertRaises(backend.PackageError):
                self.build(b"not a library", directory)
            self.assertFalse(os.path.exists(os.path.join(directory, "out")))

    def test_without_the_variable_the_library_is_the_checkouts_and_its_absence_says_how_to_build_it(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(backend, "HERE", os.path.join(
                directory, "clients", "python")), mock.patch.dict(os.environ):
            os.environ.pop("CAVALRY_FFI_LIBRARY", None)
            suffix = ".dylib" if sys.platform == "darwin" else ".so"
            built = os.path.join(directory, "target", "release", "libcavalry_ffi" + suffix)
            with self.assertRaises(backend.PackageError) as raised:
                backend.library_path()
            for part in (built, "cargo build --release -p cavalry-ffi --lib", "CAVALRY_FFI_LIBRARY",
                         "clients/python/README.md#the-package"):
                self.assertIn(part, str(raised.exception))
            os.makedirs(os.path.dirname(built))
            with open(built, "wb") as f:
                f.write(elf())
            self.assertEqual(backend.library_path(), built)

    def test_a_copy_of_the_packages_directory_alone_builds_the_same_wheel(self):
        """A pip before 21.3 builds from a copy of clients/python: the copy has no
        checkout around it, so everything the wheel holds but the library is in it."""
        with tempfile.TemporaryDirectory() as directory:
            copy = os.path.join(directory, "copy")
            shutil.copytree(os.path.join(HERE, "python"), copy, ignore=shutil.ignore_patterns("__pycache__"))
            copied = load("cavalry_package_copy", os.path.join(copy, "cavalry_package.py"))
            self.addCleanup(sys.modules.pop, "cavalry_package_copy")
            library = os.path.join(directory, "made-up-library")
            with open(library, "wb") as f:
                f.write(elf())
            with mock.patch.dict(os.environ, {"CAVALRY_FFI_LIBRARY": library}):
                name = copied.build_wheel(os.path.join(directory, "from-copy"))
                self.assertEqual(backend.build_wheel(os.path.join(directory, "in-place")), name)
            with open(os.path.join(directory, "from-copy", name), "rb") as one, \
                    open(os.path.join(directory, "in-place", name), "rb") as two:
                self.assertEqual(one.read(), two.read())
            # Without its licence the directory builds nothing, and says how to get it back.
            os.remove(os.path.join(copy, "LICENSE"))
            with mock.patch.dict(os.environ, {"CAVALRY_FFI_LIBRARY": library}):
                with self.assertRaisesRegex(copied.PackageError, "LICENSE does not exist, and the wheel carries the "
                                                                 "licence; `python3 clients/generate.py python`"):
                    copied.build_wheel(os.path.join(directory, "without"))
            self.assertFalse(os.path.exists(os.path.join(directory, "without")))

    def test_the_build_needs_nothing_installed_and_there_is_no_source_distribution(self):
        self.assertEqual(backend.get_requires_for_build_wheel(), [])
        # PEP 517's own name for it, which a frontend looks up on the backend.
        with self.assertRaisesRegex(backend.UnsupportedOperation, "has no source distribution"):
            backend.build_sdist("anywhere")
        with contextlib.redirect_stderr(io.StringIO()) as said:
            self.assertEqual(backend.main(["cavalry_package.py"]), 2)
        self.assertIn("usage: cavalry_package.py <directory", said.getvalue())


class FindLibrary(unittest.TestCase):
    """`cavalry.find_library`, on a copy of the generated package in a made-up tree: the
    committed package sits in a checkout, where the last of the three places may hold
    a library that a developer built."""

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        # Seven directories deep, so that the walk up from the package stays inside the
        # made-up tree: <root>/a/b/c/d/e/f/cavalry, and <root>/a is the sixth above it.
        self.root = os.path.realpath(self.directory.name)
        self.package = os.path.join(self.root, "a", "b", "c", "d", "e", "f", "cavalry")
        os.makedirs(self.package)
        for source in ("__init__.py", "_abi.py"):
            with open(os.path.join(HERE, "python", "cavalry", source), "rb") as f, \
                    open(os.path.join(self.package, source), "wb") as copy:
                copy.write(f.read())
        self.cavalry = load(f"cavalry_copy_{id(self)}", os.path.join(self.package, "__init__.py"), package=True)
        self.addCleanup(lambda: [sys.modules.pop(name) for name in list(sys.modules)
                                 if name.startswith(f"cavalry_copy_{id(self)}")])
        self.name = "libcavalry_ffi" + (".dylib" if sys.platform == "darwin" else ".so")
        environment = mock.patch.dict(os.environ)
        environment.start()
        self.addCleanup(environment.stop)
        os.environ.pop("CAVALRY_FFI_LIBRARY", None)

    def put(self, *parts):
        path = os.path.join(self.root, *parts)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "wb") as f:
            f.write(b"a library")
        return path

    def test_the_packages_own_library_comes_first_whatever_else_there_is(self):
        os.environ["CAVALRY_FFI_LIBRARY"] = named = self.put("elsewhere", self.name)
        self.put("a", "b", "c", "d", "target", "release", self.name)
        # A directory of the library's name in the package is no library.
        os.makedirs(os.path.join(self.package, self.name))
        self.assertEqual(self.cavalry.find_library(), named)
        os.rmdir(os.path.join(self.package, self.name))
        packaged = self.put("a", "b", "c", "d", "e", "f", "cavalry", self.name)
        self.assertEqual(self.cavalry.find_library(), packaged)

    def test_the_variable_comes_before_a_checkouts_build_and_a_path_with_no_file_is_refused(self):
        self.put("a", "b", "c", "d", "target", "release", self.name)
        os.environ["CAVALRY_FFI_LIBRARY"] = named = self.put("elsewhere", self.name)
        self.assertEqual(self.cavalry.find_library(), named)
        os.environ["CAVALRY_FFI_LIBRARY"] = missing = os.path.join(self.root, "nothing-here")
        with self.assertRaises(self.cavalry.CavalryError) as raised:
            self.cavalry.find_library()
        for part in (f"CAVALRY_FFI_LIBRARY is set to {missing}, and no file is there",
                     os.path.join(self.package, self.name), "clients/python/README.md#the-package"):
            self.assertIn(part, raised.exception.message)
        # A bare name, with no directory in it, is the system loader's to find, as it
        # was before the package looked anywhere: it is handed on as it is.
        os.environ["CAVALRY_FFI_LIBRARY"] = "libcavalry_ffi_of_the_system.so"
        self.assertEqual(self.cavalry.find_library(), "libcavalry_ffi_of_the_system.so")

    def test_a_checkouts_build_is_found_from_the_nearest_directory_above(self):
        self.put("a", "target", "release", self.name)
        near = self.put("a", "b", "c", "target", "release", self.name)
        self.assertEqual(self.cavalry.find_library(), near)

    def test_with_no_library_the_error_names_every_path_it_tried_and_what_to_do(self):
        # One above the sixth directory, where the search does not look.
        self.put("target", "release", self.name)
        with self.assertRaises(self.cavalry.CavalryError) as raised:
            self.cavalry.find_library()
        message = raised.exception.message
        tried = [os.path.join(self.package, self.name)]
        above = self.package
        for _ in range(6):
            above = os.path.dirname(above)
            tried.append(os.path.join(above, "target", "release", self.name))
        self.assertEqual(above, os.path.join(self.root, "a"))
        positions = [message.index(path) for path in tried]
        self.assertEqual(positions, sorted(positions))
        self.assertNotIn(os.path.join(self.root, "target"), message)
        for part in (f"{self.name} was not found. Tried, in order:", "CAVALRY_FFI_LIBRARY, which is not set",
                     "Install the package from a wheel built for this platform",
                     "cargo build --release -p cavalry-ffi --lib", "clients/python/README.md#the-package"):
            self.assertIn(part, message)
        self.assertEqual((raised.exception.function, raised.exception.code), ("find_library", 0))


if __name__ == "__main__":
    unittest.main()
