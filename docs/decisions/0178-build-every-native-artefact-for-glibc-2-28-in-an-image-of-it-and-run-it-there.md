# 0178. Build every native artefact for glibc 2.28, in an image of it, and run it there

- **Status:** Accepted
- **Date:** 2026-11-26
- **Invariants affected:** 3, 5, 6
- **Supersedes:** none. It decides what ADR 0166 left to "the release's ADR" under
  "The wheel's floor is the build machine's", and names what it leaves to the release
  ADR of work item 490 and to work item 497.

## Context

A native file of Cavalry, the `cavalry` binary or the library `libcavalry_ffi.so`
that every client's package holds, is linked against glibc, the C library of most
Linux systems. A file linked against one glibc does not load on an older one. CI's
Linux runner is Ubuntu 24.04 with glibc 2.39, and work item 469 found that a library
linked there needs glibc 2.34. RHEL 8 has 2.28, Ubuntu 20.04 and Debian 11 have 2.31,
and none of them loads it.

Nothing in Cavalry's code asks for a new glibc. The same commit linked against glibc
2.28 asks for 2.28 at most, and against 2.17 for 2.17 (Evidence). The 2.34 is the
link machine's: glibc 2.34 moved its thread and loader libraries into `libc.so.6`,
so a link against it binds `pthread_create` and `dlsym` at 2.34. Rust's own floor
for this target is glibc 2.17 and Linux 3.2.

So the floor, the oldest system a native artefact runs on, is a choice of what
Cavalry promises, and invariant 5 with the working rules makes the test of a
promise that a gate holds it. The beta ships for the two systems the gates run on,
Linux on x86-64 and macOS on Apple silicon (ADR 0138, condition 1). Work items 483
to 486, 489 and 491 each build a native artefact, and each needs the floor and the
way to build at it before it starts.

## Decision

**The floor of every native artefact for Linux is glibc 2.28, and each is linked in
an image of glibc 2.28 and run there by a gate.**

- **The floor.** glibc 2.28, on x86-64. RHEL 8 and its rebuilds, Debian 10 and
  Ubuntu 20.04 have it or a newer one, and so has every later release of the three.
  Amazon Linux 2 (2.26), RHEL 7 (2.17) and Ubuntu 18.04 (2.27) are below it, and no
  artefact of Cavalry is built for them.
- **Where an artefact is built.** In a container of the image `manylinux_2_28` that
  the Python packaging project publishes: AlmaLinux 8, a rebuild of RHEL 8, with
  glibc 2.28, a C compiler and Python. `scripts/floor.sh <command>` runs a command
  there. It names the image by its digest, so the image moves only by a change to
  that file. The checkout is in the container at its own path, its `target/` is the
  checkout's `target/floor/`, and on Linux the Rust toolchain is the host's own, at
  the host's paths: the compiler of the other gates, with a linker and a C library
  of the floor. The release's workflow (work item 491) builds every artefact for
  Linux through this script, and so does each package's own work item.
- **The gate.** `just package`, a step of `gates (linux)`, runs whole in that
  container on Linux. It builds the library and the binary there.
  `scripts/glibc-floor.py` then fails either file that asks for a glibc newer than
  the floor, counting `GLIBC_ABI_DT_RELR` as 2.36, or that loads a library other than
  glibc's own and `libgcc_s`. The Python client's wheel is built from that library,
  installed into a new environment and run against that driver, as ADR 0166 has it,
  now on glibc 2.28.
- **The name an artefact carries.** An artefact names its system in the words of
  its own ecosystem, and where those words hold a glibc version it is the floor's:
  the Python wheel is `manylinux_2_28_x86_64`. Its build backend tags no library
  below the floor, whatever the library asks for, since Cavalry runs its packages on
  nothing older; that also ends the tags no installer matches, below `manylinux_2_5`
  on x86-64 and `2_17` on 64-bit ARM. A library that asks for more keeps its own
  tag, and the gate refuses it. An ecosystem whose names hold no glibc version names
  Linux on x86-64, and its package's README names the floor.
- **macOS.** The floor is macOS 11 on Apple silicon, as the linker sets it and as
  ADR 0166 read it from the library; nothing changes there. Whether the beta ships
  packages for macOS is the release ADR's (work item 490), and this decision does
  not assume its answer.
- **Linux on 64-bit ARM.** No artefact of 0.1.0, and no gate: no runner of the gates
  is one. It builds and runs at the same floor from a checkout, `scripts/floor.sh`
  has its image, and its names are fixed with x86-64's, `manylinux_2_28_aarch64`, so
  that adding it changes no name.
- **musl**, the C library of Alpine Linux. No artefact, and not tried. The wheel's
  backend refuses a library built for it, and so does the floor's check.

**The kernel is the other half of the same promise, and this decision does not make
it.** What Cavalry asks of a Linux kernel:

