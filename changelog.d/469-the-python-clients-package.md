- The Python client now has a package a user installs: a wheel named `cavalry-client`
  that holds the shared library beside the client, so a program that uses it needs no
  checkout and no Rust toolchain. `python3 clients/python/cavalry_package.py
  target/package`, or `pip wheel clients/python`, builds it from a checkout with
  nothing from a package index, and the wheel's platform tag is read from the library
  itself. No wheel is published yet
  ([The package](clients/python/README.md#the-package);
  [ADR 0166](docs/decisions/0166-carry-the-native-library-inside-a-clients-package-and-look-for-it-there-first.md)).
- The Python client now looks for the shared library in its own package's directory
  first, then at `CAVALRY_FFI_LIBRARY`, then in the checkout it sits in. A
  `CAVALRY_FFI_LIBRARY` that is a path with no file at it is now an error where the
  client looks for the library, not a failure of the loader; a bare name, with no
  directory in it, is still the system loader's to find. The error for a library that
  is found nowhere names every path that was tried and what to do.
- `just package` builds the Python client's wheel, installs it into a new environment
  outside the checkout and runs the client README's example there. CI runs it on
  Linux for every pull request and every commit on `master`, and on macOS each week.
