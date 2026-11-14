- A cluster session's replies never go back a term. `ClusterSession::poll` now hands
  its handler each reply with a `ReplyHeader`, the leadership term of the entry the
  reply answers and the cluster's time, where it handed over the time alone: a
  handler `|payload, time|` becomes `|payload, header|`, with `header.timestamp`. A
  reply of a term below that of a reply the session has handed over is a stale
  reply. It reaches a client only around a change of leader, while the member that
  led before still answers. The session passes it over and counts it, in the new
  counter `cluster.replies_stale` on the client's driver and in
  `ClusterSession::stale_replies()`. Once a program has read a reply of a term, it
  reads no reply to an entry of an earlier term: where its service answers each
  message at the message's own entry, a message it offered in an earlier term that
  has no answer yet has none coming, and it sends the message again
  ([ADR 0121](docs/decisions/0121-hand-a-sessions-handler-each-replys-term-and-pass-over-a-reply-of-a-term-below-one-handed-over.md);
  work item 229).
