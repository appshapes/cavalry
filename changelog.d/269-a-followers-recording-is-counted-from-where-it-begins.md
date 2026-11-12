- Fixed: a cluster follower whose leader's image arrived between its subscription
  and the arming of its recording counted the recording from the image's join
  position, though the recording began later, at what the image had received by
  then: it could report an append position for bytes no recording of its held. The
  consensus module now reads where a recording begins from the driver's catalog and
  tells its election that position, so such a follower fails one election, counted
  in `cluster.join_position_mismatches`, and joins again. A recording taken by an
  image of another session is no longer counted either. `ConsensusModule::start`
  needs its `log_dir` to be its driver's log directory, as it has always said, and
  returns the new `RaftError::Catalog` when the directory holds no catalog
  ([ADR 0093](docs/decisions/0093-take-where-a-members-recording-of-the-log-begins-from-the-catalog-and-count-it-once-its-image-is-checked.md)).
