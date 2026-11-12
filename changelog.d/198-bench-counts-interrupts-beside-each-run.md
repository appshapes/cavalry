- `just bench --pin` writes a second file beside its result,
  `bench/results/interrupts/<profile>-<unix time>.json`: the interrupts and softirqs
  the kernel counted on each pinned CPU around each run's measured loop, by benchmark,
  CPU and the kernel's label, one count per run in the result's run order. On one bench
  host about one run in two hundred of a one-way row reads a p99.99 of 1.3 to 2.3 times
  its row's, and the counts around a whole process could not name the interrupt behind
  it. The gate reads the result alone, and an unpinned run writes no such file
  ([Run the bench](docs/how-to/run-the-bench.md#when-it-does-not-work); card 198).
