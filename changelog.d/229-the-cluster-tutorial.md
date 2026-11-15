- A tutorial runs a cluster in ten minutes:
  [Run a cluster in ten minutes](docs/tutorials/run-a-cluster.md). Three members on one
  host, a service beside each, and a client that sends numbers and reads the total. The
  leader is stopped, another member's service answers with the same total, and the
  stopped member comes back and reads the log to it. Its two programs are examples of
  the `cavalry` crate, `adder-service` and `adder-client`, and `just quickstart` runs
  the page's path after the README's
  ([ADR 0126](docs/decisions/0126-run-the-cluster-tutorials-path-in-just-quickstart.md);
  work item 229).
