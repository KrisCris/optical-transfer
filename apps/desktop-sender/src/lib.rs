#![forbid(unsafe_code)]

//! The desktop sender's testable core.
//!
//! Split out of the binary so the frame pipeline — the seam between payload
//! bytes and QR modules — can be tested without opening a window.

pub mod app;
pub mod packing;
pub mod qr;
pub mod theme;
pub mod transfer;
