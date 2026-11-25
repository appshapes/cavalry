- Version 3 of the cluster protocol. A cluster member that came back with its log empty
  now also waits for its leader's answer to its present run before it votes again. While
  it holds a vote floor it asks with a bit of its append position that earlier versions
  ignore, and a leader of version 3 answers with its append position and the session id
  of the member's report. The member takes only an answer to its own run and clears its
  floor once its log reaches it, so a report that its earlier run sent and that reaches
  the leader late can no longer commit an entry the member's vote could lose. The new
  `cluster.floor_answers_sent`, `cluster.floor_answers_ignored` and the gauge
  `cluster.floor_answer` count it, and `cavalry doctor` names a member whose floor no
  leader has answered, as beside a leader of an earlier version during an upgrade
  ([ADR 0163](docs/decisions/0163-clear-a-vote-floor-only-on-a-leaders-answer-to-the-present-run.md);
  work item 463).
