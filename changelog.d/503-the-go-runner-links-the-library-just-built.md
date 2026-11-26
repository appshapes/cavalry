- The Go client's conformance runner tests the library it has just built. cgo links
  `libcavalry_ffi.a` by its path, Go's build cache does not see that file change, and
  with no Go source changed `go run` ran the program it had linked with the library of
  an earlier build: after a change to the C ABI's Rust alone, `conformance/go/run.sh`
  on a machine whose Go cache is on its disk ran its quickstart against the old
  library and passed. The runner now names the contents of the library and of the
  header in its build's flags, and each of its programs that links the library stops
  unless it was linked with the one just built. CI's Linux runner was measured and was
  not affected: its Go cache is a cache program's, set in `GOCACHEPROG`, and Go keeps
  a `go run` program only in a cache on its own disk. A program of
  your own has the same cache, and the Go client's README says what `go run`,
  `go test` and `go build` each need after the library is rebuilt
  ([the Go client's README](clients/go/README.md#example)).
