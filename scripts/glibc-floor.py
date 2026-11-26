#!/usr/bin/env python3
"""Fail when a native artefact needs a newer glibc than the floor (ADR 0178;
docs/how-to/build-a-native-artefact-at-the-floor.md).

    python3 scripts/glibc-floor.py <file> [<file> ...]

The floor is glibc 2.28, the C library of the oldest Linux a native artefact of Cavalry
runs on. Each file is an ELF program or shared library, and `objdump -p` reads two
things from it:

- The version names it asks of the libraries it loads, such as `GLIBC_2.28` for a
  function glibc 2.28 added. The newest of them is the oldest glibc the file loads on.
  `GLIBC_ABI_DT_RELR`, which a link with packed relative relocations asks for, has no
  number in it and stands for glibc 2.36.
- The libraries it loads. A system of the floor is sure to have glibc's own and
  `libgcc_s`, and no other.

A file that needs more than the floor was linked on a newer system than the floor's:
the message names the functions that ask for the newer versions, as `objdump -T` shows
them, and says to build with `scripts/floor.sh`. `scripts/package.sh` runs this on what
it built, and `scripts/package-python.py` takes its second reading of the wheel's tag
from here. Python 3.9 standard library only (ADR 0003).
"""

import re
import shutil
import subprocess
import sys

# The floor, as (major, minor). The same number is in scripts/floor.sh, which names the
# image of that glibc, and in the Python package's build backend, which tags a wheel no
# lower (clients/generate_python_package.py); scripts/glibc-floor-test.py holds the three
# together.
FLOOR = (2, 28)
# What a file may load: the libraries every glibc system has, which PEP 600's
# `manylinux` policies let a package leave outside, and no other.
LIBRARIES = frozenset(("libc.so.6", "libm.so.6", "libdl.so.2", "librt.so.1", "libpthread.so.0", "libgcc_s.so.1",
                       "ld-linux-x86-64.so.2", "ld-linux-aarch64.so.1"))
# Version names with no number in them, and the glibc that first has each.
NAMES = {"GLIBC_ABI_DT_RELR": (2, 36)}
HELP = "see docs/how-to/build-a-native-artefact-at-the-floor.md"


class FloorError(Exception):
    """A file could not be read, or does not hold the floor: the message says which."""


def glibc(name):
    """The glibc a version name stands for, as (major, minor), or None for a name that
    is no glibc version. A third number, `GLIBC_2.3.4`, is the next minor: a file that
    needs 2.3.4 does not load on 2.3."""
    if name in NAMES:
        return NAMES[name]
    version = re.fullmatch(r"GLIBC_(\d+)\.(\d+)(?:\.(\d+))?", name)
    if version is None:
        return None
    return int(version.group(1)), int(version.group(2)) + (1 if int(version.group(3) or 0) else 0)


def read(listing):
    """`(libraries, versions)` from what `objdump -p` printed of one file: the names of
    its NEEDED entries, and the version names under "Version References"."""
    libraries = re.findall(r"^\s*NEEDED\s+(\S+)", listing, flags=re.M)
    versions = []
    references = re.search(r"^Version References:\n((?:[ \t]+.*\n?)*)", listing, flags=re.M)
    if references is not None:
        for line in references.group(1).splitlines():
            # `    0x06969194 0x00 03 GLIBC_2.28`; a "required from libc.so.6:" line has no such shape.
            entry = re.fullmatch(r"\s+0x[0-9a-f]+\s+0x[0-9a-f]+\s+\d+\s+(\S+)", line)
            if entry is not None:
                versions.append(entry.group(1))
    return libraries, versions


def newest(versions):
    """The newest glibc among version names, as (major, minor), or None when none of
    them is glibc's."""
    found = [version for version in map(glibc, versions) if version is not None]
    return max(found) if found else None


def check(path, listing, symbols=""):
    """The line to print for a file that holds the floor; FloorError for one that does
    not. `listing` is `objdump -p`'s output and `symbols` `objdump -T`'s, which names the
    functions in the message."""
    libraries, versions = read(listing)
    needs = newest(versions)
    if needs is None:
        raise FloorError(f"{path} asks for no GLIBC version, so it was not linked against glibc, and the floor "
                         f"is glibc's: build it for a `-linux-gnu` target with scripts/floor.sh; {HELP}")
    other = sorted(set(libraries) - LIBRARIES)
    if other:
        raise FloorError(f"{path} loads {', '.join(other)}, and a system of the floor is sure to have glibc's own "
                         f"libraries and libgcc_s alone: link that library statically, or carry it beside the "
                         f"artefact and say so in a successor of ADR 0178; {HELP}")
    if needs > FLOOR:
        above = sorted({name for name in versions if (glibc(name) or FLOOR) > FLOOR},
                       key=lambda name: (glibc(name), name))
        functions = sorted(set(re.findall(r"\((?:%s)\)\s+(\S+)" % "|".join(map(re.escape, above)), symbols)))
        shown = f", for {', '.join(functions[:8])}{' and others' if len(functions) > 8 else ''}" if functions else ""
        raise FloorError(f"{path} needs glibc {needs[0]}.{needs[1]}: it asks for {', '.join(above)}{shown}. The "
                         f"floor is glibc {FLOOR[0]}.{FLOOR[1]}, and a system of the floor refuses to load this "
                         f"file. It was linked on a newer system: build it at the floor, `scripts/floor.sh cargo "
                         f"build --release ...`, as `just package` does; {HELP}")
    return (f"glibc-floor: {path} needs glibc {needs[0]}.{needs[1]} at most, and the floor is "
            f"{FLOOR[0]}.{FLOOR[1]}; it loads {', '.join(libraries)}")


def objdump(path, flag):
    try:
        result = subprocess.run(["objdump", flag, path], capture_output=True, text=True, timeout=120)
    except subprocess.TimeoutExpired:
        raise FloorError(f"`objdump {flag} {path}` did not finish in 120 s") from None
    if result.returncode != 0:
        raise FloorError(f"`objdump {flag} {path}` exited {result.returncode} and printed {result.stderr.strip()!r}: "
                         f"the file must be an ELF program or shared library; {HELP}")
    return result.stdout


def main(arguments):
    if not arguments:
        print(f"glibc-floor: no file: python3 scripts/glibc-floor.py <file> [<file> ...]; {HELP}", file=sys.stderr)
        return 2
    if shutil.which("objdump") is None:
        print(f"glibc-floor: objdump is not installed, and it reads what a file needs of glibc: install binutils "
              f"and run this again; {HELP}", file=sys.stderr)
        return 1
    failed = False
    for path in arguments:
        try:
            print(check(path, objdump(path, "-p"), objdump(path, "-T")))
        except FloorError as error:
            print(f"glibc-floor: {error}", file=sys.stderr)
            failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
