//! Receive mode: a camera pointed at another screen, decoded into a file.
//!
//! The pieces mirror the web receiver one for one, so the two schedule the
//! same work and report the same numbers: [`tracker`] is `capture.js`'s
//! `RegionTracker`, [`pool`] its `DecodePool`, [`telemetry`] is
//! `telemetry.js`, and [`session`] plays the part of `receive.js`, feeding the
//! one shared `optical_core::receiver::ReceiverState`.

pub mod camera;
pub mod telemetry;
pub mod tracker;

#[cfg(feature = "receive")]
pub mod decoder;
#[cfg(feature = "receive")]
pub mod pool;
#[cfg(feature = "receive")]
pub mod session;
#[cfg(feature = "receive")]
pub mod synthetic;
#[cfg(feature = "receive")]
pub mod unmix;
#[cfg(feature = "receive")]
pub mod view;
