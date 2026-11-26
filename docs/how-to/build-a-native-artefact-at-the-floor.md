# Build a native artefact at the floor

A how-to page: one task, building a native file of Cavalry so that it runs on the
oldest Linux Cavalry runs on, and checking that it does. The native files are the
`cavalry` binary and the clients' shared library, `libcavalry_ffi.so`. Why the oldest
Linux is the one it is, and why the files are built this way, is
[ADR 0178](../decisions/0178-build-every-native-artefact-for-glibc-2-28-in-an-image-of-it-and-run-it-there.md).

The floor is glibc 2.28. glibc is the C library of most Linux systems, and a program
linked against one version of it does not load on an older one. A file linked on a
developer's machine or on CI's runner needs that machine's glibc, 2.34 or later
today. So Cavalry links its native files in a container whose glibc is 2.28, and runs
them there. `scripts/floor.sh` starts that container, and `just package` uses it on
Linux for everything it builds.

## Prerequisites

- A Linux machine on x86-64 or 64-bit ARM, or a Mac. The floor has an image for each
  of the two machines.
- Docker, with its daemon running, and a user who may use it. `docker version` prints
  a `Server` section when all three hold.
- On Linux, the Rust toolchain that [Implement a feature](implement-a-feature.md)
  lists under Prerequisites, installed with `rustup`. The container uses the host's
  toolchain, at the host's paths. On a Mac the host's toolchain cannot run in a Linux
  container, so the first run installs one into a Docker volume named
  `cavalry-floor-toolchain`, which takes about a minute, once.
- A connection to `quay.io` for the first run, which pulls the image. On x86-64 it
  is 556 MB to download, by the sizes the registry lists for its layers, and 1.6 GB
  on disk, as `docker image inspect` reports it.
- A checkout of this repository. Every command below runs at its root.

## Steps

1. Run one command at the floor, to see that the container starts:

   ```sh
   # ignore: needs Docker, and its first run pulls an image of 556 MB
   scripts/floor.sh ldd --version
   ```

   The first line is the script's own. It names the glibc, the compiler, the C
   compiler, the Python and the kernel the command runs with. These lines, and those
   of step 3, are CI's Linux runner's, on x86-64:

   ```text
   floor: glibc 2.28, rustc 1.100.0 (0ef1bc386 2026-11-09), cc (GCC) 14.2.1 20250110 (Red Hat 14.2.1-11), Python 3.12.16, kernel 6.6.141
   ldd (GNU libc) 2.28
   ```

   The kernel is the host's. A container has its own C library and shares its host's
   kernel, so the floor says nothing of an older kernel.

2. Build at the floor. Any command works; these two build the library and the
   binary:

   ```sh
   # ignore: needs Docker
   scripts/floor.sh cargo build --release -p cavalry-ffi --lib
   scripts/floor.sh cargo build --release -p cavalry --bin cavalry
   ```

   In the container the checkout is at its own path, and its `target/` is the
   directory `target/floor/` of the checkout. So the two files are now at
   `target/floor/release/libcavalry_ffi.so` and `target/floor/release/cavalry`, and
   nothing in `target/release/`, which the host's own builds write, has changed.

3. Check what the two files need:

   ```sh
   # ignore: needs the files of step 2
   python3 scripts/glibc-floor.py target/floor/release/libcavalry_ffi.so target/floor/release/cavalry
   ```

   You will see one line for each file:

   ```text
   glibc-floor: target/floor/release/libcavalry_ffi.so needs glibc 2.28 at most, and the floor is 2.28; it loads libgcc_s.so.1, libpthread.so.0, libdl.so.2, libc.so.6, ld-linux-x86-64.so.2
   glibc-floor: target/floor/release/cavalry needs glibc 2.28 at most, and the floor is 2.28; it loads libgcc_s.so.1, libpthread.so.0, libdl.so.2, libc.so.6, ld-linux-x86-64.so.2
   ```

   On 64-bit ARM the list ends at `libc.so.6`.

   The check needs `objdump`, which the package `binutils` holds. On a Mac, run it
   in the container, where the files are under `target/`:
   `scripts/floor.sh python3 scripts/glibc-floor.py target/release/libcavalry_ffi.so target/release/cavalry`.

