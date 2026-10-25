//! Colour codes: separating the three layers a camera mixed.
//!
//! The native counterpart of the web receiver's `web/app/unmix.js`. A colour
//! code (`optical_core::qr::compose`) puts one QR code in each of the red,
//! green and blue channels. A camera does not keep them apart: its filters
//! overlap, its colour correction mixes them, and 4:2:0 video keeps colour at
//! half resolution. The layers are independent, though, so the mixture is
//! undone blind from the picture's own statistics: the symmetric whitening
//! (ZCA) of the colours inside the code. ZCA is the decorrelating transform
//! nearest the identity, so it leaves each layer in its own channel, dark
//! modules still dark, without knowing the camera.

use std::array::from_fn;

use crate::receive::{camera::LumaFrame, tracker::Crop};

/// Smallest share of an area's colour variance on its weakest axis for it to
/// count as colour. Black and white through a camera stays well under 1%; a
/// colour code's three independent layers give tens of percent.
pub const COLOR_SPREAD: f64 = 0.04;
/// Grey levels per standard deviation of an unmixed layer: its dark and light
/// modules land near 68 and 188.
const GAIN: f64 = 60.0;
/// Enough samples to pin a 3×3 covariance down.
const SAMPLES: usize = 40_000;
/// Enough to tell colour from black and white, whose spreads differ tenfold.
const PROBE_SAMPLES: usize = 2_500;

/// Per layer, grey-level weights on camera RGB and an offset.
#[derive(Clone, Debug)]
pub struct Unmixing {
    rows: [[f32; 3]; 3],
    offsets: [f32; 3],
}

/// Eigenvalues and eigenvectors (the columns of the matrix) of a symmetric
/// 3×3 matrix, by cyclic Jacobi rotations.
pub fn eigen3(matrix: [[f64; 3]; 3]) -> ([f64; 3], [[f64; 3]; 3]) {
    let mut a = matrix;
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..16 {
        let off = a[0][1].abs() + a[0][2].abs() + a[1][2].abs();
        if off <= 1e-12 * (a[0][0].abs() + a[1][1].abs() + a[2][2].abs()) {
            break;
        }
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            if a[p][q] == 0.0 {
                continue;
            }
            // The rotation in the (p, q) plane that zeroes a[p][q]: A ← RᵀAR, V ← VR.
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            for m in [&mut a, &mut v] {
                for row in m.iter_mut() {
                    let (kp, kq) = (row[p], row[q]);
                    row[p] = c * kp - s * kq;
                    row[q] = s * kp + c * kq;
                }
            }
            let (row_p, row_q) = (a[p], a[q]);
            a[p] = from_fn(|k| c * row_p[k] - s * row_q[k]);
            a[q] = from_fn(|k| s * row_p[k] + c * row_q[k]);
        }
    }
    ([a[0][0], a[1][1], a[2][2]], v)
}

/// A frame's colour at any luma pixel: the luma there, and chroma bilinear
/// between the centres of the 2×2 blocks it was measured over, read as
/// BT.709 full range, which is what macOS cameras deliver for HD video.
struct Colour<'a> {
    frame: &'a LumaFrame,
    chroma_width: usize,
    chroma_height: usize,
}

impl<'a> Colour<'a> {
    /// `None` for a frame that came without chroma.
    fn of(frame: &'a LumaFrame) -> Option<Self> {
        let chroma_width = frame.width.div_ceil(2);
        let chroma_height = frame.height.div_ceil(2);
        (frame.chroma.len() >= chroma_width * chroma_height * 2).then_some(Self {
            frame,
            chroma_width,
            chroma_height,
        })
    }

    /// The two chroma samples around luma position `at` along an axis of
    /// `count` samples, and the weight of the second.
    fn axis(at: usize, count: usize) -> (usize, usize, f32) {
        let position = (at as f32 + 0.5) / 2.0 - 0.5;
        if position <= 0.0 {
            return (0, 0, 0.0);
        }
        let low = (position as usize).min(count - 1);
        (low, (low + 1).min(count - 1), position - low as f32)
    }

    fn at(&self, x: usize, y: usize) -> [f32; 3] {
        let (x0, x1, tx) = Self::axis(x, self.chroma_width);
        let (y0, y1, ty) = Self::axis(y, self.chroma_height);
        let sample = |cx: usize, cy: usize, k: usize| {
            f32::from(self.frame.chroma[(cy * self.chroma_width + cx) * 2 + k])
        };
        let chroma = |k: usize| {
            let top = sample(x0, y0, k) * (1.0 - tx) + sample(x1, y0, k) * tx;
            let bottom = sample(x0, y1, k) * (1.0 - tx) + sample(x1, y1, k) * tx;
            top * (1.0 - ty) + bottom * ty - 128.0
        };
        let (cb, cr) = (chroma(0), chroma(1));
        let luma = f32::from(self.frame.pixels[y * self.frame.width + x]);
        [
            luma + 1.5748 * cr,
            luma - 0.1873 * cb - 0.4681 * cr,
            luma + 1.8556 * cb,
        ]
    }
}

