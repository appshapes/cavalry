- A cluster member whose read of the consensus stream fails after a message of the
  same read faulted in the election now reports both faults, the message's first:
  `ConsensusModule::do_work` returns the message's fault from that call and the
  read's, `RaftError::Poll`, from the next. Before, it returned the read's error and
  dropped the message's, so the member's end named the wrong cause
  ([`cavalry-raft`'s README](crates/cavalry-raft/README.md)).