4. Build a client's package at the floor, install it there and run it there:

   ```sh
   # ignore: needs Docker
   just package
   ```

   On Linux, `just package` starts the container by itself and does everything in
   it: steps 2 and 3, a driver, the Python client's wheel, its install into a new
   environment, and the client README's example against the driver. On a Mac, `just
   package` builds the macOS package; `scripts/floor.sh bash scripts/package.sh` runs
   the Linux one from a Mac.

## How to verify it worked

- Step 3 printed a line for each file and exited 0.
- Step 4 printed the wheel's name with the floor in it, and the example's line:

  ```text
  package: python: built cavalry_client-0.1.0-py3-none-manylinux_2_28_x86_64.whl, ...
  package: python: installed into a new environment outside the checkout; the README's example printed `1000 messages over cavalry:ipc, position 64000`; ...
  ```

- On Linux, a file from `target/floor/` starts on your own system, whose glibc is
  newer: `target/floor/release/cavalry doctor` prints its report.

## When it does not work

- **`floor: docker is not installed`.** Install Docker Engine on Linux or Docker
  Desktop on a Mac, and run the command again.
- **`floor: docker is installed and its daemon does not answer`.** Start the daemon:
  `sudo systemctl start docker` on Linux, or open Docker Desktop. On Linux, a user
  outside the group `docker` gets the same message: add the user to the group and
  sign in again.
- **`floor: no Rust toolchain at ...`.** On Linux the container uses the host's
  toolchain, and found none at `CARGO_HOME` and `RUSTUP_HOME`, or at `~/.cargo` and
  `~/.rustup`. Install it with `rustup`.
- **The pull fails, or `docker: Error response from daemon`, with `quay.io` in the
  message.** The image comes from `quay.io`. Run the command again; a registry that
  stays unreachable fails every run until it answers, CI's included.
- **`glibc-floor: ... needs glibc 2.34`, with a list of functions.** The file was
  linked on a newer system than the floor, most often because it came from
  `target/release/` and not from `target/floor/release/`. Build it with
  `scripts/floor.sh` as in step 2. If the file was built at the floor, the image has
  moved: the first line of `scripts/floor.sh`'s output names its glibc.
- **`glibc-floor: ... the version name GLIBC_... is glibc's and has no number`.** The
  file asks its C library for a name the check cannot place, such as
  `GLIBC_PRIVATE`, which one build of glibc alone has. A file built at the floor
  asks for none. If a newer toolchain adds a name for a feature of the loader, the
  message says where to add it, with the glibc that first has it.
- **`glibc-floor: ... loads lib<name>.so`.** A dependency links a system library
  that a system of the floor may not have. Link it statically, or leave the
  dependency out.
- **`floor: the image has glibc ..., not 2.28`.** `scripts/floor.sh` names an image
  that is not the floor's. `python3 scripts/glibc-floor-test.py` says which of the
  three files that name the floor disagrees.
- **A build on a Mac fails with a compiler error that the Mac's own build does not
  have.** The toolchain in the volume is the stable one of the day it was installed.
  Remove the volume with `docker volume rm cavalry-floor-toolchain`, and the next run
  installs today's.
- **`cargo` in the container cannot write under `target/`.** `target/floor/` was
  made by another user, as a run of the container as root makes it. Remove
  `target/floor/` as that user and run the command again. With rootless Docker, or
  with Podman in Docker's place, the container's user is not yours whatever the
  script asks for, and the script does not work: use Docker Engine with its daemon,
  as CI does.
- **`floor: the checkout's path ... has a colon in it`.** Docker reads a colon as
  the end of a path to mount. Move the checkout to a path without one.
- **Ctrl-C does not stop a run.** It does, through the small program the script
  puts at the container's process 1. If a container is left all the same, `docker
  ps` lists it and `docker stop <its id>` ends it.
- **The floor's files run in the container and the driver fails on an old system
  all the same.** The floor is the C library's. What Cavalry asks of a kernel is in
  ADR 0178, with what holds it.