/// The colours of `area` from about `samples` pixels on a grid: their mean,
/// and their covariance's eigenvalues and eigenvectors.
fn statistics(colour: &Colour, area: Crop, samples: usize) -> ([f64; 3], [f64; 3], [[f64; 3]; 3]) {
    let step = ((area.w * area.h / samples) as f64).sqrt().max(1.0) as usize;
    let mut count = 0.0;
    let mut sum = [0.0_f64; 3];
    let mut products = [[0.0_f64; 3]; 3];
    for y in (area.y..area.y + area.h).step_by(step) {
        for x in (area.x..area.x + area.w).step_by(step) {
            let rgb = colour.at(x, y).map(f64::from);
            for i in 0..3 {
                sum[i] += rgb[i];
                for j in 0..3 {
                    products[i][j] += rgb[i] * rgb[j];
                }
            }
            count += 1.0;
        }
    }
    let mean = sum.map(|total| total / count);
    let covariance = from_fn(|i| from_fn(|j| products[i][j] / count - mean[i] * mean[j]));
    let (values, vectors) = eigen3(covariance);
    (mean, values, vectors)
}

/// Whether colours spread over every axis by at least `spread` of the widest.
fn colourful(values: [f64; 3], spread: f64) -> bool {
    let largest = values.iter().copied().fold(f64::MIN, f64::max);
    let smallest = values.iter().copied().fold(f64::MAX, f64::min);
    largest > 0.0 && smallest >= largest * spread
}

/// The unmixing for the code inside `area`, or `None` when, to the camera,
/// the area is black and white, or the frame came without colour.
pub fn unmixing(frame: &LumaFrame, area: Crop) -> Option<Unmixing> {
    let colour = Colour::of(frame)?;
    let right = (area.x + area.w).min(frame.width);
    let bottom = (area.y + area.h).min(frame.height);
    if right < area.x + 8 || bottom < area.y + 8 {
        return None;
    }
    let area = Crop {
        w: right - area.x,
        h: bottom - area.y,
        ..area
    };
    // Most areas checked are black and white. A coarse look turns them away at
    // a sixteenth of the cost, so only colour pays for the full estimate.
    let (_, coarse, _) = statistics(&colour, area, PROBE_SAMPLES);
    if !colourful(coarse, COLOR_SPREAD / 2.0) {
        return None;
    }
    let (mean, values, vectors) = statistics(&colour, area, SAMPLES);
    if !colourful(values, COLOR_SPREAD) {
        return None;
    }
    // W = V Λ^-½ Vᵀ, in grey levels, with the mean folded into each offset.
    let rows: [[f64; 3]; 3] = from_fn(|i| {
        from_fn(|j| {
            GAIN * (0..3)
                .map(|k| vectors[i][k] * vectors[j][k] / values[k].sqrt())
                .sum::<f64>()
        })
    });
    let offsets: [f64; 3] = from_fn(|i| 128.0 - (0..3).map(|j| rows[i][j] * mean[j]).sum::<f64>());
    Some(Unmixing {
        rows: rows.map(|row| row.map(|weight| weight as f32)),
        offsets: offsets.map(|offset| offset as f32),
    })
}

/// The three unmixed layers of `crop`, each a luma picture of its size.
/// Empty for a frame without chroma, which [`unmixing`] already refused.
pub fn layers(frame: &LumaFrame, crop: Crop, unmix: &Unmixing) -> [Vec<u8>; 3] {
    let mut layers: [Vec<u8>; 3] = from_fn(|_| Vec::with_capacity(crop.w * crop.h));
    let Some(colour) = Colour::of(frame) else {
        return layers;
    };
    for y in crop.y..crop.y + crop.h {
        for x in crop.x..crop.x + crop.w {
            let [r, g, b] = colour.at(x, y);
            for (channel, layer) in layers.iter_mut().enumerate() {
                let [kr, kg, kb] = unmix.rows[channel];
                let level = unmix.offsets[channel] + kr * r + kg * g + kb * b;
                layer.push(level.clamp(0.0, 255.0) as u8);
            }
        }
    }
    layers
}

