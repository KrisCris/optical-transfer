// QR decode worker: the pinned decimen-codec engine, a QR-only reader-only
// zxing-cpp build compiled to WebAssembly.
//
// WASM rather than the platform's own decoder because there is no portable
// platform decoder: Safari has never shipped BarcodeDetector, and Safari is
// half the phones this has to run on.
//
// One frame in flight per worker. The main thread drops frames when every
// worker is busy rather than queueing them — a queued camera frame is a stale
// camera frame, and the fountain does not care which frames it misses.

import DecimenCodec from "./vendor/decimen-codec/decimen_codec.js";

const ready = DecimenCodec({
  // Emscripten resolves the .wasm relative to the glue by default, which is
  // already correct here; naming it explicitly keeps that true if the glue
  // ever moves.
  locateFile: (path, prefix) =>
    path.endsWith(".wasm") ? new URL("./vendor/decimen-codec/decimen_codec.wasm", import.meta.url).href : prefix + path,
});

/** Axis-aligned bounds of a symbol quad, in capture coordinates. */
function boundsOf(quad, offsetX, offsetY) {
  const xs = [quad.topLeft.x, quad.topRight.x, quad.bottomRight.x, quad.bottomLeft.x];
  const ys = [quad.topLeft.y, quad.topRight.y, quad.bottomRight.y, quad.bottomLeft.y];
  const x = Math.min(...xs);
  const y = Math.min(...ys);
  return { x: offsetX + x, y: offsetY + y, w: Math.max(...xs) - x, h: Math.max(...ys) - y };
}

function shifted(quad, offsetX, offsetY) {
  const move = (point) => ({ x: point.x + offsetX, y: point.y + offsetY });
  return {
    topLeft: move(quad.topLeft),
    topRight: move(quad.topRight),
    bottomRight: move(quad.bottomRight),
    bottomLeft: move(quad.bottomLeft),
  };
}

self.onmessage = async (event) => {
  const {
    id,
    buffer,
    width = 0,
    height = 0,
    offsetX = 0,
    offsetY = 0,
    full = true,
    quad,
    modules,
  } = event.data;

  let codec;
  try {
    codec = await ready;
  } catch (error) {
    self.postMessage({ id, symbols: [], sightings: [], fatal: String(error) });
    return;
  }

  const pixels = new Uint8Array(buffer);
  const pointer = codec._malloc(width * height * 4);
  try {
    codec.HEAPU8.set(pixels, pointer);
    const symbols = [];
    const sightings = [];

    // Tracked path: a crop that arrives with a cached quad and module count
    // skips detection entirely — the transform is rebuilt from the quad and
    // the grid sampled directly. It is opportunistic and never load-bearing:
    // any miss falls through to a full read on the same buffer, which also
    // re-anchors the quad.
    let trackedHit = false;
    if (!full && quad && modules) {
      const result = codec.readTracked(
        pointer, width, height, modules,
        quad.topLeft.x - offsetX, quad.topLeft.y - offsetY,
        quad.topRight.x - offsetX, quad.topRight.y - offsetY,
        quad.bottomRight.x - offsetX, quad.bottomRight.y - offsetY,
        quad.bottomLeft.x - offsetX, quad.bottomLeft.y - offsetY,
      );
      if (result.valid && result.bytes.length > 0) {
        symbols.push({
          bytes: result.bytes,
          box: boundsOf(result.position, offsetX, offsetY),
          quad: shifted(result.position, offsetX, offsetY),
          modules: result.modules,
          tracked: true,
        });
        trackedHit = true;
      }
    }

    if (!trackedHit) {
      // tryHarder stays on everywhere: marginal captures are where it earns
      // its cost. Full scans ask for error results too, because a symbol the
      // decoder DETECTED but could not decode is still a fix on where a code
      // sits — and a crop aimed there often decodes where the full frame did
      // not. Error results count against the symbol cap, hence the headroom.
      const results = codec.readFull(pointer, width, height, true, full ? 12 : 2, full);
      for (let index = 0; index < results.size(); index++) {
        const result = results.get(index);
        if (result.valid && result.bytes.length > 0) {
          symbols.push({
            bytes: result.bytes,
            box: boundsOf(result.position, offsetX, offsetY),
            quad: shifted(result.position, offsetX, offsetY),
            modules: result.modules,
            tracked: false,
          });
        } else if (full) {
          const box = boundsOf(result.position, offsetX, offsetY);
          if (box.w > 0 && box.h > 0) sightings.push(box);
        }
      }
      results.delete();
    }

    self.postMessage({ id, symbols, sightings });
  } catch {
    self.postMessage({ id, symbols: [], sightings: [] });
  } finally {
    // Every path frees. An embind vector leak here is a phone that gets slower
    // the longer you point it at something.
    codec._free(pointer);
  }
};

// Warm the module — instantiation plus first-call JIT — so the first real
// camera frame does not pay for it. The pool ignores this reply's id.
(async () => {
  try {
    const codec = await ready;
    const pointer = codec._malloc(8 * 8 * 4);
    codec.HEAPU8.set(new Uint8Array(8 * 8 * 4).fill(255), pointer);
    codec.readFull(pointer, 8, 8, false, 1, false).delete();
    codec._free(pointer);
    self.postMessage({ id: -1, ready: true, version: codec.version(), build: codec.build() });
  } catch (error) {
    // A failed warm-up is a slow first frame, not a broken receiver — but a
    // failed *load* is fatal and the UI has to know.
    self.postMessage({ id: -1, ready: false, fatal: String(error) });
  }
})();
