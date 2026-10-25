#!/usr/bin/env node
// Colour codes through a software camera, offline and deterministic: frames
// from the real Rust sender, filmed with the colour a phone actually keeps,
// then decoded by the real web decode worker under the real region tracker.
//
//   ./scripts/build-web.sh
//   node scripts/colour-camera.mjs [key=value ...]
//
//   bytes=1465   bytes per code (the sender's option; 1465 is version 27)
//   color=1      0 for a black and white code, to compare
//   side=629     the code's side in camera pixels (default: 3.4 px/module at v40)
//   blur=1.0     lens blur, σ in pixels, on every channel
//   cblur=1.5    colour smear on top of 4:2:0, σ in pixels: a phone's colour
//                noise reduction, which loopback.html does not model
//   xtalk=0.2    crosstalk between channels; noise=4 grey levels; tilt=2°
//   frames=24    camera frames, one new code each; camera=1280x720
//
// Prints one JSON line: the QR version, pixels per module, codes read per
// frame (3 is every layer), reads per layer (red, green, blue), and the
// worker's milliseconds per full scan and per crop. Time is simulated, so
// the numbers do not depend on how fast this machine films.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const app = join(root, "web", "app");
const options = Object.fromEntries(process.argv.slice(2).map((pair) => {
  const [key, value] = pair.split("=");
  return [key, Number.isNaN(Number(value)) ? value : Number(value)];
}));
const config = {
  bytes: 1465, color: 1, side: 629, blur: 1.0, cblur: 1.5, xtalk: 0.2, noise: 4, tilt: 2,
  frames: 24, camera: "1280x720", seed: 1, ...options,
};
const [width, height] = config.camera.split("x").map(Number);

// The worker is a module worker; here it is a module with a `self`. The
// Emscripten glue fetches its .wasm, and Node's fetch does not read files.
const fetchUrl = globalThis.fetch;
globalThis.fetch = async (url, init) => String(url).startsWith("file:")
  ? new Response(readFileSync(new URL(String(url))), { headers: { "Content-Type": "application/wasm" } })
  : fetchUrl(url, init);
const replies = [];
globalThis.self = { postMessage: (message) => replies.push(message) };
const { initSync, WasmSender } = await import(pathToFileURL(join(app, "pkg", "optical_wasm.js")).href);
initSync({ module: readFileSync(join(app, "pkg", "optical_wasm_bg.wasm")) });
await import(pathToFileURL(join(app, "decode-worker.js")).href);
const { RegionTracker } = await import(pathToFileURL(join(app, "capture.js")).href);
while (!replies.some((reply) => reply.id === -1)) await new Promise((resolve) => setTimeout(resolve, 10));

let state = config.seed >>> 0 || 1;
const random = () => {
  state ^= state << 13; state ^= state >>> 17; state ^= state << 5;
  return (state >>> 0) / 2 ** 32;
};

const sender = new WasmSender();
const text = Array.from({ length: 400_000 }, () => String.fromCharCode(33 + Math.floor(random() * 90))).join("");
const info = sender.prepareText(text, config.bytes, 60, "L", 7, 4, config.color === 1);
const size = info.qrSize;
const pitch = config.side / size;

/** Each channel keeps 1 − x of itself and picks x up from the other two. */
const x = config.xtalk;
const mixing = [[1 - x, x * 0.75, x * 0.25], [x * 0.45, 1 - x, x * 0.55], [x * 0.15, x * 0.85, 1 - x]];

function gaussian(sigma) {
  if (sigma <= 0) return [1];
  const radius = Math.ceil(sigma * 3);
  const kernel = Array.from({ length: 2 * radius + 1 }, (_, i) => Math.exp(-((i - radius) ** 2) / (2 * sigma * sigma)));
  const total = kernel.reduce((a, b) => a + b, 0);
  return kernel.map((value) => value / total);
}

/** `plane` (width × height) blurred in place by a separable `kernel`. */
function blurred(plane, kernel, scratch) {
  const radius = (kernel.length - 1) / 2;
  if (!radius) return;
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      let sum = 0;
      for (let k = -radius; k <= radius; k++) sum += kernel[k + radius] * plane[y * width + Math.min(width - 1, Math.max(0, x + k))];
      scratch[y * width + x] = sum;
    }
  }
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      let sum = 0;
      for (let k = -radius; k <= radius; k++) sum += kernel[k + radius] * scratch[Math.min(height - 1, Math.max(0, y + k)) * width + x];
      plane[y * width + x] = sum;
    }
  }
}

const lens = gaussian(config.blur);
const smear = gaussian(config.cblur);
const scratch = new Float32Array(width * height);

