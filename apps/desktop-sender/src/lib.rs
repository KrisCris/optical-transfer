// Unsafe is confined to the AVFoundation bindings in `receive::camera`.
#![deny(unsafe_code)]

//! The desktop sender's testable core, split from the binary so the packing
//! thread can be exercised without opening a window.

pub mod app;
pub mod packing;
pub mod receive;
pub mod stream;
pub mod theme;