/// Camera RGB to the frame layout a camera delivers: luma, and chroma
/// averaged over 2×2 blocks (4:2:0), BT.709 full range. For synthetic
/// cameras; the inverse of what [`unmixing`] reads.
pub fn to_420(width: usize, height: usize, rgb: &[[f32; 3]]) -> LumaFrame {
    let ycc = |[r, g, b]: [f32; 3]| {
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        [y, (b - y) / 1.8556 + 128.0, (r - y) / 1.5748 + 128.0]
    };
    let pixels = rgb
        .iter()
        .map(|&colour| ycc(colour)[0].clamp(0.0, 255.0) as u8)
        .collect();
    let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
    let mut chroma = Vec::with_capacity(chroma_width * chroma_height * 2);
    for cy in 0..chroma_height {
        for cx in 0..chroma_width {
            let mut block = [0.0_f32; 2];
            let mut count = 0.0;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (x, y) = (2 * cx + dx, 2 * cy + dy);
                if x < width && y < height {
                    let [_, cb, cr] = ycc(rgb[y * width + x]);
                    block[0] += cb;
                    block[1] += cr;
                    count += 1.0;
                }
            }
            chroma.extend(block.map(|total| (total / count).clamp(0.0, 255.0) as u8));
        }
    }
    LumaFrame {
        width,
        height,
        pixels,
        chroma,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift, so every run sees the same picture.
    fn random(mut state: u32) -> impl FnMut() -> f32 {
        move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as f32 / u32::MAX as f32
        }
    }

    #[test]
    fn eigen3_diagonalises_into_orthonormal_eigenvectors() {
        let matrix = [[4.0, 1.0, 0.5], [1.0, 3.0, 0.25], [0.5, 0.25, 2.0]];
        let (values, vectors) = eigen3(matrix);
        for i in 0..3 {
            for j in 0..3 {
                let rebuilt: f64 = (0..3)
                    .map(|k| vectors[i][k] * values[k] * vectors[j][k])
                    .sum();
                assert!((rebuilt - matrix[i][j]).abs() < 1e-9, "V Λ Vᵀ at {i},{j}");
                let dot: f64 = (0..3).map(|k| vectors[k][i] * vectors[k][j]).sum();
                assert!((dot - f64::from(u8::from(i == j))).abs() < 1e-9);
            }
        }
    }

    /// Three random layers (true is dark) as a camera that mixes channels,
    /// squeezes contrast and adds noise films them, through 4:2:0.
    fn filmed(side: usize, layers: &[Vec<bool>; 3], mix: [[f32; 3]; 3]) -> LumaFrame {
        let mut next = random(11);
        // Modules of 4×4 pixels: chroma at half resolution still resolves them.
        let rgb: Vec<[f32; 3]> = (0..side * side)
            .map(|index| {
                let module = (index / side / 4) * (side / 4) + (index % side) / 4;
                let light = layers
                    .each_ref()
                    .map(|layer| f32::from(u8::from(!layer[module])));
                from_fn(|channel| {
                    let mixed: f32 = (0..3)
                        .map(|source| mix[channel][source] * light[source])
                        .sum();
                    40.0 + 175.0 * mixed + (next() - 0.5) * 8.0
                })
            })
            .collect();
        to_420(side, side, &rgb)
    }

    #[test]
    fn black_and_white_has_no_colour_to_unmix() {
        let mut next = random(7);
        let layer: Vec<bool> = (0..32 * 32).map(|_| next() < 0.5).collect();
        let frame = filmed(
            128,
            &[layer.clone(), layer.clone(), layer],
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        );
        let whole = Crop {
            x: 0,
            y: 0,
            w: 128,
            h: 128,
        };
        assert!(unmixing(&frame, whole).is_none());
        let grey = LumaFrame {
            chroma: Vec::new(),
            ..frame
        };
        assert!(unmixing(&grey, whole).is_none(), "no chroma, no colour");
    }

    #[test]
    fn unmixing_undoes_crosstalk_through_4_2_0_and_keeps_polarity() {
        let mut next = random(5);
        let sources: [Vec<bool>; 3] = from_fn(|_| (0..48 * 48).map(|_| next() < 0.5).collect());
        // Rows are camera channels, columns the layers they pick up.
        let mix = [[0.78, 0.18, 0.04], [0.14, 0.7, 0.16], [0.03, 0.27, 0.7]];
        let side = 192;
        let frame = filmed(side, &sources, mix);
        let whole = Crop {
            x: 0,
            y: 0,
            w: side,
            h: side,
        };
        let unmix = unmixing(&frame, whole).expect("three independent layers are colour");
        for (channel, picture) in layers(&frame, whole, &unmix).iter().enumerate() {
            // Each module's centre reads as its own layer's.
            let wrong = (0..48 * 48)
                .filter(|&module| {
                    let (mx, my) = (module % 48, module / 48);
                    let centre = (my * 4 + 2) * side + mx * 4 + 2;
                    (picture[centre] < 128) != sources[channel][module]
                })
                .count();
            assert!(wrong <= 4, "layer {channel}: {wrong} modules misread");
        }
    }
}
