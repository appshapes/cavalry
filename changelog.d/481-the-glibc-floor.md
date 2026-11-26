- Every native file of Cavalry for Linux, the `cavalry` binary and the clients'
  library, is now built for glibc 2.28, the C library of RHEL 8, and no longer for
  the glibc of the machine that linked it, which on CI's runner was 2.34 for the
  library: it loads on RHEL 8 and its rebuilds,
  Debian 10, Ubuntu 20.04 and every later release of the three. The Python client's
  wheel is `manylinux_2_28_x86_64`, and its build backend tags no library below that
  floor. `just package` on Linux now runs in a container of glibc 2.28 and needs
  Docker; `scripts/floor.sh` runs any command there, and `scripts/glibc-floor.py`
  fails a file that needs a newer glibc
  ([ADR 0178](docs/decisions/0178-build-every-native-artefact-for-glibc-2-28-in-an-image-of-it-and-run-it-there.md);
  [Build a native artefact at the floor](docs/how-to/build-a-native-artefact-at-the-floor.md)).
