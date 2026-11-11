- A refused command's message names the page and section that explains it once, and
  ends with it. The client no longer adds `; see
  docs/reference/client-protocol.md#error-codes` after a driver's message that
  already ends with its section, and the driver's refusal of a `sync recording`
  whose sync failed now ends with its section, after what the sync said, as every
  other error message does
  ([error codes](docs/reference/client-protocol.md#error-codes); card 258).
