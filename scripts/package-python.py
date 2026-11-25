#!/usr/bin/env python3
"""The Python client's package, built and installed as a user installs it, against a
running driver; `scripts/package.sh` starts the driver and calls this (work item 469;
ADR 0166; clients/python/README.md, "The package").

It makes a directory outside the checkout and in it:

1. A new virtual environment, with the Python that runs this script. Its pip builds the
   wheel from `clients/python` through the package's own build backend, with nothing
   from a package index, and the wheel must hold the library `cargo` built, byte for
   byte, under the platform tag a second reader gives it: `objdump` for the glibc an ELF
   library needs, `otool` for the least macOS of a Mach-O one.
2. The wheel installed into that environment, and the README's example run with the
   environment's Python: from a directory outside the checkout, with no `PYTHONPATH`
   and no `CAVALRY_FFI_LIBRARY`, so the client can find the library in one place alone,
   its package. The program must print the line the README promises, and the library
   the client names must be the one inside the environment.
3. The control: the same wheel with the library taken out, installed into a second new
   environment, and the same example. It must fail, with the client's error that lists
   the paths it tried. If it passed, a library would be reachable from the environment
   by some other path, and step 2 would have proven nothing.

It exits 1 at the first step that does not hold, with what the step printed. Python 3.9
standard library only (ADR 0003).
"""

import importlib.util
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUFFIX = ".dylib" if sys.platform == "darwin" else ".so"
LIBRARY = os.path.join(ROOT, "target", "release", "libcavalry_ffi" + SUFFIX)
HELP = "see clients/python/README.md#the-package"
STEP_SECONDS = 300


def fail(what):
    print(f"package: python: {what}", file=sys.stderr)
    sys.exit(1)


def run(arguments, what, **options):
    """Run one command of a step to its end, and fail with its output unless it exits 0."""
    try:
        result = subprocess.run(arguments, capture_output=True, text=True, timeout=STEP_SECONDS, **options)
    except subprocess.TimeoutExpired as timeout:
        fail(f"{what} did not finish in {STEP_SECONDS} s; it printed {timeout.stdout!r}")
    if result.returncode != 0:
        fail(f"{what} failed: `{' '.join(arguments)}` exited {result.returncode} and printed\n"
             f"{result.stdout}{result.stderr}")
    return result.stdout


