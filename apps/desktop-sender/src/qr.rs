//! Frame bytes to QR modules.
//!
//! Two decisions here are wire-adjacent rather than cosmetic, and both are
//! ported from upstream deliberately:
//!
//! **The mask is pinned.** Any declared mask is valid to a decoder, so fixing
//! it skips the specification's eight-way mask evaluation. That is roughly a
//! four-times speedup on generation, which matters at 24 frames a second, and
//! it is what makes the second decision possible.
//!
//! **The version is locked to the stream's first code.** Every frame in a
//! transfer carries the same byte count at the same ECC with the same mask, so
//! they all land on identical geometry — which grid tiling and partial-cell
//! blitting both require, and which a receiver tracking a fixed region on
//! screen benefits from. Without locking, a frame that happened to compress a
//! little differently could shift version and resize the code mid-stream.

use qrcodegen::{DataTooLong, Mask, QrCode, QrCodeEcc, QrSegment, Version};

/// Quiet-zone modules around every code. Four is the specification's minimum
/// and what upstream uses; below it a decoder loses the finder patterns.
pub const QUIET_ZONE_MODULES: usize = 4;

/// Pinned mask. The value itself is arbitrary — what matters is that it never
/// varies within a stream.
const PINNED_MASK: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Ecc {
    /// ~7% recovery. The default: at a fixed frame size, lower ECC means a
    /// smaller QR version, which means larger modules on screen, which is what
    /// a phone camera actually struggles with.
    #[default]
    Low,
    Medium,
    Quartile,
    High,
}

impl Ecc {
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::Quartile, Self::High];

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "L",
            Self::Medium => "M",
            Self::Quartile => "Q",
            Self::High => "H",
        }
    }

    fn to_qrcodegen(self) -> QrCodeEcc {
        match self {
            Self::Low => QrCodeEcc::Low,
            Self::Medium => QrCodeEcc::Medium,
            Self::Quartile => QrCodeEcc::Quartile,
            Self::High => QrCodeEcc::High,
        }
    }
}

/// One frame as a square raster of light and dark modules, quiet zone included.
///
/// One `bool` per module rather than per pixel: scaling to the display is the
/// caller's business, and it must be nearest-neighbour — a smoothed QR code is
/// an undecodable QR code.
#[derive(Clone, Debug)]
pub struct QrFrame {
    /// Modules per side including the quiet zone.
    pub size: usize,
    /// Modules per side of the code itself.
    pub modules: usize,
    /// QR version, 1 to 40. Constant for the life of a stream.
    pub version: u8,
    /// Row-major, `true` is dark.
    pub pixels: Vec<bool>,
}

/// Renders every frame of one stream at identical geometry.
///
/// Stateful on purpose: the first frame chooses the version and every frame
/// after it is pinned to that choice.
pub struct FrameRenderer {
    ecc: Ecc,
    version: Option<Version>,
}

impl FrameRenderer {
    pub fn new(ecc: Ecc) -> Self {
        Self { ecc, version: None }
    }

    /// The version this stream locked onto, once its first frame is rendered.
    pub fn locked_version(&self) -> Option<u8> {
        self.version.map(Version::value)
    }

    pub fn render(&mut self, payload: &[u8]) -> Result<QrFrame, DataTooLong> {
        let segments = [QrSegment::make_bytes(payload)];
        let (min, max) = match self.version {
            Some(version) => (version, version),
            None => (Version::MIN, Version::MAX),
        };
        let code = QrCode::encode_segments_advanced(
            &segments,
            self.ecc.to_qrcodegen(),
            min,
            max,
            // A pinned mask, and no ECC boosting: boosting would silently raise
            // the level when a frame left spare capacity, which is exactly the
            // per-frame variation version locking exists to prevent.
            Some(Mask::new(PINNED_MASK)),
            false,
        )?;
        self.version = Some(code.version());
        Ok(rasterize(&code))
    }
}

fn rasterize(code: &QrCode) -> QrFrame {
    let modules = code.size() as usize;
    let size = modules + 2 * QUIET_ZONE_MODULES;
    let mut pixels = vec![false; size * size];
    for y in 0..modules {
        let row = (y + QUIET_ZONE_MODULES) * size + QUIET_ZONE_MODULES;
        for x in 0..modules {
            if code.get_module(x as i32, y as i32) {
                pixels[row + x] = true;
            }
        }
    }
    QrFrame {
        size,
        modules,
        version: code.version().value(),
        pixels,
    }
}

/// Grid shape for a code count: as square as possible, taller before wider.
///
/// The count must fill the rectangle exactly — 1, 2, 4, 6, 9… — because a
/// part-empty grid would silently waste the channel.
pub fn grid_dims(count: usize) -> Option<(usize, usize)> {
    if count == 0 {
        return None;
    }
    let cols = (count as f64).sqrt().floor() as usize;
    let cols = cols.max(1);
    let rows = count.div_ceil(cols);
    (cols * rows == count).then_some((cols, rows))
}

/// Code counts that tile without a gap, for the sender's own control.
pub const GRID_OPTIONS: [usize; 5] = [1, 2, 4, 6, 9];
