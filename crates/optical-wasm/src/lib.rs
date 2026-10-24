#![forbid(unsafe_code)]

//! The browser-facing edge of the optical sender and receiver.
//!
//! This crate is deliberately thin. All of the protocol lives in
//! `optical-core`, including the receive-side sequencing in
//! [`ReceiverState`], and the only thing the `wasm_bindgen` layer adds is
//! serialisation — so a phone and a test binary run the same code, and there is
//! no second implementation of the wire format written in JavaScript.

mod sender;

pub use sender::{SenderInfo, SenderState};

pub use optical_core::receiver::{
    MAX_FRAMES_PER_SOURCE_BLOCK, MIN_FRAME_BUDGET, ReceiverEvent, ReceiverState, ReceiverStats,
    frame_budget,
};

#[cfg(target_arch = "wasm32")]
mod bindings {
    use wasm_bindgen::prelude::*;

    use crate::{ReceiverEvent, ReceiverState, ReceiverStats, SenderState};

    /// File preparation and QR generation run inside a dedicated browser worker.
    #[wasm_bindgen]
    pub struct WasmSender(SenderState);

    #[wasm_bindgen]
    impl WasmSender {
        #[wasm_bindgen(constructor)]
        pub fn new() -> Self {
            Self(SenderState::new())
        }

        #[wasm_bindgen(js_name = addFile)]
        pub fn add_file(
            &mut self,
            name: &str,
            media_type: &str,
            bytes: Vec<u8>,
        ) -> Result<(), JsValue> {
            self.0
                .add_file(name, media_type, bytes)
                .map_err(|error| JsValue::from_str(&error))
        }

        #[wasm_bindgen(js_name = addDirectoryFile)]
        pub fn add_directory_file(
            &mut self,
            name: &str,
            media_type: &str,
            relative_path: &str,
            bytes: Vec<u8>,
        ) -> Result<(), JsValue> {
            self.0
                .add_directory_file(name, media_type, relative_path, bytes)
                .map_err(|error| JsValue::from_str(&error))
        }

        #[wasm_bindgen(js_name = prepareFiles)]
        pub fn prepare_files(
            &mut self,
            frame_bytes: usize,
            fps: u32,
            ecc: &str,
            session_id: u16,
        ) -> Result<JsValue, JsValue> {
            let info = self
                .0
                .prepare_files(frame_bytes, fps, ecc, session_id)
                .map_err(|error| JsValue::from_str(&error))?;
            serde_wasm_bindgen::to_value(&info)
                .map_err(|error| JsValue::from_str(&error.to_string()))
        }

        #[wasm_bindgen(js_name = prepareText)]
        pub fn prepare_text(
            &mut self,
            text: &str,
            frame_bytes: usize,
            fps: u32,
            ecc: &str,
            session_id: u16,
        ) -> Result<JsValue, JsValue> {
            let info = self
                .0
                .prepare_text(text, frame_bytes, fps, ecc, session_id)
                .map_err(|error| JsValue::from_str(&error))?;
            serde_wasm_bindgen::to_value(&info)
                .map_err(|error| JsValue::from_str(&error.to_string()))
        }

        /// `count` frames of modules back to back, one byte per module.
        #[wasm_bindgen(js_name = nextFrames)]
        pub fn next_frames(&mut self, count: usize) -> Result<Vec<u8>, JsValue> {
            self.0
                .next_frames(count)
                .map_err(|error| JsValue::from_str(&error))
        }
    }

    impl Default for WasmSender {
        fn default() -> Self {
            Self::new()
        }
    }

    /// `addFrame` outcomes. A small integer per frame rather than a
    /// serialised event: at a few hundred decoded codes a second the object
    /// allocations were measurable, and the UI only reads the details at its
    /// own render rate.
    const IGNORED: u8 = 0;
    const PROGRESS: u8 = 1;
    const COMPLETE: u8 = 2;
    const INCOMPATIBLE: u8 = 3;
    const FAILED: u8 = 4;
    /// OR-ed into the outcome when this frame started a new stream.
    const NEW_STREAM: u8 = 0x80;
    /// OR-ed in when the frame carried information: a sequence number not
    /// seen before, covering at least one block not yet solved.
    const USEFUL: u8 = 0x40;

