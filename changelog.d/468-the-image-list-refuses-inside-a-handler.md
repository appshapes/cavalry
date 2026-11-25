- The list of a subscription's images is now a refusal where the ABI refuses the image
  count: from inside a handler of that subscription's own poll, with the ABI's code for
  a call during a poll, and for a subscription that is closed. The Go client's `Images`
  returns the ABI's `*Error` and no slice, where it panicked, "makeslice: cap out of
  range". The JVM's `images()` and the .NET client's `Images()` throw their
  `CavalryException`, where each threw a bare exception of its runtime for a capacity
  of −1. The Python client's `images()` raises `CavalryError` and the Node client's
  `images()` throws it, where each returned an empty list. The image count itself
  still returns −1 there
  ([the Go client](clients/go/README.md#what-it-must-never-do)).
