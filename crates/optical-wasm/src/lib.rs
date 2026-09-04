#![forbid(unsafe_code)]

//! The browser-facing edge of the optical receiver.
//!
//! This crate is deliberately thin. All of the protocol lives in
//! `optical-core`, all of the receive-side sequencing lives in
//! [`ReceiverState`], and the only thing the `wasm_bindgen` layer adds is
//! serialisation — so a phone and a test binary run the same code, and there is
//! no second implementation of the wire format written in JavaScript.

mod receiver;

pub use receiver::{
    MAX_FRAMES_PER_SOURCE_BLOCK, MIN_FRAME_BUDGET, ReceiverEvent, ReceiverState, frame_budget,
};

#[cfg(target_arch = "wasm32")]
mod bindings {
    use wasm_bindgen::prelude::*;

    use crate::ReceiverState;

    /// One transfer's worth of receive state, owned by the JavaScript side.
    #[wasm_bindgen]
    pub struct WasmReceiver(ReceiverState);

    #[wasm_bindgen]
    impl WasmReceiver {
        #[wasm_bindgen(constructor)]
        pub fn new() -> Self {
            Self(ReceiverState::new())
        }

        /// Forget the current stream; the next frame starts a fresh transfer.
        pub fn reset(&mut self) {
            self.0.reset();
        }

        /// Feed one decoded QR payload in and get back a tagged event object.
        ///
        /// Errors here are serialisation failures only. Every protocol outcome,
        /// including the ones that mean "this went wrong", is a normal event —
        /// a caller should not have to use exceptions for control flow over a
        /// channel where a bad frame is the expected case.
        #[wasm_bindgen(js_name = addFrame)]
        pub fn add_frame(&mut self, bytes: &[u8]) -> Result<JsValue, JsValue> {
            serde_wasm_bindgen::to_value(&self.0.add_frame(bytes))
                .map_err(|error| JsValue::from_str(&error.to_string()))
        }
    }

    impl Default for WasmReceiver {
        fn default() -> Self {
            Self::new()
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use bindings::WasmReceiver;
