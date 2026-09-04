// Receiver: camera → WASM QR decode in workers → the Rust fountain decoder →
// a verified file.
//
// The protocol lives entirely in `WasmReceiver`. Nothing in this file parses a
// frame, tracks a stream, or decides when a transfer is done — that would be a
// second implementation of the wire format written in JavaScript, and the two
// would drift. This file owns the camera, the worker pool, and the DOM.

import init, { WasmReceiver } from "./pkg/optical_wasm.js";

const els = {
  viewfinder: document.getElementById("viewfinder"),
  percent: document.getElementById("percent"),
  progressLabel: document.getElementById("progress-label"),
  blocksGrid: document.getElementById("blocks-grid"),
  start: document.getElementById("start"),
  stop: document.getElementById("stop"),
  video: document.getElementById("video"),
  overlay: document.getElementById("overlay"),
  status: document.getElementById("status"),
  progress: document.getElementById("progress"),
  bar: document.getElementById("bar"),
  frames: document.getElementById("m-frames"),
  blocks: document.getElementById("m-blocks"),
  decodes: document.getElementById("m-decodes"),
  result: document.getElementById("result"),
  resultName: document.getElementById("result-name"),
  resultStats: document.getElementById("result-stats"),
  resultText: document.getElementById("result-text"),
  resultSave: document.getElementById("result-save"),
  resultCopy: document.getElementById("result-copy"),
  dismiss: document.getElementById("dismiss"),
  error: document.getElementById("error"),
};

/** Bumped on every start/stop so in-flight callbacks from a previous run can
 *  tell they are stale rather than writing into the current one. */
let generation = 0;
let receiver = null;
let stream = null;
let workers = [];
let busy = [];
let capture = null;
let captureContext = null;
let objectUrl = null;
let startedAt = 0;
let decodes = 0;
let lastVerdict = null;
/** Cells currently in the block grid, so it is rebuilt only when k changes. */
let gridCells = [];

/** Regions where a code was seen recently, so the next frames can be cropped
 *  to them. A crop is both cheaper to decode and more likely to succeed than a
 *  full frame, because the symbol fills more of the buffer. */
let regions = [];
const REGION_TTL_MS = 900;
/** How often to scan the whole frame anyway, so a code that moves or a second
 *  sender arriving is not invisible until the crops expire. */
const FULL_SCAN_EVERY_MS = 500;
let lastFullScan = 0;

function setStatus(text) {
  els.status.textContent = text;
}

/** Drives the viewfinder's framing guides and glow. Chrome only — the decoder
 *  reads the raw capture and never sees any of this. */
function setViewfinder(state) {
  els.viewfinder.dataset.state = state;
}

/** One cell per source block, rebuilt only when the stream's k changes.
 *  Capped because a 65535-block transfer would be 65535 DOM nodes for a
 *  texture nobody can read at that density. */
function syncBlockGrid(total, solved) {
  const cap = 512;
  const cells = Math.min(total, cap);
  if (gridCells.length !== cells) {
    els.blocksGrid.replaceChildren();
    gridCells = Array.from({ length: cells }, () => {
      const cell = document.createElement("i");
      els.blocksGrid.append(cell);
      return cell;
    });
  }
  // When capped, each cell stands for a proportional share of the blocks.
  const lit = total > 0 ? Math.round((solved / total) * cells) : 0;
  for (let index = 0; index < gridCells.length; index++) {
    gridCells[index].classList.toggle("solved", index < lit);
  }
}

function showError(text) {
  els.error.textContent = text;
  els.error.hidden = !text;
}

/** Text goes in as text, always. A snippet arrived over the optical channel
 *  from a screen this device does not control, and `innerHTML` on it would be
 *  a scripting vector with a camera for a delivery mechanism. */
function showText(text) {
  els.resultText.textContent = text;
  els.resultText.hidden = false;
}

function releaseObjectUrl() {
  if (objectUrl) {
    URL.revokeObjectURL(objectUrl);
    objectUrl = null;
  }
}

async function startCamera() {
  const generationAtStart = ++generation;
  showError("");
  hideResult();
  decodes = 0;
  regions = [];
  lastVerdict = null;
  startedAt = performance.now();

  try {
    await init();
  } catch (error) {
    showError(`The decoder could not be loaded: ${error}`);
    return;
  }
  receiver = new WasmReceiver();

  try {
    // The rear camera, at as much resolution as the device will give: a QR
    // module has to survive being sampled, and every pixel helps.
    stream = await navigator.mediaDevices.getUserMedia({
      video: {
        facingMode: { ideal: "environment" },
        width: { ideal: 1920 },
        height: { ideal: 1080 },
      },
      audio: false,
    });
  } catch (error) {
    showError(
      error && error.name === "NotAllowedError"
        ? "Camera access was refused. Allow it in your browser settings, then try again."
        : `The camera could not be opened: ${error}`,
    );
    return;
  }
  if (generationAtStart !== generation) {
    stream.getTracks().forEach((track) => track.stop());
    return;
  }

  els.video.srcObject = stream;
  await els.video.play();

  ensureWorkers();
  els.start.hidden = true;
  els.stop.hidden = false;
  els.progress.hidden = false;
  setViewfinder("scanning");
  setStatus("Looking for a code…");
  requestAnimationFrame(() => pump(generationAtStart));
}

