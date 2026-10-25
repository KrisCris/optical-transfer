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
//
// A colour code (crates/optical-core/src/qr.rs, `compose`) is three QR codes
// in the red, green and blue channels sharing one geometry. Read for
// brightness it is found and does not decode, or decodes as the layer luma
// favours, so wherever the code's area has colour in it the channels are
// unmixed and each is read on its own. Black and white codes have none, and a
// coarse look at a few thousand pixels per crop, about 0.02 ms, says so.

import DecimenCodec from "./vendor/decimen-codec/decimen_codec.js";
import { sharpenLayer, unmixing, writeLayer } from "./unmix.js";

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

/** Codes a full scan checks for colour. */
const COLOR_SIGHTINGS = 4;

/** Green first: it carries most of luma, so it survives a camera's coarse
 *  colour best, and where it reads, the other two layers are read there. */
const LAYER_ORDER = [1, 0, 2];

/** Pixels per module of a code at `quad` with `modules` across. */
function pitchOf(quad, modules) {
  const length = (a, b) => Math.hypot(b.x - a.x, b.y - a.y);
  const sides = length(quad.topLeft, quad.topRight) + length(quad.bottomLeft, quad.bottomRight)
    + length(quad.topLeft, quad.bottomLeft) + length(quad.topRight, quad.bottomRight);
  return sides / (4 * modules);
}

/** A tracked read of the layer in `at` (`rect` of the picture) at `anchor`,
 *  picture coordinates; the result when it decoded, else null. */
function readAt(codec, at, rect, anchor) {
  const q = anchor.quad;
  const result = codec.readTracked(
    at, rect.w, rect.h, anchor.modules,
    q.topLeft.x - rect.x, q.topLeft.y - rect.y,
    q.topRight.x - rect.x, q.topRight.y - rect.y,
    q.bottomRight.x - rect.x, q.bottomRight.y - rect.y,
    q.bottomLeft.x - rect.x, q.bottomLeft.y - rect.y,
  );
  return result.valid && result.bytes.length > 0 ? result : null;
}

/**
 * Decode each layer of a colour code in `rect` of the picture on its own.
 * `at` is heap space for `rect` in RGBA and is overwritten. With a `quad` and
 * `modules` (picture coordinates) each layer tries the tracked path first.
 * `originX/Y` place the picture in capture coordinates. `paths` counts
 * tracked attempts and hits for the telemetry.
 */
function readLayers(codec, at, pixels, width, rect, unmix, quad, modules, originX, originY, paths) {
  const symbols = [];
  const x = originX + rect.x;
  const y = originY + rect.y;
  // A fresh view each time: a decode that grows the heap detaches the last.
  const layer = () => new Uint32Array(codec.HEAPU8.buffer, at, rect.w * rect.h);
  let anchor = quad && modules ? { quad, modules } : null;
  // Once one layer of this picture has been read, the code's position is this
  // frame's own and a layer that still misses there is unreadable, not
  // misplaced: a full read would find the same code and fail again.
  let placed = false;
  for (const channel of LAYER_ORDER) {
    writeLayer(layer(), pixels, width, rect, unmix, channel);
    let found = null;
    if (anchor) {
      paths.trackedAttempted = true;
      found = readAt(codec, at, rect, anchor);
      // Coarse camera colour smears a layer's modules into their neighbours,
      // which sharpening at the module pitch mostly undoes. It costs about
      // what unmixing the layer did, so only a layer that missed pays for it.
      if (!found) {
        sharpenLayer(layer(), rect.w, rect.h, pitchOf(anchor.quad, anchor.modules));
        found = readAt(codec, at, rect, anchor);
      }
      if (found) paths.trackedHit = true;
    }
    const tracked = Boolean(found);
    if (!found && !placed) {
      const results = codec.readFull(at, rect.w, rect.h, true, 1, false);
      for (let index = 0; index < results.size() && !found; index++) {
        const result = results.get(index);
        if (result.valid && result.bytes.length > 0) found = result;
      }
      results.delete();
    }
    if (found) {
      symbols.push({
        bytes: found.bytes,
        box: boundsOf(found.position, x, y),
        quad: shifted(found.position, x, y),
        modules: found.modules,
        tracked,
        color: true,
        layer: channel,
      });
      if (!placed) anchor = { quad: shifted(found.position, rect.x, rect.y), modules: found.modules };
      placed = true;
    }
  }
  return symbols;
}

/** `box` grown by `fraction` of its size on every side, clamped to the picture. */
function grown(box, fraction, width, height) {
  const pad = Math.max(box.w, box.h) * fraction;
  const x = Math.max(0, Math.floor(box.x - pad));
  const y = Math.max(0, Math.floor(box.y - pad));
  return {
    x, y,
    w: Math.min(width, Math.ceil(box.x + box.w + pad)) - x,
    h: Math.min(height, Math.ceil(box.y + box.h + pad)) - y,
  };
}

