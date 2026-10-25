import assert from "node:assert/strict";
import test from "node:test";
import { eigen3, unmixing, writeLayer } from "../unmix.js";

/** xorshift32, so every run sees the same picture. */
function random(seed) {
  let state = seed >>> 0 || 1;
  return () => {
    state ^= state << 13; state ^= state >>> 17; state ^= state << 5;
    return (state >>> 0) / 2 ** 32;
  };
}

test("eigen3 diagonalises a symmetric matrix into orthonormal eigenvectors", () => {
  const matrix = [[4, 1, 0.5], [1, 3, 0.25], [0.5, 0.25, 2]];
  const { values, vectors } = eigen3(matrix);
  for (let i = 0; i < 3; i++) {
    for (let j = 0; j < 3; j++) {
      const rebuilt = values.reduce((sum, value, k) => sum + vectors[i][k] * value * vectors[j][k], 0);
      assert.ok(Math.abs(rebuilt - matrix[i][j]) < 1e-9, `V Λ Vᵀ differs at ${i},${j}`);
      const dot = vectors.reduce((sum, row) => sum + row[i] * row[j], 0);
      assert.ok(Math.abs(dot - (i === j ? 1 : 0)) < 1e-9, "eigenvectors are orthonormal");
    }
  }
});

/** A `side`² RGBA picture of three random layers (true is dark), as a camera
 *  that mixes its channels, squeezes contrast and adds noise would film it. */
function filmed(side, layers, mix, noise, next) {
  const pixels = new Uint8ClampedArray(side * side * 4);
  for (let index = 0; index < side * side; index++) {
    const light = layers.map((layer) => (layer[index] ? 0 : 1));
    for (let channel = 0; channel < 3; channel++) {
      const mixed = mix[channel].reduce((sum, weight, source) => sum + weight * light[source], 0);
      pixels[index * 4 + channel] = 40 + 175 * mixed + (next() - 0.5) * 2 * noise;
    }
    pixels[index * 4 + 3] = 255;
  }
  return pixels;
}

test("a black and white picture has no colour to unmix", () => {
  const next = random(7);
  const side = 64;
  const layer = Array.from({ length: side * side }, () => next() < 0.5);
  const pixels = filmed(side, [layer, layer, layer], [[0.9, 0.1, 0], [0.1, 0.8, 0.1], [0, 0.2, 0.8]], 8, next);
  assert.equal(unmixing(pixels, side, { x: 0, y: 0, w: side, h: side }), null);
});

test("unmixing undoes a camera's crosstalk and keeps every layer's polarity", () => {
  const next = random(11);
  const side = 96;
  const layers = [0, 1, 2].map(() => Array.from({ length: side * side }, () => next() < 0.5));
  // Rows are camera channels, columns the layers they pick up: green leaks
  // most, as a Bayer sensor's does.
  const mix = [[0.78, 0.18, 0.04], [0.14, 0.7, 0.16], [0.03, 0.27, 0.7]];
  const pixels = filmed(side, layers, mix, 10, next);
  const unmix = unmixing(pixels, side, { x: 0, y: 0, w: side, h: side });
  assert.ok(unmix, "three independent layers are colour");
  const rect = { x: 0, y: 0, w: side, h: side };
  for (let channel = 0; channel < 3; channel++) {
    // Without unmixing, thresholding the raw channel misreads a share of the
    // modules; with it, every module reads as its own layer's.
    const out = new Uint8Array(side * side * 4);
    writeLayer(new Uint32Array(out.buffer), pixels, side, rect, unmix, channel);
    let wrong = 0;
    for (let index = 0; index < side * side; index++) {
      if ((out[index * 4] < 128) !== layers[channel][index]) wrong++;
      assert.equal(out[index * 4], out[index * 4 + 1], "the output is grey");
      assert.equal(out[index * 4 + 3], 255, "and opaque");
    }
    assert.ok(wrong / (side * side) < 0.002, `channel ${channel} misread ${wrong} modules`);
  }
});

test("statistics from the code's area unmix the whole crop around it", () => {
  const next = random(5);
  const side = 120;
  const inner = { x: 30, y: 30, w: 60, h: 60 };
  const within = (index) => {
    const x = index % side;
    const y = Math.floor(index / side);
    return x >= inner.x && x < inner.x + inner.w && y >= inner.y && y < inner.y + inner.h;
  };
  // Inside, three independent layers; outside, one pattern in all three, as
  // the black and white surround of a crop would be.
  const surround = Array.from({ length: side * side }, () => next() < 0.3);
  const layers = [0, 1, 2].map(() =>
    Array.from({ length: side * side }, (_, index) => (within(index) ? next() < 0.5 : surround[index])));
  const pixels = filmed(side, layers, [[0.85, 0.15, 0], [0.1, 0.8, 0.1], [0, 0.15, 0.85]], 6, next);
  const unmix = unmixing(pixels, side, inner);
  assert.ok(unmix, "the code's own area is colour");
  const out = new Uint8Array(side * side * 4);
  writeLayer(new Uint32Array(out.buffer), pixels, side, { x: 0, y: 0, w: side, h: side }, unmix, 2);
  let wrong = 0;
  for (let index = 0; index < side * side; index++) {
    if (within(index) && (out[index * 4] < 128) !== layers[2][index]) wrong++;
  }
  assert.ok(wrong < 8, `${wrong} modules misread`);
});
