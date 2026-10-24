#![forbid(unsafe_code)]

pub mod bundle;
pub mod capacity;
pub mod container;
pub mod error;
pub mod fountain;
pub mod protocol;
pub mod qr;
pub mod receiver;
pub mod snippet;
pub mod transfer;

pub const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;

pub use error::OpticalError;