/** One camera frame of `modules` (bit c set where channel c is dark), as RGBA. */
function film(modules, shiftX, shiftY) {
  const angle = (config.tilt * Math.PI) / 180;
  const cos = Math.cos(angle);
  const sin = Math.sin(angle);
  const planes = [0, 1, 2].map(() => new Float32Array(width * height));
  const SUB = 3;
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const light = [0, 0, 0];
      for (let sy = 0; sy < SUB; sy++) {
        for (let sx = 0; sx < SUB; sx++) {
          const px = x + (sx + 0.5) / SUB - width / 2 - shiftX;
          const py = y + (sy + 0.5) / SUB - height / 2 - shiftY;
          const u = (cos * px + sin * py) / pitch + size / 2;
          const v = (-sin * px + cos * py) / pitch + size / 2;
          // The code on a white border on a grey desk.
          const code = u >= 0 && v >= 0 && u < size && v < size;
          const border = u >= -4 && v >= -4 && u < size + 4 && v < size + 4;
          const dark = code ? modules[Math.floor(v) * size + Math.floor(u)] : 0;
          for (let c = 0; c < 3; c++) light[c] += border ? (dark & (1 << c) ? 0 : 1) : 0.43;
        }
      }
      for (let c = 0; c < 3; c++) planes[c][y * width + x] = light[c] / (SUB * SUB);
    }
  }
  for (const plane of planes) blurred(plane, lens, scratch);
  // Crosstalk and exposure into BT.601 luma and chroma; chroma kept as each
  // 2×2 block's mean, bilinearly upsampled, then smeared further.
  const luma = new Float32Array(width * height);
  const cb = new Float32Array(width * height);
  const cr = new Float32Array(width * height);
  for (let i = 0; i < width * height; i++) {
    const [r, g, b] = mixing.map((row) => 35 + 185 * (row[0] * planes[0][i] + row[1] * planes[1][i] + row[2] * planes[2][i]));
    luma[i] = 0.299 * r + 0.587 * g + 0.114 * b;
    cb[i] = (b - luma[i]) * 0.564;
    cr[i] = (r - luma[i]) * 0.713;
  }
  const bw = width / 2;
  const bh = height / 2;
  for (const plane of [cb, cr]) {
    const blocks = new Float32Array(bw * bh);
    for (let by = 0; by < bh; by++) {
      for (let bx = 0; bx < bw; bx++) {
        const i = 2 * by * width + 2 * bx;
        blocks[by * bw + bx] = (plane[i] + plane[i + 1] + plane[i + width] + plane[i + width + 1]) / 4;
      }
    }
    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width; x++) {
        const fx = Math.min(bw - 1, Math.max(0, (x - 0.5) / 2));
        const fy = Math.min(bh - 1, Math.max(0, (y - 0.5) / 2));
        const x0 = Math.floor(fx);
        const y0 = Math.floor(fy);
        const x1 = Math.min(bw - 1, x0 + 1);
        const y1 = Math.min(bh - 1, y0 + 1);
        const tx = fx - x0;
        const ty = fy - y0;
        const top = blocks[y0 * bw + x0] * (1 - tx) + blocks[y0 * bw + x1] * tx;
        const bottom = blocks[y1 * bw + x0] * (1 - tx) + blocks[y1 * bw + x1] * tx;
        plane[y * width + x] = top * (1 - ty) + bottom * ty;
      }
    }
    blurred(plane, smear, scratch);
  }
  const out = new Uint8ClampedArray(width * height * 4);
  const grain = () => (random() - 0.5) * 2 * config.noise;
  for (let i = 0; i < width * height; i++) {
    const r = luma[i] + cr[i] / 0.713;
    const b = luma[i] + cb[i] / 0.564;
    const g = (luma[i] - 0.299 * r - 0.114 * b) / 0.587;
    out[i * 4] = r + grain();
    out[i * 4 + 1] = g + grain();
    out[i * 4 + 2] = b + grain();
    out[i * 4 + 3] = 255;
  }
  return out;
}

async function decode(message) {
  replies.length = 0;
  await self.onmessage({ data: message });
  return replies[0];
}

const tracker = new RegionTracker();
const seen = new Set();
const layers = [0, 0, 0];
const time = { full: 0, fulls: 0, crop: 0, crops: 0 };
let id = 0;
let now = 1000;
for (let frame = 0; frame < config.frames; frame++, now += 1000 / 60) {
  const picture = film(sender.nextFrames(1), (random() - 0.5) * 6, (random() - 0.5) * 6);
  // Four workers' worth of jobs per frame, as a phone's pool offers.
  const plan = tracker.plan(now, 4, width, height);
  const jobs = [...(plan.full ? [{ x: 0, y: 0, w: width, h: height, full: true }] : []),
    ...plan.crops.map((crop) => ({ ...crop, full: false }))];
  for (const job of jobs) {
    const pixels = new Uint8ClampedArray(job.w * job.h * 4);
    for (let row = 0; row < job.h; row++) {
      const start = ((job.y + row) * width + job.x) * 4;
      pixels.set(picture.subarray(start, start + job.w * 4), row * job.w * 4);
    }
    const reply = await decode({
      id: ++id, buffer: pixels.buffer, width: job.w, height: job.h, offsetX: job.x, offsetY: job.y,
      full: job.full, quad: job.quad, modules: job.modules, box: job.box,
    });
    time[job.full ? "full" : "crop"] += reply.decodeMs;
    time[job.full ? "fulls" : "crops"]++;
    for (const box of reply.sightings ?? []) tracker.noteSighting(box, now);
    for (const symbol of reply.symbols ?? []) {
      tracker.noteDecoded(symbol.box, now, symbol.quad, symbol.modules);
      const key = Array.from(symbol.bytes.slice(0, 16)).join();
      if (!seen.has(key)) {
        seen.add(key);
        if (symbol.color) layers[symbol.layer]++;
      }
    }
  }
}

const round = (value, places = 1) => Number(value.toFixed(places));
console.log(JSON.stringify({
  version: info.qrVersion,
  pxPerModule: round(pitch, 2),
  color: config.color === 1, blur: config.blur, cblur: config.cblur,
  readsPerFrame: round(seen.size / config.frames, 2),
  layers,
  bytesPerFrame: Math.round((seen.size / config.frames) * (config.bytes - 16)),
  fullMs: time.fulls ? round(time.full / time.fulls) : null,
  cropMs: time.crops ? round(time.crop / time.crops) : null,
}));
