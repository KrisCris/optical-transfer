#![forbid(unsafe_code)]

pub mod capacity;
pub mod container;
pub mod error;
pub mod fountain;
pub mod protocol;
pub mod snippet;

pub const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;

pub use error::OpticalError;
