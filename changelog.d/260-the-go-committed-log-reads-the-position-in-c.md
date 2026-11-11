- The Go client's committed log reads each entry's position in its C trampoline,
  where the Go side made a cgo call of its own for it: one cgo call fewer for each
  entry ([the Go client](clients/go/README.md#the-call-back-by-construction); card
  260).
- The Python, Node, .NET and JVM clients close a committed log whose constants they
  cannot read, as the Go client does. The ABI fails that read for a null argument
  alone, so no program met it (card 260).
