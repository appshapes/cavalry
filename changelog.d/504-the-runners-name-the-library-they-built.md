- The Python, JVM and .NET clients' conformance runners test the library they have just
  built whatever `CAVALRY_FFI_LIBRARY` holds in the caller's shell. Each client takes
  that variable's path before the checkout's library, and no runner set it, so a value
  left from trying a packaged library by hand was what `just conformance` tested for
  those three clients. Each runner now sets it to the library it built, and the Python
  runner stops where a library is packaged beside the module, which that client loads
  before either. The README's example alone still runs without the variable, as a
  reader runs it, so the client's own search for the checkout's library stays tested
  ([the suite's page](conformance/README.md)).