const inside = (box, point) =>
  point.x >= box.x && point.x <= box.x + box.w && point.y >= box.y && point.y <= box.y + box.h;

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
    box,
  } = event.data;

  let codec;
  try {
    codec = await ready;
  } catch (error) {
    self.postMessage({ id, symbols: [], sightings: [], fatal: String(error) });
    return;
  }

  const pixels = new Uint8Array(buffer);
  const decodeStartedAt = performance.now();
  // Whether the tracked path was tried and hit, in brightness or in colour.
  const paths = { trackedAttempted: false, trackedHit: false };
  let colorAttempted = 0;
  const pointer = codec._malloc(width * height * 4);
  const picture = { x: 0, y: 0, w: width, h: height };
  // Where the code sits in this picture, for unmixing: the cached quad's
  // bounds, else the region's, else the middle of the crop.
  const local = quad && shifted(quad, -offsetX, -offsetY);
  const area = local ? boundsOf(local, 0, 0)
    : box ? { x: box.x - offsetX, y: box.y - offsetY, w: box.w, h: box.h }
      : { x: width * 0.2, y: height * 0.2, w: width * 0.6, h: height * 0.6 };
  try {
    const symbols = [];
    const sightings = [];
    // Where the scan's detector placed each sighting, for the colour path.
    const placements = new Map();

    // Colour first wherever the code has colour in it. A miss still falls
    // through to brightness on the original pixels.
    const unmix = full ? null : unmixing(pixels, width, area);
    if (unmix) {
      colorAttempted++;
      // With a quad the code's own bounds are enough, quiet zone and some
      // motion included; unmixing the whole padded crop would triple the cost.
      const rect = local ? grown(area, 0.12, width, height) : picture;
      symbols.push(...readLayers(codec, pointer, pixels, width, rect, unmix, local, modules, offsetX, offsetY, paths));
    }
    if (!symbols.length) codec.HEAPU8.set(pixels, pointer);

    // Tracked path: a crop that arrives with a cached quad and module count
    // skips detection entirely — the transform is rebuilt from the quad and
    // the grid sampled directly. It is opportunistic and never load-bearing:
    // any miss falls through to a full read on the same buffer, which also
    // re-anchors the quad.
    if (!symbols.length && !full && quad && modules) {
      paths.trackedAttempted = true;
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
        paths.trackedHit = true;
      }
    }

    if (!symbols.length) {
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
          if (box.w > 0 && box.h > 0) {
            sightings.push(box);
            placements.set(box, { quad: shifted(result.position, offsetX, offsetY), modules: result.modules });
          }
        }
      }
      results.delete();
    }

    // A full scan is how a colour code is first acquired. Read for brightness
    // it is found and not decoded, or decoded as whichever layer dominates
    // luma (green), so every code the scan found is checked for colour and,
    // where it has some, read layer by layer instead.
    if (full && (symbols.length || sightings.length)) {
      const found = [
        ...symbols.splice(0).map((symbol) => ({ box: symbol.box, symbol })),
        ...sightings.splice(0).map((box) => ({ box, symbol: null })),
      ];
      for (const { box: foundBox, symbol } of found) {
        const centre = { x: foundBox.x + foundBox.w / 2, y: foundBox.y + foundBox.h / 2 };
        if (symbols.some((layer) => layer.color && inside(layer.box, centre))) continue;
        const foundArea = { x: foundBox.x - offsetX, y: foundBox.y - offsetY, w: foundBox.w, h: foundBox.h };
        const unmix = colorAttempted < COLOR_SIGHTINGS && unmixing(pixels, width, foundArea);
        const before = symbols.length;
        if (unmix) {
          colorAttempted++;
          const rect = grown(foundArea, 0.1, width, height);
          // The detector's own fix on the code, decoded or not, is where the
          // layers are tried first, and gives the pitch to sharpen them at.
          const placement = symbol ?? placements.get(foundBox);
          const quad = placement?.modules ? shifted(placement.quad, -offsetX, -offsetY) : null;
          symbols.push(...readLayers(codec, pointer, pixels, width, rect, unmix, quad, placement?.modules ?? 0,
            offsetX, offsetY, paths));
        }
        if (symbols.length === before) {
          if (symbol) symbols.push(symbol);
          else sightings.push(foundBox);
        }
      }
    }

    self.postMessage({ id, symbols, sightings, ...paths, colorAttempted,
      decodeMs: performance.now() - decodeStartedAt });
  } catch {
    self.postMessage({ id, symbols: [], sightings: [], ...paths, colorAttempted,
      decodeMs: performance.now() - decodeStartedAt });
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
