- The list of a subscription's images, and the image at an index, are now each a
  refusal where the ABI refuses them: from inside a handler of that subscription's own
  poll, with the ABI's code for a call during a poll, and after the subscription's own
  close, with its code for a null handle. The ABI answers −1 for the count there and
  no image for an index, which is also its answer for an index past the count, so
  each client asks the count when it is handed no image. The Go client's `Images`
  and `ImageAt` return the ABI's `*Error`, where `Images` panicked, "makeslice: cap
  out of range", and `ImageAt` returned no image and no error. The JVM's `images()`
  and `imageAt` and the .NET client's `Images()` and `ImageAt` throw their
  `CavalryException`, where the list threw a bare exception of its runtime for a
  capacity of −1 and the image was `null`. The Python client's `images()` and
  `image_at_index` raise `CavalryError` and the Node client's `images()` and
  `imageAtIndex` throw it, where they returned an empty list and no image: a Python
  or a Node handler that asks for its own subscription's images now raises at that
  line, and its `poll` raises `HandlerError` once the ABI has returned. The image
  count itself still returns −1 there. Each client's README has the rule, in "What it
  must never do"
  ([the Go client's](clients/go/README.md#what-it-must-never-do)).
