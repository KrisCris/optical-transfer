//! QR decoding: upstream zxing-cpp, linked natively.
//!
//! zxing-cpp rather than the pure-Rust `rxing` because it read version 40
//! codes more reliably and faster where it counts. Measured on synthetic
//! camera frames (2×2 supersampled, rotated, blurred, contrast-squeezed and
//! noised) of the frames `optical-core` renders, 20 random payloads per case,
//! release builds on Apple Silicon:
//!
//! | case                                   | rxing  | ms   | zxing-cpp | ms  |
//! |----------------------------------------|--------|------|-----------|-----|
//! | v40, 1280×960, 4.3 px/module, 2°       | 20/20  | 4.4  | 20/20     | 5.2 |
//! | v40, 1280×960, 3.2 px/module, 3°       | 15/20  | 11.1 | 20/20     | 8.8 |
//! | v40 crop 640², 3.2 px/module, 3°       | 15/20  | 2.8  | 20/20     | 2.2 |
//! | v40 2×2 grid, 1280×960, 2.5 px/module  | 71/80  | 23.9 | 67/80     | 8.4 |
//! | v40 crop 520², 2.5 px/module           | 16/20  | 3.4  | 19/20     | 2.0 |
//!
//! Crops are the hot path once codes are locked, and there zxing-cpp is both
//! the more reliable and the faster. It is also the engine the web receiver
//! runs (as the vendored `decimen-codec` WebAssembly build), so both receivers
//! fail on the same captures. The vendored build's tracked fast path is a
//! custom addition to zxing-cpp that upstream does not have, so every crop
//! here is a detect-and-decode.

use std::time::Instant;

use zxingcpp::{BarcodeFormat, BarcodeReader, ImageFormat, ImageView};

use crate::receive::tracker::{Bounds, Crop};

/// One decoded code, in whole-frame pixels.
#[derive(Clone, Debug)]
pub struct Symbol {
    pub bytes: Vec<u8>,
    pub bounds: Bounds,
}

#[derive(Debug, Default)]
pub struct Reply {
    pub symbols: Vec<Symbol>,
    /// Codes detected but not decoded — still a fix on where a code sits.
    pub sightings: Vec<Bounds>,
    pub decode_ms: f64,
}

/// Readers configured once; each pool worker owns one.
pub struct Decoder {
    full: BarcodeReader,
    crop: BarcodeReader,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Self {
        // `try_harder` stays on everywhere: marginal captures are where it
        // earns its cost. Full scans ask for error results too, because a
        // symbol detected but not decoded is still a fix on where a code sits,
        // and a crop aimed there often decodes where the full frame did not.
        // Error results count against the symbol cap, hence the headroom —
        // the same limits the web worker passes.
        let reader = || {
            zxingcpp::read()
                .formats([BarcodeFormat::QRCode])
                .try_harder(true)
                .try_rotate(true)
                .try_invert(false)
        };
        Self {
            full: reader().return_errors(true).max_number_of_symbols(12),
            crop: reader().return_errors(false).max_number_of_symbols(2),
        }
    }

    /// Decode `area` of a `width`×`height` luma frame. A full scan also
    /// reports sightings.
    pub fn decode(
        &self,
        luma: &[u8],
        width: usize,
        height: usize,
        area: Crop,
        full: bool,
    ) -> Reply {
        let started = Instant::now();
        let mut reply = Reply::default();
        let Ok(view) = ImageView::from_slice(luma, width as i32, height as i32, ImageFormat::Lum)
        else {
            return reply;
        };
        let view = if full {
            view
        } else {
            view.cropped(area.x as i32, area.y as i32, area.w as i32, area.h as i32)
        };
        let reader = if full { &self.full } else { &self.crop };
        let (offset_x, offset_y) = if full {
            (0.0, 0.0)
        } else {
            (area.x as f32, area.y as f32)
        };
        if let Ok(barcodes) = reader.from(view) {
            for barcode in barcodes {
                let position = barcode.position();
                let corners = [
                    position.top_left,
                    position.top_right,
                    position.bottom_right,
                    position.bottom_left,
                ];
                let xs = corners.map(|point| point.x as f32);
                let ys = corners.map(|point| point.y as f32);
                let (left, top) = (
                    xs.iter().copied().fold(f32::MAX, f32::min),
                    ys.iter().copied().fold(f32::MAX, f32::min),
                );
                let (right, bottom) = (
                    xs.iter().copied().fold(f32::MIN, f32::max),
                    ys.iter().copied().fold(f32::MIN, f32::max),
                );
                let bounds = Bounds {
                    x: offset_x + left,
                    y: offset_y + top,
                    w: right - left,
                    h: bottom - top,
                };
                if barcode.is_valid() {
                    let bytes = barcode.bytes();
                    if !bytes.is_empty() {
                        reply.symbols.push(Symbol { bytes, bounds });
                    }
                } else if full && bounds.w > 0.0 && bounds.h > 0.0 {
                    reply.sightings.push(bounds);
                }
            }
        }
        reply.decode_ms = started.elapsed().as_secs_f64() * 1000.0;
        reply
    }
}

/// The bundled zxing-cpp release, for the telemetry header.
pub const ENGINE: &str = "zxing-cpp 3.1.1";