function stopCamera() {
  generation++;
  if (stream) {
    stream.getTracks().forEach((track) => track.stop());
    stream = null;
  }
  els.video.srcObject = null;
  els.start.hidden = false;
  els.stop.hidden = true;
  els.progress.hidden = true;
  drawOverlay([]);
  setViewfinder("idle");
  setStatus("Stopped.");
}

function ensureWorkers() {
  if (workers.length) return;
  // Four is the cap on purpose: past that the phone is thermally throttled
  // long before it is decode-bound, and a hot phone drops camera frames.
  const count = Math.max(1, Math.min(4, navigator.hardwareConcurrency || 2));
  for (let index = 0; index < count; index++) {
    const worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
    worker.onmessage = (event) => onDecoded(worker, event.data);
    workers.push(worker);
    busy.push(false);
  }
}

/** One capture per animation frame, dispatched to whichever workers are free.
 *  Frames arriving while every worker is busy are dropped, not queued. */
function pump(generationAtStart) {
  if (generationAtStart !== generation) return;
  requestAnimationFrame(() => pump(generationAtStart));

  const video = els.video;
  if (!video.videoWidth || !video.videoHeight) return;

  if (!capture) {
    capture = document.createElement("canvas");
    captureContext = capture.getContext("2d", { willReadFrequently: true });
  }
  if (capture.width !== video.videoWidth || capture.height !== video.videoHeight) {
    capture.width = video.videoWidth;
    capture.height = video.videoHeight;
  }

  const free = busy.indexOf(false);
  if (free < 0) return;

  captureContext.drawImage(video, 0, 0);
  const now = performance.now();
  regions = regions.filter((region) => now - region.seen < REGION_TTL_MS);

  let job;
  if (regions.length && now - lastFullScan < FULL_SCAN_EVERY_MS) {
    job = cropJob(regions[0]);
    regions.push(regions.shift());
  } else {
    lastFullScan = now;
    job = fullJob();
  }
  if (!job) return;

  busy[free] = true;
  workers[free].postMessage(job, [job.buffer]);
}

function fullJob() {
  const image = captureContext.getImageData(0, 0, capture.width, capture.height);
  return {
    id: 0,
    buffer: image.data.buffer,
    width: capture.width,
    height: capture.height,
    offsetX: 0,
    offsetY: 0,
    full: true,
  };
}

function cropJob(region) {
  // A margin around the last sighting, so a code that drifted between frames
  // is still inside the crop.
  const pad = Math.max(16, Math.round(Math.max(region.box.w, region.box.h) * 0.25));
  const x = Math.max(0, Math.floor(region.box.x - pad));
  const y = Math.max(0, Math.floor(region.box.y - pad));
  const w = Math.min(capture.width - x, Math.ceil(region.box.w + pad * 2));
  const h = Math.min(capture.height - y, Math.ceil(region.box.h + pad * 2));
  if (w <= 0 || h <= 0) return null;
  const image = captureContext.getImageData(x, y, w, h);
  return {
    id: 0,
    buffer: image.data.buffer,
    width: w,
    height: h,
    offsetX: x,
    offsetY: y,
    full: false,
    quad: region.quad,
    modules: region.modules,
  };
}

function onDecoded(worker, message) {
  if (message.id === -1) {
    if (message.fatal) showError(`The QR decoder failed to start: ${message.fatal}`);
    return;
  }
  const index = workers.indexOf(worker);
  if (index >= 0) busy[index] = false;
  if (!receiver) return;

  const now = performance.now();
  for (const box of message.sightings ?? []) {
    // A symbol the decoder saw but could not read is still a fix on where a
    // code is. Aim a crop there next time.
    regions.push({ box, quad: null, modules: 0, seen: now });
  }

  for (const symbol of message.symbols ?? []) {
    decodes++;
    regions = regions.filter((region) => !overlaps(region.box, symbol.box));
    regions.push({ box: symbol.box, quad: symbol.quad, modules: symbol.modules, seen: now });
    handleFrame(symbol.bytes);
  }

  if (message.symbols?.length || message.sightings?.length) {
    drawOverlay(regions.map((region) => region.box));
  }
  els.decodes.textContent = String(decodes);
}

function overlaps(a, b) {
  return a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
}