| What | Where | Since | On a kernel without it |
| --- | --- | --- | --- |
| What Rust's standard library asks: threads and their parking, files, sockets, clocks | every crate | Linux 3.2, Rust's floor | Rust does not run |
| The populate advice, `MADV_POPULATE_READ` and `MADV_POPULATE_WRITE` | `cavalry-shm`, `pretouch.rs`: every mapping is made resident when it is made (ADR 0027) | Linux 5.14 | The advice fails with `EINVAL`, and the pages are touched in a loop. A driver runs. A driver directory with no room then kills the driver with a bus error, where the advice gives a refusal (work item 497) |
| UDP sockets: `recvmsg`, `MSG_DONTWAIT`, `SO_RCVBUF`, `SO_SNDBUF` | `cavalry-driver`, `cavalry-client`, through `nix` | before Linux 3.2 | none known |
| Shared file mappings, `pread`, `pwrite`, `fsync`, `ftruncate`, `flock` | `cavalry-shm`, `cavalry-log`, through `memmap2` and the standard library | before Linux 3.2 | none known |
| Files of `/proc` and `/sys` that `doctor` reads | `cavalry-control` | each its own | `doctor` leaves out the check whose file is absent |

The list is from a search of the crates for every call through `nix`, `memmap2` and
`libc`; the crates make no system call of their own. One line of it is above Rust's
floor, and it is on the floor's own systems: RHEL 8 ships Linux 4.18, Debian 10
ships 4.19, Debian 11 ships 5.10, and Ubuntu 20.04 ships 5.4, or 5.15 with its
newer kernel installed. A container shares its host's kernel, 6.6 on CI's runner, so
the gate above shows the C library of RHEL 8 and not its kernel. Work item 497
decides what the driver does on a kernel without the advice, with a test that takes
that kernel's path, and its ADR names the kernels a driver starts on. Until it
lands, the C library is the gated half: the pages that name the floor say that it
is the C library's, and no page names a kernel.

## Alternatives considered

- **Stay at glibc 2.34, what a link on the runner gives.** It costs nothing and
  adds no image. It lost because it leaves out RHEL 8, which its maker supports
  until 2029, and with it Ubuntu 20.04 and Debian 11: the systems an operator of a
  message transport is most likely to run today.
- **glibc 2.17, in the image `manylinux2014`.** Measured: the same commit builds
  there and asks for 2.17 alone. It would add Amazon Linux 2, RHEL 7 and Ubuntu
  18.04. It lost on four counts. The three have passed the end of their makers'
  standard support, in June 2026, June 2024 and May 2023. The image is built on
  CentOS 7, which has ended too. 2.17 is Rust's own floor, with no room under it:
  Rust raised that floor in 2022, and the next raise would break the promise from
  outside. And the runtimes two of the clients need stop higher: Node's own Linux
  binaries have needed glibc 2.28 since Node 18.
- **A linker that targets an older glibc, such as `cargo zigbuild`, with no
  image.** Not tried. It lost on what it could show: such a link is against lists of
  symbols and no system, so nothing could run at the floor, and the gate would read
  version names alone. The container runs the artefact on the floor's C library, and
  the control in Evidence is what that run is worth.
- **Run every gate in the image.** Then `just test` and `just conformance` would run
  at the floor too. It lost for now: the image would need Node, a JDK, .NET and Go,
  and the required check would stand on an image for every step. The floor matters
  for what ships, and `just package` is where what ships is built.
- **Tag the wheel by what its library asks for, with no floor in the backend**, as
  ADR 0166 has it. The tag would be 2.28 today, from one function the standard
  library looks for, and would move when that function does. A tag below the floor
  promises a system nothing here has run on.
- **A statically linked binary on musl, one file for every Linux.** Not tried. It
  cannot be the library, which Python, the JVM, .NET and Node load into a process
  that already has glibc, and two kinds of artefact would then need two gates.

## Evidence

- **The same commit against three C libraries**: on the laptop, 64-bit ARM, in the
  two images, and on the runner. Linked in `manylinux_2_28`: the library and the
  binary ask for `GLIBC_2.28` at most, for `statx`, which the standard library
  looks for. Linked in `manylinux2014`: `GLIBC_2.17` alone. Linked on the runner,
  x86-64, glibc 2.39: the library asks for 2.34 and the binary for 2.39.
- **The gate on the runner**, x86-64, `trial 481` run 45827238634 from a throwaway
  branch: the image's pull took 9.7 s, `just package` 17.8 s with nothing built and
  6.6 s again. The wheel is `cavalry_client-0.1.0-py3-none-manylinux_2_28_x86_64.whl`,
  405,365 bytes, and the README's example printed its line on glibc 2.28. The files
  in `target/floor/` are the runner's user's. The floor's driver then ran on the
  runner's own glibc 2.39, and `doctor` read it.
