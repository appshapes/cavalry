- The Go client's conformance runner tests the library it has just built. cgo links
  `libcavalry_ffi.a` by its path, Go's build cache does not see that file change, and
  with no Go source changed `go run` reused the program it had linked with the library
  of an earlier build: after a change to the C ABI's Rust alone, `conformance/go/run.sh`
  on a machine with a warm Go cache ran its quickstart against the old library and
  passed. The runner now names the contents of the library and of the header in its
  build's flags. CI's runs were measured and were not affected. A program of your own
  has the same cache: build it with `-a` once after rebuilding the library
  ([the Go client's README](clients/go/README.md#example)).
