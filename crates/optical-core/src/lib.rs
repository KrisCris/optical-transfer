#![forbid(unsafe_code)]

pub mod bundle;
pub mod capacity;
pub mod container;
pub mod error;
pub mod fountain;
pub mod protocol;
pub mod qr;
pub mod raptor;
pub mod receiver;
pub mod snippet;
pub mod transfer;

/// The largest file either sender will send and either receiver will accept.
///
/// Wire v4 can describe 4 GiB and RaptorQ can code far more; this is a memory
/// budget instead. A browser receiving holds the received symbols, one source
/// block's decoding workspace, the assembled container and the unpacked file at
/// once, roughly four times the file, and a phone's browser tab is the tightest
/// place this runs.
pub const MAX_FILE_BYTES: usize = 128 * 1024 * 1024;

/// Upstream's limit, which wire v3 streams keep in both directions.
pub const V3_MAX_FILE_BYTES: usize = 64 * 1024 * 1024;

pub use error::OpticalError;