def readme():
    """conformance/python/readme.py, the one reader of the README's example."""
    path = os.path.join(ROOT, "conformance", "python", "readme.py")
    spec = importlib.util.spec_from_file_location("cavalry_readme", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def environment(directory):
    """A new virtual environment at `directory`, and its Python's path."""
    try:
        subprocess.run([sys.executable, "-m", "venv", directory], check=True, capture_output=True, text=True,
                       timeout=STEP_SECONDS)
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        fail(f"`{sys.executable} -m venv` made no environment; it printed\n{error.stdout}{error.stderr}\n"
             f"On Debian and Ubuntu the module is the package python3-venv: install it and run this again; {HELP}")
    return os.path.join(directory, "bin", "python")


def expected_platform():
    """The platform tag a second reader gives the library: the wheel's must be this one."""
    machine = platform.machine()
    if sys.platform == "darwin":
        listing = run(["otool", "-l", LIBRARY], "otool's reading of the library's load commands")
        least = re.search(r"cmd LC_BUILD_VERSION\n(?:.*\n)*? *minos (\d+)\.(\d+)", listing)
        if least is None:
            fail(f"otool shows no LC_BUILD_VERSION with a minos in {LIBRARY}; {HELP}")
        major, minor = int(least.group(1)), int(least.group(2))
        if major >= 11:
            major, minor = (major, 0) if minor == 0 else (major + 1, 0)
        return f"macosx_{major}_{minor}_{machine}"
    if shutil.which("objdump") is None:
        fail("objdump is not installed, and it is the second reader of the glibc version the library needs; "
             f"install binutils and run this again; {HELP}")
    listing = run(["objdump", "-T", LIBRARY], "objdump's reading of the library's dynamic symbols")
    versions = [(int(a), int(b)) for a, b in re.findall(r"GLIBC_(\d+)\.(\d+)", listing)]
    if not versions:
        fail(f"objdump shows no symbol of a GLIBC version in {LIBRARY}; {HELP}")
    newest = max(versions)
    return f"manylinux_{newest[0]}_{newest[1]}_{machine}"


def without_library(wheel, directory):
    """A copy of the wheel under the same name with the library and its RECORD line
    taken out: the control's package."""
    os.makedirs(directory)
    copy = os.path.join(directory, os.path.basename(wheel))
    member = "cavalry/libcavalry_ffi" + SUFFIX
    with zipfile.ZipFile(wheel) as source, zipfile.ZipFile(copy, "w", zipfile.ZIP_DEFLATED) as target:
        for entry in source.infolist():
            content = source.read(entry)
            if entry.filename == member:
                continue
            if entry.filename.endswith(".dist-info/RECORD"):
                content = b"".join(line for line in content.splitlines(keepends=True)
                                   if not line.startswith(member.encode() + b","))
            target.writestr(entry, content)
    return copy


def run_example(python, directory, program):
    """The README's program with an environment's Python, as a user of the installed
    package runs it: outside the checkout, with neither variable that names one."""
    names = {name: value for name, value in os.environ.items()
             if name not in ("PYTHONPATH", "PYTHONHOME", "CAVALRY_FFI_LIBRARY")}
    wait = readme().WAIT_SECONDS
    try:
        return subprocess.run([python, "-"], input=program, capture_output=True, text=True, timeout=wait,
                              cwd=directory, env=names)
    except subprocess.TimeoutExpired as timeout:
        fail(f"the README's example did not finish in {wait} s against the installed package; it printed "
             f"{timeout.stdout!r}")


def main():
    if not os.path.isfile(LIBRARY):
        fail(f"{LIBRARY} does not exist; build it with `cargo build --release -p cavalry-ffi --lib`, which "
             f"scripts/package.sh does before it calls this; {HELP}")
    program, expected = readme().example()
    with tempfile.TemporaryDirectory(prefix="cavalry-package-") as work:
        work = os.path.realpath(work)
        if os.path.commonpath([work, os.path.realpath(ROOT)]) == os.path.realpath(ROOT):
            fail(f"the temporary directory, {work}, is inside the checkout, where the client's search for a "
                 f"checkout's library would find one; set TMPDIR to a directory outside it and run this again")

        # 1. Build, through the package's manifest and its backend. The library is named
        # to the backend, not found by it: a pip before 21.3 builds from a copy of
        # clients/python, from which the checkout's target/ cannot be reached.
        python = environment(os.path.join(work, "installed"))
        wheels = os.path.join(work, "wheel")
        run([python, "-m", "pip", "wheel", "--no-deps", "--no-index", "--wheel-dir", wheels,
             os.path.join(ROOT, "clients", "python")], "the wheel's build",
            env=dict(os.environ, CAVALRY_FFI_LIBRARY=LIBRARY), cwd=work)
        built = os.listdir(wheels)
        name = re.fullmatch(r"cavalry_client-\d+\.\d+\.\d+-py3-none-(\w+)\.whl", built[0]) if len(built) == 1 else None
        if name is None:
            fail(f"the build wrote {built}, not one wheel named cavalry_client-<version>-py3-none-<platform>.whl")
        wheel = os.path.join(wheels, built[0])
        second = expected_platform()
        if name.group(1) != second:
            fail(f"the wheel's platform tag is {name.group(1)}, and {'otool' if sys.platform == 'darwin' else 'objdump'} "
                 f"reads {second} from the library; one of the two readings is wrong, and a wrong tag installs "
                 f"the package where its library cannot load; {HELP}")
        with zipfile.ZipFile(wheel) as archive, open(LIBRARY, "rb") as library:
            member = "cavalry/libcavalry_ffi" + SUFFIX
            if member not in archive.namelist() or archive.read(member) != library.read():
                fail(f"{built[0]} does not hold {member} with the bytes of {LIBRARY}; it holds {archive.namelist()}")
        print(f"package: python: built {built[0]}, {os.path.getsize(wheel)} bytes, with {member} inside, the "
              f"library cargo built")

        # 2. Install, and run the README's example as written.
        run([python, "-m", "pip", "install", "--no-deps", "--no-index", wheel], "the wheel's install", cwd=work)
        result = run_example(python, work, program)
        if result.returncode != 0:
            fail(f"the README's example exited {result.returncode} against the installed package; it printed "
                 f"{result.stdout!r} and {result.stderr!r}")
        if expected not in result.stdout.splitlines():
            fail(f"the README's example printed {result.stdout!r} against the installed package, not the line "
                 f"the README says the reader will see, {expected!r}")
        names = {name: value for name, value in os.environ.items()
                 if name not in ("PYTHONPATH", "PYTHONHOME", "CAVALRY_FFI_LIBRARY")}
        loaded = run([python, "-c", "import cavalry; print(cavalry.find_library())"],
                     "the installed client's search for its library", cwd=work, env=names).strip()
        if not os.path.realpath(loaded).startswith(os.path.join(work, "installed") + os.sep):
            fail(f"the installed client loads {loaded}, which is not inside its environment, "
                 f"{os.path.join(work, 'installed')}: the package's own library must come first; {HELP}")
        print(f"package: python: installed into a new environment outside the checkout; the README's example "
              f"printed `{expected}`; the library is the package's, {os.path.relpath(loaded, work)}")

        # 3. The control: no library in the package, so no library anywhere the client looks.
        control = environment(os.path.join(work, "control"))
        run([control, "-m", "pip", "install", "--no-deps", "--no-index",
             without_library(wheel, os.path.join(work, "without-library"))],
            "the install of the wheel without its library", cwd=work)
        result = run_example(control, work, program)
        if result.returncode == 0:
            fail(f"the README's example passed against a package with no library inside, so it found one by "
                 f"another path and this environment is not a clean one; it printed {result.stdout!r}")
        said = result.stderr.strip().splitlines()[-1] if result.stderr.strip() else ""
        for part in ("was not found", os.path.join("site-packages", "cavalry", "libcavalry_ffi" + SUFFIX),
                     "clients/python/README.md#the-package"):
            if part not in said:
                fail(f"against a package with no library inside, the example failed without the client's error "
                     f"that lists what it tried: {part!r} is not in its last line, {said!r}")
        print("package: python: the same wheel without its library fails as it must, with the client's error "
              "that lists the paths it tried")
    return 0


if __name__ == "__main__":
    sys.exit(main())
