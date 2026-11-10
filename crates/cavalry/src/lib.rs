#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

pub use cavalry_client::{
    ArmedRecording, BufferClaim, Cavalry, ClientError, Closed, Counter, DataFrameHeader,
    FOLLOW_LENGTH, Fault, Flags, FrameType, Image, MAX_IMAGES, NULL_LENGTH, NULL_POSITION, Offer,
    Positions, Publication, Recording, Replay, ReplayStopReason, Subscription,
};
pub use cavalry_driver::{Driver, DriverError, DriverThread, IdleStrategy, ThreadStart, spec};
pub use cavalry_log::index;

/// A cluster member: the consensus module that replicates the cluster's log across
/// drivers and commits it at a quorum, a client of the driver beside it (ADR 0063).
/// `cavalry run` starts one when `cluster.members` and `cluster.member.id` are set.
pub mod cluster {
    pub use cavalry_raft::{
        ConsensusModule, Counts, ElectionState, MAX_PAYLOAD_LENGTH, RaftError, Refused, Role,
    };
}

#[cfg(feature = "cli")]
pub use cavalry_control::{config, doctor};