- **The control**, in the same run. The library and the binary linked on the runner:
  `scripts/glibc-floor.py` failed both, naming `dlsym`, `pthread_create`, `gettid`
  and others. At the floor, Python's `ctypes` refused the library, ``version
  `GLIBC_2.32' not found``, and the loader refused the binary.
- **The floor's files in each distribution's own image**, x86-64 on the runner,
  `trial 481` run 45827593995, and 64-bit ARM on the laptop, with one result on
  both. The driver started and `doctor` read it in AlmaLinux 8 and Debian 10 (glibc
  2.28), Ubuntu 20.04 and Debian 11 (2.31), AlmaLinux 9 and Amazon Linux 2023
  (2.34), Ubuntu 22.04 (2.35), Debian 12 (2.36), Ubuntu 24.04 (2.39), Debian 13
  (2.41) and Fedora 43 (2.42). It did not start in Amazon Linux 2 (2.26), Ubuntu
  18.04 (2.27) and CentOS 7 (2.17): the loader said ``version `GLIBC_2.28' not
  found``, and on x86-64 then `symbol statx, version GLIBC_2.28 not defined`. Each
  container ran on its host's kernel, 6.6 on the runner and 7.0 on the laptop.
- **The kernels**, from each distribution's own package index on 2026-11-26:
  AlmaLinux 8 `4.18.0-553`, Debian 11 `5.10.262`, Ubuntu 20.04 `5.4.0` and, as
  `linux-image-generic-hwe-20.04`, `5.15.0`; AlmaLinux 9 `5.14.0`.
- **The tests.** `scripts/glibc-floor-test.py`: the check's reading of what
  `objdump` prints, a file above the floor, packed relative relocations, a library
  the floor may lack, and that the check, the image's name and the backend's tag
  name one floor. `clients/package-test.py`: a tag is never below the floor and
  never lowered by it, and `GLIBC_ABI_DT_RELR` is 2.36.
- **Outside facts**: Rust's floor for the target, Linux 3.2 and glibc 2.17, from its
  platform support page, and its raise from 2.11 in Rust 1.64; Node 18's release
  notes, "glibc 2.28 or later"; Amazon Linux 2's end of support, 2026-06-30, from its
  maker's page.

## Consequences

Easier: Cavalry's files load on RHEL 8, Ubuntu 20.04 and Debian 11, and a gate shows
it for every pull request. Each package's work item builds its native file with one
script and checks it with one more. An artefact's name does not move with the
machine that built it.

Harder:

- `gates (linux)`, the required check, now pulls an image of 556 MB from `quay.io`
  in every run. It took 10 s. A registry that does not answer fails the check for
  every pull request until it does.
- `just package` on Linux needs Docker. A contributor without it gets a message
  that says so and what to install.
- On a system below the floor a wheel built there is tagged for the floor, and pip
  refuses to install it. The client still runs from a checkout, with a library
  built there, and nothing here has run it.
- The floor is in three files that cannot import one another, the script, the check
  and the generated backend. One test holds them together.

Not shown by any gate: an artefact on the kernel of a system of the floor; an
artefact on 64-bit ARM; RHEL 8 itself, for which its rebuild stands.

To maintain: `scripts/floor.sh` and its two digests; `scripts/glibc-floor.py`; the
backend's floor;
[Build a native artefact at the floor](../how-to/build-a-native-artefact-at-the-floor.md).
A new digest is a change to `scripts/floor.sh`, and its pull request's run of
`gates (linux)` is its trial.

Revisit when work item 497 lands: its ADR names the kernels, and the pages that
name the floor then name both halves. Revisit the floor, by a new ADR, when Rust
raises its glibc floor above 2.28, when RHEL 8 leaves its maker's support in 2029,
or when the image is no longer published. Revisit the registry when a run of
`gates (linux)` fails at the pull twice in a month: then the image is kept where
the runner's cache is. Revisit 64-bit ARM when a runner of the gates is one.

## References

- Work item 481, part 1 of work item 369's remaining parts; work item 469, which
  found the 2.34; work item 497, the kernel; work item 490, the release ADR.
- ADR 0166 (the package and its tag), ADR 0138 (the beta's platforms), ADR 0027
  (pre-touching), ADR 0170 (where the macOS gates run).
- `scripts/floor.sh`, `scripts/glibc-floor.py`, `scripts/glibc-floor-test.py`,
  `scripts/package.sh`, `scripts/package-python.py`;
  `clients/generate_python_package.py`, `clients/package-test.py`;
  `clients/python/README.md`, "Where it runs".
- <https://doc.rust-lang.org/rustc/platform-support.html>,
  <https://blog.rust-lang.org/2022/08/01/Increasing-glibc-kernel-requirements/>,
  <https://nodejs.org/en/blog/release/v18.0.0>, <https://aws.amazon.com/amazon-linux/>,
  <https://peps.python.org/pep-0600/>.
