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
//!
//! A colour code is three codes in the red, green and blue channels sharing
//! one geometry. Read for brightness it is found and does not decode, or
//! decodes as the layer luma favours, so wherever a code has colour in it the
//! layers are unmixed ([`crate::receive::unmix`]) and each is read on its own:
//! green first, which carries most of luma and survives coarse camera colour
//! best, then red and blue, sharpened at green's module pitch if they miss.

use std::time::Instant;

use zxingcpp::{BarcodeFormat, BarcodeReader, ImageFormat, ImageView, PointI};

use crate::receive::{
    camera::LumaFrame,
    tracker::{Bounds, Crop},
    unmix::{Unmixing, layers, sharpen, unmixing},
};

/// Codes a full scan checks for colour.
const COLOR_SIGHTINGS: usize = 4;

/// Green first: it carries most of luma, so it survives a camera's coarse
/// colour best, and the pitch it reads at is what the others sharpen at.
const LAYER_ORDER: [usize; 3] = [1, 0, 2];

/// One decoded code, in whole-frame pixels.
#[derive(Clone, Debug)]
pub struct Symbol {
    pub bytes: Vec<u8>,
    pub bounds: Bounds,
    /// Read from one layer of a colour code.
    pub colour: bool,
    /// Frame pixels per module, where zxing reported the code's version.
    pub pitch: Option<f32>,
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

    /// Decode `area` of a camera frame. A full scan also reports sightings.
    pub fn decode(&self, frame: &LumaFrame, area: Crop, full: bool) -> Reply {
        let started = Instant::now();
        let mut reply = Reply::default();
        // Colour first wherever the code has colour in it. A crop is the
        // code's last bounds padded on every side, so its middle is code.
        if !full {
            let middle = Crop {
                x: area.x + area.w * 3 / 10,
                y: area.y + area.h * 3 / 10,
                w: area.w * 2 / 5,
                h: area.h * 2 / 5,
            };
            if let Some(unmix) = unmixing(frame, middle) {
                self.read_layers(frame, area, &unmix, None, &mut reply);
            }
        }
        if reply.symbols.is_empty()
            && let Ok(view) = ImageView::from_slice(
                &frame.pixels,
                frame.width as i32,
                frame.height as i32,
                ImageFormat::Lum,
            )
        {
            if full {
                self.read(&self.full, view, (0.0, 0.0), true, false, &mut reply);
            } else {
                let view = view.cropped(area.x as i32, area.y as i32, area.w as i32, area.h as i32);
                let offset = (area.x as f32, area.y as f32);
                self.read(&self.crop, view, offset, false, false, &mut reply);
            }
        }
        if full {
            self.read_colour(frame, &mut reply);
        }
        reply.decode_ms = started.elapsed().as_secs_f64() * 1000.0;
        reply
    }

    /// A full scan is how a colour code is first acquired. Read for
    /// brightness it is found and not decoded, or decoded as whichever layer
    /// dominates luma (green), so every code the scan found is checked for
    /// colour and, where it has some, read layer by layer instead.
    fn read_colour(&self, frame: &LumaFrame, reply: &mut Reply) {
        let found: Vec<(Bounds, Option<Symbol>)> = reply
            .symbols
            .drain(..)
            .map(|symbol| (symbol.bounds, Some(symbol)))
            .chain(reply.sightings.drain(..).map(|bounds| (bounds, None)))
            .collect();
        let mut attempts = 0;
        for (bounds, symbol) in found {
            let centre = (bounds.x + bounds.w / 2.0, bounds.y + bounds.h / 2.0);
            let layered = reply.symbols.iter().any(|symbol| {
                let b = symbol.bounds;
                symbol.colour
                    && (b.x..=b.x + b.w).contains(&centre.0)
                    && (b.y..=b.y + b.h).contains(&centre.1)
            });
            if layered {
                continue;
            }
            let unmix = (attempts < COLOR_SIGHTINGS)
                .then(|| unmixing(frame, crop_of(bounds, 0.0, frame)))
                .flatten();
            let before = reply.symbols.len();
            if let Some(unmix) = unmix {
                attempts += 1;
                // A code read for brightness is the layer luma favours, and
                // its pitch is the colour layers' too.
                let pitch = symbol.as_ref().and_then(|symbol| symbol.pitch);
                self.read_layers(frame, crop_of(bounds, 0.1, frame), &unmix, pitch, reply);
            }
            if reply.symbols.len() == before {
                match symbol {
                    Some(symbol) => reply.symbols.push(symbol),
                    None => reply.sightings.push(bounds),
                }
            }
        }
    }

