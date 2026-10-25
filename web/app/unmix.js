// Colour codes, receive side: separating the three layers a camera mixed.
//
// A colour code (crates/optical-core/src/qr.rs, `compose`) puts one QR code in
// each of the red, green and blue channels. A camera does not keep them apart:
// its filters overlap, its colour correction mixes them, and 4:2:0 video keeps
// colour at half resolution. The layers are independent, though, so their
// mixture can be undone blind, from the picture's own colour statistics.
// Pure functions, no DOM, so the maths is testable without a camera.

/** Smallest share of an area's colour variance on its weakest axis for it to
 *  count as colour. Black and white through a camera stays well under 1%; a
 *  colour code's three independent layers give tens of percent. */
const COLOR_SPREAD = 0.04;
/** Grey levels per standard deviation of an unmixed channel: a layer's dark
 *  and light modules land near 68 and 188. */
const UNMIXED_GAIN = 60;
/** A few tens of thousands of samples pin a 3×3 covariance down. */
const SAMPLES = 40_000;
/** Enough to tell colour from black and white, whose spreads differ tenfold. */
const PROBE_SAMPLES = 2_500;

/** Eigenvalues and eigenvectors (the columns of `vectors`) of a symmetric
 *  3×3 matrix, by cyclic Jacobi rotations. */
export function eigen3(matrix) {
  const a = matrix.map((row) => [...row]);
  const v = [[1, 0, 0], [0, 1, 0], [0, 0, 1]];
  for (let sweep = 0; sweep < 16; sweep++) {
    const off = Math.abs(a[0][1]) + Math.abs(a[0][2]) + Math.abs(a[1][2]);
    if (off <= 1e-12 * (Math.abs(a[0][0]) + Math.abs(a[1][1]) + Math.abs(a[2][2]))) break;
    for (const [p, q] of [[0, 1], [0, 2], [1, 2]]) {
      if (a[p][q] === 0) continue;
      // The rotation in the (p, q) plane that zeroes a[p][q]: A ← RᵀAR, V ← VR.
      const theta = (a[q][q] - a[p][p]) / (2 * a[p][q]);
      const t = (theta < 0 ? -1 : 1) / (Math.abs(theta) + Math.sqrt(theta * theta + 1));
      const c = 1 / Math.sqrt(t * t + 1);
      const s = t * c;
      for (const m of [a, v]) {
        for (let k = 0; k < 3; k++) {
          const kp = m[k][p];
          const kq = m[k][q];
          m[k][p] = c * kp - s * kq;
          m[k][q] = s * kp + c * kq;
        }
      }
      for (let k = 0; k < 3; k++) {
        const pk = a[p][k];
        const qk = a[q][k];
        a[p][k] = c * pk - s * qk;
        a[q][k] = s * pk + c * qk;
      }
    }
  }
  return { values: [a[0][0], a[1][1], a[2][2]], vectors: v };
}

/**
 * The colours of `x0..x1 × y0..y1` from about `samples` pixels on a grid:
 * their mean, and their covariance's eigenvalues and eigenvectors.
 */
function statistics(pixels, width, x0, y0, x1, y1, samples) {
  const step = Math.max(1, Math.floor(Math.sqrt(((x1 - x0) * (y1 - y0)) / samples)));
  let n = 0;
  let r = 0, g = 0, b = 0, rr = 0, gg = 0, bb = 0, rg = 0, rb = 0, gb = 0;
  for (let y = y0; y < y1; y += step) {
    for (let x = x0, i = (y * width + x0) * 4; x < x1; x += step, i += 4 * step) {
      const red = pixels[i];
      const green = pixels[i + 1];
      const blue = pixels[i + 2];
      r += red; g += green; b += blue;
      rr += red * red; gg += green * green; bb += blue * blue;
      rg += red * green; rb += red * blue; gb += green * blue;
      n++;
    }
  }
  const mean = [r / n, g / n, b / n];
  const covariance = [
    [rr / n - mean[0] * mean[0], rg / n - mean[0] * mean[1], rb / n - mean[0] * mean[2]],
    [rg / n - mean[0] * mean[1], gg / n - mean[1] * mean[1], gb / n - mean[1] * mean[2]],
    [rb / n - mean[0] * mean[2], gb / n - mean[1] * mean[2], bb / n - mean[2] * mean[2]],
  ];
  return { mean, ...eigen3(covariance) };
}

/** Whether colours spread over every axis by at least `spread` of the widest. */
function colourful(values, spread) {
  const largest = Math.max(...values);
  return largest > 0 && Math.min(...values) >= largest * spread;
}

/**
 * Blind unmixing for a colour code: the symmetric whitening (ZCA) of the
 * colours inside `area`, as three rows of grey-level weights and offsets.
 * ZCA is the decorrelating transform nearest the identity, so it undoes a
 * camera's channel crosstalk without knowing it and leaves each layer in its
 * own channel with dark modules still dark. `null` when the area is, to a
 * camera, black and white.
 */
export function unmixing(pixels, width, area) {
  const x0 = Math.max(0, Math.floor(area.x));
  const y0 = Math.max(0, Math.floor(area.y));
  const x1 = Math.min(width, Math.ceil(area.x + area.w));
  const y1 = Math.min(pixels.length / 4 / width, Math.ceil(area.y + area.h));
  if (x1 - x0 < 8 || y1 - y0 < 8) return null;
  // Most areas checked are black and white. A coarse look turns them away at
  // a sixteenth of the cost, so only colour pays for the full estimate.
  if (!colourful(statistics(pixels, width, x0, y0, x1, y1, PROBE_SAMPLES).values, COLOR_SPREAD / 2)) return null;
  const { mean, values, vectors } = statistics(pixels, width, x0, y0, x1, y1, SAMPLES);
  if (!colourful(values, COLOR_SPREAD)) return null;
  // W = V Λ^-½ Vᵀ, in grey levels, with the mean folded into each offset.
  const rows = [0, 1, 2].map((i) => [0, 1, 2].map((j) =>
    UNMIXED_GAIN * values.reduce((sum, value, k) => sum + (vectors[i][k] * vectors[j][k]) / Math.sqrt(value), 0)));
  const offsets = rows.map((row) => 128 - row[0] * mean[0] - row[1] * mean[1] - row[2] * mean[2]);
  return { rows, offsets };
}

/** One unmixed channel of `rect` (picture coordinates) as grey RGBA pixels
 *  into `out`, a Uint32Array: one write per pixel, clamped by hand. Little
 *  endian, as every platform with WebAssembly is: red is the low byte. */
export function writeLayer(out, pixels, width, rect, unmix, channel) {
  const [kr, kg, kb] = unmix.rows[channel];
  const offset = unmix.offsets[channel];
  let o = 0;
  for (let y = rect.y; y < rect.y + rect.h; y++) {
    for (let i = (y * width + rect.x) * 4, end = i + rect.w * 4; i < end; i += 4) {
      const level = offset + kr * pixels[i] + kg * pixels[i + 1] + kb * pixels[i + 2];
      const grey = level <= 0 ? 0 : level >= 255 ? 255 : level | 0;
      out[o++] = 0xff000000 | (grey << 16) | (grey << 8) | grey;
    }
  }
}
