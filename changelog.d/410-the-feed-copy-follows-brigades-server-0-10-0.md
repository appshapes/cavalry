- The copy of Brigade's feed in `just test` now follows its server 0.10.0, the tag
  `server-v0.10.0`, and its fix for a recording that ends in a term's padding: the
  reader keeps the furthest position the replay's image stood at, so a start reads such
  a recording to its end however late it learns that the replay ended. Two new tests
  hold it, one on a recording a stop left and one on a recording a kill left, which
  ends at its last record answered or at the term's end. The pages on Brigade's server
  now tell apart server 0.9.0, which the servers in production run on 620a2e29, server
  0.10.0 on d265af82, and what Brigade's `master` pins
  ([What does Brigade's server rely on?](docs/explanation/what-brigades-server-relies-on.md);
  work item 410).
