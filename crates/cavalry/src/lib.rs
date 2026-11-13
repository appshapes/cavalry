#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

pub use cavalry_client::{
    Action, ArmedRecording, BufferClaim, Cavalry, ClientError, Closed, ClusterError,
    ClusterSession, CommittedLog, Counter, DataFrameHeader, FOLLOW_LENGTH, Fault, Flags, FrameType,
    Image, MAX_IMAGES, NULL_LENGTH, NULL_POSITION, Offer, Positions, Publication, Recording,
    Replay, ReplayStopReason, SERVICE_LOG_STREAM_ID, Subscription,
};
pub use cavalry_driver::{
    Driver, DriverError, DriverThread, IdleStrategy, Idler, ThreadStart, spec,
};
pub use cavalry_log::index;

/// A cluster member: `cavalry-raft`, whose `ConsensusModule` replicates the cluster's
/// log across drivers and commits it at a quorum, a client of the driver beside it
/// (ADR 0063). `cavalry run` starts one when `cluster.members` and `cluster.member.id`
/// are set.
pub use cavalry_raft as cluster;

#[cfg(feature = "cli")]
pub use cavalry_control::{config, doctor};