    /// The three layers of a colour code in `crop`, each decoded on its own.
    /// `pitch` is the code's pixels per module where already known; a layer
    /// read here supplies it otherwise.
    fn read_layers(
        &self,
        frame: &LumaFrame,
        crop: Crop,
        unmix: &Unmixing,
        mut pitch: Option<f32>,
        reply: &mut Reply,
    ) {
        let offset = (crop.x as f32, crop.y as f32);
        let mut layers = layers(frame, crop, unmix);
        let read = |layer: &[u8], reply: &mut Reply| {
            if let Ok(view) =
                ImageView::from_slice(layer, crop.w as i32, crop.h as i32, ImageFormat::Lum)
            {
                self.read(&self.crop, view, offset, false, true, reply);
            }
        };
        for channel in LAYER_ORDER {
            let layer = &mut layers[channel];
            let before = reply.symbols.len();
            read(layer, reply);
            // Coarse camera colour smears a layer's modules into their
            // neighbours, which sharpening at the module pitch mostly undoes.
            // Only a layer that missed pays for it.
            if reply.symbols.len() == before
                && let Some(pitch) = pitch
            {
                sharpen(layer, crop.w, crop.h, pitch);
                read(layer, reply);
            }
            pitch = pitch.or_else(|| {
                reply.symbols[before..]
                    .iter()
                    .find_map(|symbol| symbol.pitch)
            });
        }
    }

    /// Run `reader` over `view`, whose origin sits at `offset` in the frame.
    fn read(
        &self,
        reader: &BarcodeReader,
        view: ImageView<'_>,
        offset: (f32, f32),
        sightings: bool,
        colour: bool,
        reply: &mut Reply,
    ) {
        let Ok(barcodes) = reader.from(view) else {
            return;
        };
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
                x: offset.0 + left,
                y: offset.1 + top,
                w: right - left,
                h: bottom - top,
            };
            if barcode.is_valid() {
                let bytes = barcode.bytes();
                if !bytes.is_empty() {
                    let version = barcode.extra_with_key("Version");
                    let pitch = version
                        .trim_matches('"')
                        .parse::<usize>()
                        .ok()
                        .map(|version| {
                            let length = |a: PointI, b: PointI| {
                                ((b.x - a.x) as f32).hypot((b.y - a.y) as f32)
                            };
                            let sides = length(corners[0], corners[1])
                                + length(corners[3], corners[2])
                                + length(corners[0], corners[3])
                                + length(corners[1], corners[2]);
                            sides / (4 * (17 + 4 * version)) as f32
                        });
                    reply.symbols.push(Symbol {
                        bytes,
                        bounds,
                        colour,
                        pitch,
                    });
                }
            } else if sightings && bounds.w > 0.0 && bounds.h > 0.0 {
                reply.sightings.push(bounds);
            }
        }
    }
}

/// `bounds` grown by `fraction` of its size on every side, as whole pixels
/// inside the frame.
fn crop_of(bounds: Bounds, fraction: f32, frame: &LumaFrame) -> Crop {
    let pad = bounds.w.max(bounds.h) * fraction;
    let x = (bounds.x - pad).floor().max(0.0) as usize;
    let y = (bounds.y - pad).floor().max(0.0) as usize;
    let right = ((bounds.x + bounds.w + pad).ceil().max(0.0) as usize).min(frame.width);
    let bottom = ((bounds.y + bounds.h + pad).ceil().max(0.0) as usize).min(frame.height);
    Crop {
        x: x.min(right),
        y: y.min(bottom),
        w: right.saturating_sub(x),
        h: bottom.saturating_sub(y),
    }
}

/// The bundled zxing-cpp release, for the telemetry header.
pub const ENGINE: &str = "zxing-cpp 3.1.1";