    struct Received {
        name: String,
        media_type: String,
        bytes: Vec<u8>,
        is_text: bool,
    }

    /// One receiver's state, owned by the JavaScript side.
    #[wasm_bindgen]
    pub struct WasmReceiver {
        state: ReceiverState,
        stats: Option<ReceiverStats>,
        message: String,
        received: Option<Received>,
    }

    #[wasm_bindgen]
    impl WasmReceiver {
        #[wasm_bindgen(constructor)]
        pub fn new() -> Self {
            Self {
                state: ReceiverState::new(),
                stats: None,
                message: String::new(),
                received: None,
            }
        }

        /// Forget the current stream; the next frame starts a fresh transfer.
        pub fn reset(&mut self) {
            self.state.reset();
            self.stats = None;
            self.received = None;
        }

        /// Feed one decoded QR payload in. Returns an outcome code; every
        /// protocol result, including failures, is a normal return.
        #[wasm_bindgen(js_name = addFrame)]
        pub fn add_frame(&mut self, bytes: &[u8]) -> u8 {
            let before = self.stats.as_ref().map_or((0, 0), |stats| {
                (stats.unique_frames, stats.redundant_frames)
            });
            let useful = |new_stream: bool, stats: &ReceiverStats| {
                let (unique, redundant) = if new_stream { (0, 0) } else { before };
                if stats.unique_frames > unique && stats.redundant_frames == redundant {
                    USEFUL
                } else {
                    0
                }
            };
            match self.state.add_frame(bytes) {
                ReceiverEvent::Ignored => IGNORED,
                ReceiverEvent::Incompatible { message } => {
                    self.message = message;
                    INCOMPATIBLE
                }
                ReceiverEvent::Failed { message } => {
                    self.message = message;
                    self.stats = None;
                    FAILED
                }
                ReceiverEvent::Progress { new_stream, stats } => {
                    let flags =
                        useful(new_stream, &stats) | if new_stream { NEW_STREAM } else { 0 };
                    self.stats = Some(stats);
                    PROGRESS | flags
                }
                ReceiverEvent::Complete {
                    new_stream,
                    stats,
                    name,
                    media_type,
                    bytes,
                    is_text,
                } => {
                    let flags =
                        useful(new_stream, &stats) | if new_stream { NEW_STREAM } else { 0 };
                    self.stats = Some(stats);
                    self.received = Some(Received {
                        name,
                        media_type,
                        bytes,
                        is_text,
                    });
                    COMPLETE | flags
                }
            }
        }

        /// Counters for the current (or just-finished) stream, or `undefined`.
        pub fn stats(&self) -> JsValue {
            self.stats
                .as_ref()
                .and_then(|stats| serde_wasm_bindgen::to_value(stats).ok())
                .unwrap_or(JsValue::UNDEFINED)
        }

        /// The explanation behind the last incompatible or failed outcome.
        pub fn message(&self) -> String {
            self.message.clone()
        }

        /// One byte per source block, 1 where recovered.
        #[wasm_bindgen(js_name = solvedMap)]
        pub fn solved_map(&self) -> Vec<u8> {
            self.state.solved_map()
        }

        /// The verified result of the last `COMPLETE`, handed over once.
        #[wasm_bindgen(js_name = takeResult)]
        pub fn take_result(&mut self) -> Option<WasmReceived> {
            self.received.take().map(WasmReceived)
        }
    }

    /// A verified file or snippet. Both checksums passed before this existed.
    #[wasm_bindgen]
    pub struct WasmReceived(Received);

    #[wasm_bindgen]
    impl WasmReceived {
        #[wasm_bindgen(getter)]
        pub fn name(&self) -> String {
            self.0.name.clone()
        }

        #[wasm_bindgen(getter, js_name = mediaType)]
        pub fn media_type(&self) -> String {
            self.0.media_type.clone()
        }

        #[wasm_bindgen(getter, js_name = isText)]
        pub fn is_text(&self) -> bool {
            self.0.is_text
        }

        /// Copied out into a `Uint8Array`.
        #[wasm_bindgen(getter)]
        pub fn bytes(&self) -> Vec<u8> {
            self.0.bytes.clone()
        }
    }

    impl Default for WasmReceiver {
        fn default() -> Self {
            Self::new()
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use bindings::{WasmReceived, WasmReceiver, WasmSender};
