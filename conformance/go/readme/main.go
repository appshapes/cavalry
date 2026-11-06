// Runs the example of clients/go/README.md against the runner's driver; run.sh calls it
// after quickstart, so the README's program is a test (CLAUDE.md, "Clients and developer
// experience"; .claude/rules/docs.md, "Code in docs"). The README's program is
// clients/go/example/main.go: this checks that the README's Go block is that file, byte for
// byte, builds the example with this Go from clients/go, as the README's `go run ./example`
// does, runs the binary for up to 10 s, and exits 1 unless it exits 0 and prints the line
// the README promises after "You will see". The README's shell lines around the program,
// the builds and the driver, are run.sh's own steps. Go's standard library only (ADR 0003).
package main

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"time"
)

const readme = "clients/go/README.md"
const wait = 10 * time.Second

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "conformance/go: "+readme+"'s example: "+format+"\n", args...)
	os.Exit(1)
}

func main() {
	root, err := filepath.Abs(filepath.Join(".", "..", ".."))
	if err != nil {
		fail("no root: %v", err)
	}
	text, err := os.ReadFile(filepath.Join(root, readme))
	if err != nil {
		fail("cannot read: %v", err)
	}
	program := regexp.MustCompile("(?s)```go\n(.*?)```\n").FindSubmatch(text)
	expected := regexp.MustCompile("(?m)^You will see `([^`]+)`\\.$").FindSubmatch(text)
	if program == nil || expected == nil {
		fail("no ```go block, or no line \"You will see `...`.\" after it; keep both shapes so this test can find them")
	}
	example, err := os.ReadFile(filepath.Join(root, "clients", "go", "example", "main.go"))
	if err != nil {
		fail("cannot read clients/go/example/main.go: %v", err)
	}
	if !bytes.Equal(program[1], example) {
		fail("the README's Go block is not clients/go/example/main.go; the README shows the program that runs, so change both")
	}
	binary := filepath.Join(os.TempDir(), fmt.Sprintf("cavalry-go-example-%d", os.Getpid()))
	defer os.Remove(binary)
	build := exec.Command("go", "build", "-o", binary, "./example")
	build.Dir = filepath.Join(root, "clients", "go")
	if out, err := build.CombinedOutput(); err != nil {
		fail("go build ./example failed: %v; it printed %q", err, out)
	}
	ctx, cancel := context.WithTimeout(context.Background(), wait)
	defer cancel()
	run := exec.CommandContext(ctx, binary)
	run.Dir = root
	var stdout, stderr bytes.Buffer
	run.Stdout, run.Stderr = &stdout, &stderr
	err = run.Run()
	if ctx.Err() != nil {
		fail("did not finish in %v; it printed %q", wait, stdout.String())
	}
	if err != nil {
		fail("exited with %v; it printed %q and %q", err, stdout.String(), stderr.String())
	}
	line := string(expected[1])
	for _, printed := range strings.Split(stdout.String(), "\n") {
		if printed == line {
			fmt.Printf("conformance/go: %s's example printed `%s`, as the README says\n", readme, line)
			return
		}
	}
	fail("printed %q, not the line the README says the reader will see, %q", stdout.String(), line)
}
