# 0165. Carry the native library inside a client's package, and look for it there first

- **Status:** Accepted
- **Date:** 2026-11-25
- **Invariants affected:** 3, 5, 6
- **Supersedes:** none

## Context

Every client of Cavalry takes its native library, the C ABI's `libcavalry_ffi`, from a
checkout: a path in the tree or one named in the environment. The Python and the JVM
client have no package manifest. So nobody has seen a client installed the way a user
installs one, and the beta's line asks for exactly that: each client's README example
runs against its built package (ADR 0138, feature 10; work item 369). Work item 369
asks for one client's package to be built and installed in CI before the others,
"since nothing here has tried it", and work item 469 is that part. It publishes
nothing. The names are ruled already, on work item 369 on 2026-11-24: `cavalry-client`
on PyPI, Python's package index, where `cavalry` is another project's.

The first thing the work showed is that it did not work. The Python client looked for
the library at `CAVALRY_FFI_LIBRARY`, and then under `target/release` of the six
directories above its own. Installed into a new environment outside a checkout, it
found none: the gate this ADR adds failed on its first run, before the search
changed, with `libcavalry_ffi.dylib was not found`.

Three facts bound the choice. CI's gates jobs take nothing from a package index for
any client, and say so step by step in `.github/workflows/ci.yml`. A Python package
with a native library inside is a wheel for one platform, and the index refuses a
Linux wheel whose file name does not say which glibc, the C library of most Linux
systems, it needs (PEP 600's `manylinux` tags). And `gates (linux)` is the one check
`master`'s protection requires (ADR 0073; ADR 0139).

## Decision

**A client's package holds the native library, and the client looks for it in its own
package before anywhere else.** The Python client is the first. The other four follow
in their own parts of work item 369, each with its language's packaging.

For the Python client:

- **The package is a wheel named `cavalry-client`, version the workspace's**, with the
  library beside the package's two files, `cavalry/libcavalry_ffi.so` or
  `cavalry/libcavalry_ffi.dylib`. It has no dependency and no source distribution,
  and it does not hold the driver.
- **The search order is the package's own directory, then `CAVALRY_FFI_LIBRARY`, then
  the checkout the package sits in.** An installed package so loads the library it was
  built with whatever its environment says. A `CAVALRY_FFI_LIBRARY` that names no file
  is an error and not a reason to look further. When nothing is found, the error
  names every path that was tried, in order, and what to do. `Client(library_path=...)`
  stays the way to give an installed package another library.
- **The wheel is written by a build backend of the package's own, on Python's standard
  library, generated like the rest of the package.** `clients/python/pyproject.toml`
  names `clients/python/cavalry_package.py` as its PEP 517 backend with nothing to
  install first, so `pip wheel clients/python` builds the wheel and so does the file
  run as a program. The build takes nothing from a package index.
- **The wheel's platform tag is read from the library, never from the machine that
  builds it.** The least macOS a Mach-O library names, and the newest `GLIBC_` symbol
  version an ELF library needs. A library the backend has no tag for is refused and
  no wheel is written.
- **`just package` is the gate, a step of the gates jobs.** It builds the wheel through
  pip, checks the tag against a second reader (`objdump`, `otool`), installs the wheel
  into a new environment outside the checkout, and runs the client README's example
  there against a driver of the same commit. Then it runs a control in every run: the
  same wheel without its library must fail with the search's error.

## Alternatives considered

- **`CAVALRY_FFI_LIBRARY` first, as before.** A variable is the usual way to override.
  It lost because the installed package is the default case, and its default must be
  right without a look at the environment (invariant 5): a variable left in a shell by
  work in a checkout would give an installed client another build of the library, and
  the ABI version check passes two builds of one version. The override an operator
  means is still there, as an argument.
- **A packaging tool: setuptools, hatchling, or maturin, with auditwheel and delocate,
  or cibuildwheel around them.** They are what most projects use. They lost for this
  package on three counts. Each is a download from the package index in a gates job
  that takes none. setuptools tags a package with no extension module as pure,
  `py3-none-any`, unless its commands are overridden, and maturin builds pyo3, cffi
  and uniffi bindings, while this client is `ctypes` over a library another crate
  builds. And what auditwheel and delocate do beyond naming the tag, copying in the
  other libraries a library needs, has nothing to copy: on Linux the library needs
  `libc.so.6`, `libgcc_s.so.1` and the loader alone (Evidence), which every glibc
  system has and which the `manylinux` tags let a wheel leave outside. The cost is about 230 lines of
  generated Python that read two file formats, held by tests on made-up files and by
  a second reader in the gate.
- **The build machine's tag.** Python's `sysconfig.get_platform()` says what the
  interpreter was built for, not what the library needs: `linux-x86_64` on the Linux
  runner, which the index refuses, and `macosx-26.0-arm64` on the laptop this was
  written on, for a library that runs on macOS 11.
- **A second package for the library, which the client's package depends on.** It lost
  because it makes two versions that can differ where one file cannot.
- **Fetch the library at install or at first use.** It lost to invariant 5: an install
  that needs a second network fetch fails on a host with no route to it, late.
- **The system's library path.** `ctypes.util.find_library` and the loader's path load
  whatever library of that name a host has. It lost for the reason the variable did.
- **Another client first.** The Node and the Go client link the static library, so
  they have no library to find at run time. The .NET client's packaging has a
  directory the runtime searches by itself (`runtimes/<rid>/native`), so it would have
  shown least about a search of our own. The JVM client is the nearest to Python, and
  needs one thing more, a place to copy the library to. Python has no manifest, its
  search is the client's own code, and both runners carry what its build and its
  install need.
- **A job of its own in `ci.yml`.** It lost because a job under a new name is not
  required until the owner changes the protection rule, and it would build the driver
  and the library a second time. The step adds about 4 s to a gates job (Evidence).
- **The control once, by hand.** Work item 469 asks that the job be shown to fail once
  with the library left out. The control runs every time instead, since what it
  guards can change with a runner's image: that the new environment has no other
  library within the client's reach.

## Evidence

- **Before the search changed**, `scripts/package.sh` on the laptop (macOS 26.6.1 on
  Apple silicon, Python 3.14.6, pip 26.1.2) built and installed the wheel, and the
  README's example exited 1 in the new environment: `find_library:
  libcavalry_ffi.dylib was not found`.
- **After it**, the same script passed there, and passed with the system's Python
  3.9.6 and its pip 21.2.4, which builds from a copy of `clients/python`: without
  `CAVALRY_FFI_LIBRARY` that build stops with the backend's error, and the script
  names the library to the backend for that reason. The wheel built through pip and
  the one built by the backend run as a program were the same bytes under one Python;
  under the two Pythons they differ, 361,183 and 357,030 bytes, since the compression
  is each Python's own zlib.
- **On CI's two runners**, run 45712527335 of a throwaway branch. Linux, Ubuntu
  24.04 with Python 3.12.3, pip 24.0 and glibc 2.39: the wheel is
  `cavalry_client-0.1.0-py3-none-manylinux_2_34_x86_64.whl`, 401,968 bytes around a
  library of 955,384, and `objdump` agrees that the newest symbol version the library
  needs is `GLIBC_2.34`; its `NEEDED` entries are `libgcc_s.so.1`, `libc.so.6` and
  `ld-linux-x86-64.so.2`. macOS on Apple silicon, Python 3.14.6 and pip 26.1.2:
  `cavalry_client-0.1.0-py3-none-macosx_11_0_arm64.whl`, 360,185 bytes, and `otool`
  agrees. Both ran the README's example in the new environment and printed its line,
  and both controls failed as they must. The runner's own Python made the
  environment on both: Ubuntu's `venv` module was there.
- **With the library left out by the backend**, the same step fails: the two jobs of
  that run that remove the backend's line for the library exited 1 at `does not hold
  cavalry/libcavalry_ffi.so with the bytes of` the built library, and the same for
  `.dylib`.
- **The time.** With the driver and the library built, `just package` took 3.8 s on
  the laptop and 3.4 s to 4.5 s on the Linux runner, twenty runs in a row. With the
  build, from a warm cache of dependencies, it took 15.8 s on the Linux runner and
  17.4 s on the macOS one. In a gates job the build is done by the steps before it.

## Consequences

- A user of the Python client needs no checkout and no Rust toolchain for the client.
  They still need the driver, which only `cargo` gives: the release's ADR decides how
  a user without Rust gets it, and a wheel is one place that could hold it.
- **The wheel's floor is the build machine's.** A library linked on Ubuntu 24.04 needs
  glibc 2.34, which leaves out systems on an older one, among them RHEL 8 and Ubuntu
  20.04. A lower floor is a build on an older image or with a linker that targets an
  older glibc, and the release's ADR decides it with the platforms. The backend will
  tag what it is given.
- We maintain the backend's two readers. A new platform is a new tag there, with a
  made-up file in `clients/package-test.py`: Linux on musl, Windows, and a macOS
  library of two architectures are refused today, each with a message that says so.
- The wheel carries Cavalry's `LICENSE` and no notice for the crates the library is
  linked from. The release adds what their licences ask.
- The package's version is the workspace's, written as it stands, and the generator
  refuses a version that is not three numbers: a pre-release needs its spelling for
  Python decided first, by the release's ADR.
- A `CAVALRY_FFI_LIBRARY` that names no file was a failure of the loader with the
  system's message, and is now the client's error with what to do. A checkout's
  package that set the variable to an existing file behaves as before.
- **For the other four clients** the rule is the decision's first sentence, and each
  part of work item 369 says how its package holds it. What this one taught is
  written on work item 469.
- Revisit: when the release's ADR chooses the platforms and where the wheels are
  built; when a second client's package shows that the rule does not fit its
  language; when a wheel must hold a second native file, the driver among them.

## References

- Work item 469, which carried this decision, and work item 369, whose first part it
  is and whose comments rule the names.
- [ADR 0138](0138-draw-the-finish-line-at-a-beta-that-is-an-alternative-to-aeron-and-build-it-sixth.md),
  feature 10; [ADR 0046](0046-build-the-c-abi-and-the-clients-fourth.md), the clients;
  [ADR 0073](0073-run-the-macos-gates-for-master-and-not-for-pull-requests.md), where each
  gates job runs; [ADR 0003](0003-lay-out-the-workspace.md), the one version.
- [`clients/python/README.md`](../../clients/python/README.md), "The package";
  [`clients/README.md`](../../clients/README.md), "The package a user installs";
  `scripts/package.sh`, `scripts/package-python.py` and `clients/package-test.py`.
- PEP 427, the wheel format; PEP 517, the build backend's interface; PEP 600, the
  `manylinux` tags.