/** Hand one decoded payload to the Rust receiver and render whatever it says.
 *  This function contains no protocol knowledge at all, which is the point. */
function handleFrame(bytes) {
  const event = receiver.addFrame(bytes);
  switch (event.kind) {
    case "ignored":
      return;
    case "incompatible":
      // The one failure that must not be silent: a version mismatch otherwise
      // looks exactly like bad lighting. Latched so it does not flicker.
      if (event.message !== lastVerdict) {
        lastVerdict = event.message;
        showError(event.message);
      }
      return;
    case "progress": {
      if (lastVerdict) {
        lastVerdict = null;
        showError("");
      }
      // Frames collected, not blocks solved. The peeling cascade back-loads:
      // blocks resolve in a burst near the end while frames arrive steadily,
      // so a bar driven by solved blocks looks stalled and then teleports.
      const percent = event.totalBlocks
        ? Math.min(100, Math.round((event.uniqueFrames / event.totalBlocks) * 100))
        : 0;
      els.bar.style.width = `${percent}%`;
      els.percent.innerHTML = `${percent}<small>%</small>`;
      els.progressLabel.textContent = "Receiving";
      els.progress.setAttribute("aria-valuenow", String(percent));
      els.frames.textContent = String(event.uniqueFrames);
      els.blocks.textContent = `${event.solvedBlocks}/${event.totalBlocks}`;
      syncBlockGrid(event.totalBlocks, event.solvedBlocks);
      setStatus("Hold steady — receiving.");
      return;
    }
    case "complete":
      showResult(event);
      return;
    case "failed":
      showError(event.message);
      setStatus("Ready to try again.");
      return;
  }
}

function showResult(event) {
  const seconds = (performance.now() - startedAt) / 1000;
  els.bar.style.width = "100%";
  els.percent.innerHTML = `100<small>%</small>`;
  els.progressLabel.textContent = "Complete";
  els.progress.setAttribute("aria-valuenow", "100");
  syncBlockGrid(1, 1);
  setViewfinder("done");
  els.result.hidden = false;
  els.resultName.textContent = event.name;
  els.resultStats.textContent =
    `${formatBytes(event.bytes.length)} · ${seconds.toFixed(1)}s · SHA-256 verified`;
  releaseObjectUrl();

  if (event.isText) {
    const text = new TextDecoder().decode(new Uint8Array(event.bytes));
    showText(text);
    els.resultSave.hidden = true;
    els.resultCopy.hidden = false;
    els.resultCopy.onclick = () => navigator.clipboard?.writeText(text);
  } else {
    els.resultText.hidden = true;
    els.resultCopy.hidden = true;
    els.resultSave.hidden = false;
    const blob = new Blob([new Uint8Array(event.bytes)], { type: event.mediaType });
    objectUrl = URL.createObjectURL(blob);
    els.resultSave.href = objectUrl;
    // The receiver already reduced this to a basename; the attribute is a
    // second pair of hands, not the only one.
    els.resultSave.download = event.name;
  }
  setStatus("Received and verified.");
  stopCamera();
  // stopCamera resets the viewfinder to idle; the transfer did succeed.
  setViewfinder("done");
}

function hideResult() {
  els.result.hidden = true;
  els.resultText.hidden = true;
  releaseObjectUrl();
}

function drawOverlay(boxes) {
  const canvas = els.overlay;
  const video = els.video;
  if (!video.videoWidth) return;
  const rect = video.getBoundingClientRect();
  if (canvas.width !== Math.round(rect.width) || canvas.height !== Math.round(rect.height)) {
    canvas.width = Math.round(rect.width);
    canvas.height = Math.round(rect.height);
  }
  const context = canvas.getContext("2d");
  context.clearRect(0, 0, canvas.width, canvas.height);
  // `object-fit: contain` on the video, so the same letterboxing applies here.
  const scale = Math.min(canvas.width / video.videoWidth, canvas.height / video.videoHeight);
  const offsetX = (canvas.width - video.videoWidth * scale) / 2;
  const offsetY = (canvas.height - video.videoHeight * scale) / 2;
  context.strokeStyle = "#3ddc84";
  context.lineWidth = 2;
  for (const box of boxes) {
    context.strokeRect(
      offsetX + box.x * scale,
      offsetY + box.y * scale,
      box.w * scale,
      box.h * scale,
    );
  }
}

function formatBytes(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

els.start.addEventListener("click", () => void startCamera());
els.stop.addEventListener("click", stopCamera);
els.dismiss.addEventListener("click", () => {
  hideResult();
  setViewfinder("idle");
  setStatus("Ready.");
});
window.addEventListener("pagehide", releaseObjectUrl);

if ("serviceWorker" in navigator) {
  // Registration failure is not fatal: it costs offline use, not the transfer.
  navigator.serviceWorker.register("./service-worker.js").catch(() => undefined);
}
