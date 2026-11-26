// Package linked holds each program of the Go runner to the library that
// conformance/go/run.sh has just built (work item 503).
//
// cgo links libcavalry_ffi.a by its path, and Go's build cache does not see that file
// change (go help cache): with no Go source changed, go run hands back the program it
// linked with the library of an earlier build. run.sh therefore names the library's
// contents to the linker, as the value of Library, which makes every program's link a
// new one whenever the library is, and it names the same contents in the environment.
// A program whose two values differ was linked with another library, or by a command
// that did not pass run.sh's flags, and it stops before it tests anything.
package linked

import (
	"fmt"
	"os"
)

// Library is `git hash-object` of the libcavalry_ffi.a this program was linked with:
// run.sh sets it with the linker's -X. Empty in a program that was not linked by
// run.sh's flags.
var Library string

// Built is the environment variable in which run.sh names the library it has just
// built, by the same hash.
const Built = "CAVALRY_GO_LINKED_LIBRARY"

// Check stops the program, with exit status 1, unless it was linked with the library
// that run.sh has just built.
func Check() {
	built := os.Getenv(Built)
	if Library != "" && Library == built {
		return
	}
	fmt.Fprintf(os.Stderr, "conformance/go: this program was linked with the library %q and the one just built is %q, "+
		"by `git hash-object target/release/libcavalry_ffi.a`: it would test a library that is not the one built. "+
		"Run it through conformance/go/run.sh, which names the library to the linker in GOFLAGS and in %s; "+
		"a -ldflags on a go command's own line replaces the one in GOFLAGS (conformance/README.md, the Go runner's row)\n",
		Library, built, Built)
	os.Exit(1)
}
