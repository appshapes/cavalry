- The Go, the JVM and the .NET client now refuse a count or a length below zero before
  the ABI is called: a claim's length, a poll's fragment limit, and the index of the
  image at an index. The ABI takes a count as a `size_t`, and each client cast its
  signed integer with the bits kept, so `TryClaim(-1)` was a claim of 2^64 − 1 bytes,
  refused with a length the caller never gave, and `Poll(handler, -1)` a poll with no
  limit. The JVM client throws `IllegalArgumentException` and the .NET client
  `ArgumentOutOfRangeException`, each naming the parameter. The Go client returns the
  function's own failure, the ABI's code for an error from `TryClaim` and −1 from a
  poll, with an `Err` whose `Code` is the new `ErrNegative`, and an error from
  `ImageAt`; it does not panic. A program that passed −1 as a fragment limit to mean
  no limit now passes a count that says so, such as the greatest `int`: the ABI reads
  a limit above 2^31 − 1 as 2^31 − 1
  ([ADR 0164](docs/decisions/0164-refuse-a-count-below-zero-in-the-go-the-jvm-and-the-dotnet-client-before-the-abi-is-called.md)).
- `spec/c-abi.md` now says that no error code of the ABI is below zero, in this
  version or a later one, so that a client can give a failure of its own a code the
  ABI never gives. The Go client's `ErrNegative` is −1.
- A refusal of the Node client now names the parameter as its method names it,
  `recordingId` and `message`, where it named the ABI's, `recording_id` and `buffer`
  ([ADR 0164](docs/decisions/0164-refuse-a-count-below-zero-in-the-go-the-jvm-and-the-dotnet-client-before-the-abi-is-called.md)).
