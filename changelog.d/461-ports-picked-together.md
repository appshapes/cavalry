- `just conformance`'s C quickstart and the bench harness's multi-destination rows
  take the ports a step needs together, so that no two of them can be the same.
  Each used to take them one at a time, and the system could give one port twice:
  the C quickstart's step 2b then failed with `ChannelEndpointError`, about two runs
  in ten thousand. `quickstart-c --pick-ports <draws>` tests the picker alone
  ([`conformance/README.md`](conformance/README.md)).
