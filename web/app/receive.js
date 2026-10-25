// Receive mode: camera → decode workers → the Rust receiver → a verified file.
//
// The protocol lives entirely in WasmReceiver; this file never parses a frame.
// Per-frame work is kept to what decoding needs: a decoded symbol updates the
// tracker and the receiver, and everything the user sees is drawn by a render
// loop at its own pace. Writing the DOM per decoded frame cost more main-thread
// time than reading the camera did.

import init, { WasmReceiver } from "./pkg/optical_wasm.js";
import { CaptureLoop, DecodePool, RegionTracker } from "./capture.js";
import { Telemetry, HISTORY_BUCKETS } from "./telemetry.js";
import { BlockMap, drawBrackets, drawThroughput } from "./charts.js";
import { formatBytes, formatDuration, formatRate } from "./format.js";
import { pickDesktopCamera } from "./cameras.js";

// WasmReceiver.addFrame outcome codes; see crates/optical-wasm/src/lib.rs.
const PROGRESS = 1;
const COMPLETE = 2;
const INCOMPATIBLE = 3;
const FAILED = 4;
const USEFUL = 0x40;
const NEW_STREAM = 0x80;

const NO_SIGNAL_MS = 8000;
const PANEL_MS = 150;
const MAP_MS = 120;

const $ = (id) => document.getElementById(id);
const els = {
  viewfinder: $("rx-viewfinder"), video: $("rx-video"), overlay: $("rx-overlay"),
  hudState: $("rx-hud-state"), hudRate: $("rx-hud-rate"), hudBar: $("rx-hud-bar"),
  start: $("rx-start"), stop: $("rx-stop"), settings: $("rx-settings"),
  camera: $("rx-camera"), width: $("rx-width"), fps: $("rx-fps"), workers: $("rx-workers"),
  status: $("rx-status"), error: $("rx-error"), hint: $("rx-hint"),
  phase: $("rx-phase"), percent: $("rx-percent"), speed: $("rx-speed"), eta: $("rx-eta"),
  elapsed: $("rx-elapsed"), progress: $("rx-progress"), bar: $("rx-bar"),
  chart: $("rx-chart"), chartScale: $("rx-chart-scale"), map: $("rx-map"), mapLabel: $("rx-map-label"),
  result: $("rx-result"), resultName: $("rx-result-name"), resultMeta: $("rx-result-meta"),
  resultImage: $("rx-result-image"), resultText: $("rx-result-text"),
  save: $("rx-save"), share: $("rx-share"), copy: $("rx-copy"), again: $("rx-again"),
};
const t = (id) => $(`t-${id}`);

const tracker = new RegionTracker();
const telemetry = new Telemetry();
const blockMap = new BlockMap(els.map);
const pool = new DecodePool(
  () => new Worker(new URL("./decode-worker.js", import.meta.url), { type: "module" }),
  onReply,
);
const loop = new CaptureLoop({
  video: els.video, pool, tracker,
  onCapture: (now, plan) => telemetry.capture(now, plan),
  onDropped: (now) => telemetry.dropped(now),
});

let receiver = null;
let stream = null;
let running = false;
let generation = 0;
let cameraStartedAt = 0;
let lastAcceptedAt = 0;
let lastColorAt = -Infinity;
let verdict = null;
let objectUrl = null;
let renderHandle = 0;
let lastPanel = -Infinity;
let lastMap = -Infinity;
let completed = false;

// ---------------------------------------------------------------- settings

const cores = navigator.hardwareConcurrency || 4;
const maxWorkers = Math.max(1, Math.min(8, cores));
for (let count = 1; count <= maxWorkers; count++) els.workers.append(new Option(String(count), String(count)));
// Every core the device offers: tracked decodes are cheap, and a pool behind
// the camera drops frames the fountain then has to wait a carousel for.
els.workers.value = String(maxWorkers);
els.workers.addEventListener("change", () => {
  if (running) pool.resize(Number(els.workers.value));
});
// Both are fixed when the camera opens, so a change reopens it; the receiver
// and its progress are untouched.
for (const select of [els.width, els.fps]) {
  select.addEventListener("change", () => {
    if (running) void restartCamera();
  });
}
document.addEventListener("click", (event) => {
  if (els.settings.open && !els.settings.contains(event.target)) els.settings.open = false;
});

const MOBILE = /iPhone|iPad|iPod|Android/i.test(navigator.userAgent) ||
  (navigator.maxTouchPoints > 1 && /Macintosh/.test(navigator.userAgent));
const CAMERA_KEY = "optical-transfer.camera";

function rememberedCamera() {
  try {
    return localStorage.getItem(CAMERA_KEY) ?? "";
  } catch {
    return "";
  }
}

async function listCameras() {
  try {
    return (await navigator.mediaDevices.enumerateDevices()).filter((device) => device.kind === "videoinput");
  } catch {
    return [];
  }
}

const AUTOMATIC = MOBILE ? "Rear (automatic)" : "Automatic";
els.camera.options[0].text = AUTOMATIC;

/** Lists every camera the browser will name. Before permission is granted
 *  browsers hide names and ids, so the list fills in after the first start. */
async function populateCameras() {
  const cameras = (await listCameras()).filter((device) => device.deviceId);
  if (!cameras.length) return;
  const chosen = els.camera.value || rememberedCamera();
  els.camera.replaceChildren(
    new Option(AUTOMATIC, ""),
    ...cameras.map((device, index) => new Option(device.label || `Camera ${index + 1}`, device.deviceId)),
  );
  els.camera.value = cameras.some((device) => device.deviceId === chosen) ? chosen : "";
}
els.camera.addEventListener("change", () => {
  try {
    localStorage.setItem(CAMERA_KEY, els.camera.value);
  } catch {
    // Only a convenience: the picker still works for this visit.
  }
  if (running) void restartCamera();
});

/** The device to open: the user's pick, else a Continuity Camera on a
 *  desktop, else whatever `facingMode` finds. Labels are only readable once
 *  camera permission has been granted, so the first ever start on a Mac may
 *  still open the built-in camera; every start after that finds the phone. */
async function preferredCamera() {
  if (els.camera.value) return els.camera.value;
  if (MOBILE) return "";
  const remembered = rememberedCamera();
  const cameras = await listCameras();
  if (remembered && cameras.some((device) => device.deviceId === remembered)) return remembered;
  return pickDesktopCamera(cameras);
}

// A Continuity Camera appears only once the phone is near, locked and still,
// which is often after the receiver has started. Refresh the list, and on
// Automatic move to the phone as soon as it shows up; the transfer so far is
// kept, since only the camera is reopened.
void populateCameras();
navigator.mediaDevices?.addEventListener?.("devicechange", async () => {
  await populateCameras();
  if (!running) return;
  // The camera in use went away (the phone was picked up or walked off).
  if (stream?.getVideoTracks()[0]?.readyState === "ended") return void restartCamera();
  if (els.camera.value) return;
  const wanted = await preferredCamera();
  const current = stream?.getVideoTracks()[0]?.getSettings?.().deviceId;
  if (wanted && wanted !== current) void restartCamera();
});

// ------------------------------------------------------------------ camera

/** iOS treats `frameRate: {ideal: 60}` as a suggestion and delivers 30, so
 *  demand the rate exactly first and relax only when the camera refuses.
 *  Phones reach 120 fps only in their 16:9 formats, so the 4:3 size is a
 *  preference the exact rate is allowed to override. */
async function acquireCamera() {
  const width = Number(els.width.value);
  const fps = Number(els.fps.value);
  const device = await preferredCamera();
  const base = {
    ...(device ? { deviceId: { exact: device } } : { facingMode: { ideal: "environment" } }),
    width: { ideal: width },
    height: { ideal: Math.round((width * 3) / 4) },
  };
  try {
    return await navigator.mediaDevices.getUserMedia({ audio: false, video: { ...base, frameRate: { exact: fps } } });
  } catch (error) {
    if (error?.name !== "OverconstrainedError" && error?.name !== "NotReadableError") throw error;
    return navigator.mediaDevices.getUserMedia({ audio: false, video: { ...base, frameRate: { ideal: fps } } });
  }
}

async function start() {
  if (!navigator.mediaDevices?.getUserMedia) {
    showError("Camera access needs a secure page: open this over https:// (or localhost).");
    return;
  }
  const mine = ++generation;
  showError("");
  hideResult();
  els.start.disabled = true;
  setStatus("Opening the camera…");
  try {
    await init();
  } catch (error) {
    els.start.disabled = false;
    showError(`The receiver could not load: ${error}`);
    return;
  }
  receiver?.free();
  receiver = new WasmReceiver();
  tracker.reset();
  telemetry.reset();
  blockMap.reset();
  completed = false;
  verdict = null;

  let camera;
  try {
    camera = await acquireCamera();
  } catch (error) {
    if (mine !== generation) return;
    els.start.disabled = false;
    setStatus("");
    showError(error?.name === "NotAllowedError"
      ? "Camera access was refused. Allow it in the browser's site settings, then try again."
      : `The camera could not be opened: ${error?.message ?? error}`);
    return;
  }
  if (mine !== generation) {
    camera.getTracks().forEach((track) => track.stop());
    return;
  }
  await attach(camera);
  if (mine !== generation) return;

  running = true;
  cameraStartedAt = performance.now();
  lastAcceptedAt = 0;
  pool.resize(Number(els.workers.value));
  telemetry.workers = pool.size;
  loop.start();
  els.start.hidden = true;
  els.start.disabled = false;
  els.stop.hidden = false;
  setStatus("Looking for codes…");
  void populateCameras();
  void keepAwake();
  startRendering();
}

async function attach(camera) {
  stream = camera;
  els.video.srcObject = stream;
  try {
    await els.video.play();
  } catch {
    // Autoplay of a muted inline camera preview is allowed everywhere that
    // matters; a refusal here still leaves frames flowing to rVFC.
  }
  els.viewfinder.dataset.state = "acquiring";
  syncAspect();
  applyFocus();
}

/** Switching lenses needs a fresh getUserMedia, and phones will not open a
 *  second camera while one is live: stop first, then reacquire. */
async function restartCamera() {
  stream?.getTracks().forEach((track) => track.stop());
  try {
    await attach(await acquireCamera());
  } catch (error) {
    stopReceiving();
    showError(`The camera could not be reopened: ${error?.message ?? error}`);
  }
}

function applyFocus() {
  const track = stream?.getVideoTracks()[0];
  const modes = track?.getCapabilities?.().focusMode;
  // A lens hunting between frames is the top decode killer; ask for
  // continuous focus where the camera offers it, silently otherwise.
  if (Array.isArray(modes) && modes.includes("continuous")) {
    track.applyConstraints({ advanced: [{ focusMode: "continuous" }] }).catch(() => {});
  }
}

function syncAspect() {
  const { videoWidth, videoHeight } = els.video;
  if (videoWidth && videoHeight) els.viewfinder.style.aspectRatio = `${videoWidth} / ${videoHeight}`;
}
els.video.addEventListener("resize", syncAspect);

function releaseCamera() {
  loop.stop();
  pool.forget();
  stream?.getTracks().forEach((track) => track.stop());
  stream = null;
  els.video.srcObject = null;
  running = false;
  releaseWakeLock();
}

/** Mode switches and Stop: release the camera, the workers and decoder memory. */
export function stopReceiving() {
  generation++;
  const wasRunning = running;
  releaseCamera();
  pool.resize(0);
  els.start.hidden = false;
  els.start.disabled = false;
  els.stop.hidden = true;
  if (!completed) {
    els.viewfinder.dataset.state = "idle";
    if (wasRunning) setStatus("Stopped.");
  }
  render(performance.now(), true);
  stopRendering();
}

// ------------------------------------------------------------------ frames

function onReply(job, reply) {
  if (!job) {
    showError(`The QR decoder failed to start: ${reply.fatal}`);
    return;
  }
  if (job.generation !== loop.generation || !receiver || completed) return;
  const now = performance.now();
  telemetry.reply(now, job, reply);
  for (const box of reply.sightings ?? []) tracker.noteSighting(box, now);
  for (const symbol of reply.symbols ?? []) {
    tracker.noteDecoded(symbol.box, now, symbol.quad, symbol.modules);
    if (symbol.color) lastColorAt = now;
    if (!completed) handleFrame(symbol.bytes, job.capturedAt, now);
  }
}

function handleFrame(bytes, capturedAt, now) {
  const outcome = receiver.addFrame(bytes);
  const kind = outcome & 0x0f;
  if (kind === PROGRESS || kind === COMPLETE) {
    const newStream = (outcome & NEW_STREAM) !== 0;
    if (newStream) {
      blockMap.reset();
      telemetry.setStats(receiver.stats());
    }
    telemetry.accepted(now, capturedAt, (outcome & USEFUL) !== 0, newStream, telemetry.stats?.blockLen);
    lastAcceptedAt = now;
    if (verdict) {
      verdict = null;
      showError("");
    }
  }
  if (kind === COMPLETE) finish(now);
  else if (kind === INCOMPATIBLE) {
    // The one failure that must not be silent: a version mismatch otherwise
    // looks exactly like bad lighting. Latched so it does not flicker.
    const message = receiver.message();
    if (message !== verdict) {
      verdict = message;
      showError(message);
    }
  } else if (kind === FAILED) {
    telemetry.resetTransfer();
    blockMap.reset();
    showError(receiver.message());
  }
}

function finish(now) {
  completed = true;
  telemetry.setStats(receiver.stats());
  telemetry.completed(now);
  const received = receiver.takeResult();
  releaseCamera();
  pool.resize(0);
  els.start.hidden = false;
  els.stop.hidden = true;
  els.viewfinder.dataset.state = "done";
  showResult(received);
  setStatus("Received and verified.");
  render(now, true);
  stopRendering();
}

// ------------------------------------------------------------------ result

const PREVIEWABLE = /^image\/(png|jpeg|gif|webp|avif|bmp)$/;

function showResult(received) {
  const snapshot = telemetry.snapshot(performance.now());
  const bytes = received.bytes;
  const name = received.name;
  els.resultName.textContent = received.isText ? "Text snippet" : name;
  els.resultMeta.textContent = [
    formatBytes(bytes.length),
    formatDuration(snapshot.elapsed),
    `${formatRate(snapshot.averageKiBs)} avg`,
  ].join(" · ");
  releaseObjectUrl();
  els.resultImage.hidden = true;
  els.resultText.hidden = true;

  if (received.isText) {
    // Text from a screen this device does not control: always textContent,
    // never markup.
    const text = new TextDecoder().decode(bytes);
    els.resultText.textContent = text;
    els.resultText.hidden = false;
    els.save.hidden = true;
    els.share.hidden = true;
    els.copy.hidden = !navigator.clipboard;
    els.copy.onclick = () => navigator.clipboard.writeText(text).then(
      () => { els.copy.textContent = "Copied"; },
      () => { els.copy.textContent = "Copy failed"; },
    );
    els.copy.textContent = "Copy text";
  } else {
    const blob = new Blob([bytes], { type: received.mediaType });
    objectUrl = URL.createObjectURL(blob);
    els.save.href = objectUrl;
    els.save.download = name;
    els.save.hidden = false;
    els.copy.hidden = true;
    if (PREVIEWABLE.test(received.mediaType)) {
      els.resultImage.src = objectUrl;
      els.resultImage.hidden = false;
    }
    const file = new File([blob], name, { type: received.mediaType });
    const shareable = navigator.canShare?.({ files: [file] });
    els.share.hidden = !shareable;
    els.share.onclick = () => navigator.share({ files: [file] }).catch(() => {});
  }
  els.result.hidden = false;
  els.result.scrollIntoView({ block: "nearest", behavior: "smooth" });
}

function hideResult() {
  els.result.hidden = true;
  els.resultImage.removeAttribute("src");
  releaseObjectUrl();
}

function releaseObjectUrl() {
  if (objectUrl) URL.revokeObjectURL(objectUrl);
  objectUrl = null;
}

// ------------------------------------------------------------------ render

function startRendering() {
  if (renderHandle) return;
  const frame = (now) => {
    renderHandle = requestAnimationFrame(frame);
    render(now);
  };
  renderHandle = requestAnimationFrame(frame);
}

function stopRendering() {
  cancelAnimationFrame(renderHandle);
  renderHandle = 0;
}

function render(now, force = false) {
  if (running) drawBrackets(els.overlay, els.video, tracker.regions, now);
  else els.overlay.getContext("2d").clearRect(0, 0, els.overlay.width, els.overlay.height);
  if (!force && now - lastPanel < PANEL_MS) return;
  lastPanel = now;
  if (receiver && !completed) telemetry.setStats(receiver.stats());
  const snap = telemetry.snapshot(now);
  const stats = snap.stats;

  // Viewfinder HUD.
  if (running) {
    const mode = tracker.mode();
    els.viewfinder.dataset.state = mode;
    const codes = tracker.decodedCount();
    els.hudState.textContent = mode === "acquiring" ? "Searching"
      : `${codes} code${codes === 1 ? "" : "s"}${mode === "degraded" ? ` of ${tracker.expected}` : ""}`;
    els.hudRate.textContent = stats && snap.liveKiBs > 0 ? formatRate(snap.liveKiBs) : `${snap.symbolFps.toFixed(0)} reads/s`;
  }

  const fraction = completed ? 1 : stats ? stats.solvedBlocks / stats.totalBlocks : 0;
  const percent = Math.min(completed ? 100 : 99, Math.floor(fraction * 100));
  els.percent.textContent = String(percent);
  els.bar.style.width = els.hudBar.style.width = `${fraction * 100}%`;
  els.progress.setAttribute("aria-valuenow", String(percent));
  els.speed.textContent = completed ? formatRate(snap.averageKiBs) : stats ? formatRate(snap.liveKiBs) : "—";
  els.eta.textContent = completed ? "done" : snap.eta === null ? "—" : `~${formatDuration(snap.eta)}`;
  els.elapsed.textContent = stats ? formatDuration(snap.elapsed) : "—";
  els.phase.textContent = completed ? "Verified" : stats ? "Receiving" : running ? "Listening" : "Waiting";
  els.phase.dataset.tone = completed ? "done" : stats ? "live" : "";

  const series = telemetry.series(now);
  drawThroughput(els.chart, series, HISTORY_BUCKETS);
  els.chartScale.textContent = series.length ? `peak ${formatRate(snap.peakKiBs)} · 60 s` : "";

  if (receiver && (force || now - lastMap >= MAP_MS) && (stats || completed)) {
    lastMap = now;
    const map = completed && stats ? new Uint8Array(stats.totalBlocks).fill(1) : receiver.solvedMap();
    if (map.length) blockMap.draw(map, now);
    if (stats) els.mapLabel.textContent = `${stats.solvedBlocks.toLocaleString()} / ${stats.totalBlocks.toLocaleString()}`;
  }

  // Telemetry panel.
  const totals = snap.totals;
  t("size").textContent = stats ? formatBytes(stats.totalBytes) : "—";
  // v4's counters mean RaptorQ's things; the labels follow the stream.
  const v3 = stats?.wire === 3;
  t("wire").textContent = stats
    ? `v${stats.wire} · ${v3 ? "LT" : "RaptorQ"}${now - lastColorAt < 2000 ? " · colour" : ""}` : "—";
  t("k").previousElementSibling.textContent = v3 || !stats ? "Blocks (k)" : "Symbols (k)";
  t("blocklen").previousElementSibling.textContent = v3 || !stats ? "Block size" : "Symbol size";
  t("pending").previousElementSibling.textContent = v3 || !stats ? "Parked eqs" : "Repair held";
  t("elim").previousElementSibling.textContent = v3 || !stats ? "Eliminations" : "Decode tries";
  t("k").textContent = stats ? stats.totalBlocks.toLocaleString() : "—";
  t("blocklen").textContent = stats ? `${stats.blockLen.toLocaleString()} B` : "—";
  t("frames").textContent = stats ? `${stats.uniqueFrames} / ${stats.duplicateFrames}` : "—";
  t("redundant").textContent = stats ? String(stats.redundantFrames) : "—";
  t("pending").textContent = stats ? String(stats.pendingEquations) : "—";
  t("elim").textContent = stats ? String(stats.eliminations) : "—";
  t("catch").textContent = snap.catchRate === null ? "—" : `${(Math.min(1, snap.catchRate) * 100).toFixed(0)}%`;
  t("overhead").textContent = snap.overhead === null ? "—" : `${snap.overhead.toFixed(3)}×`;
  t("avgpeak").textContent = stats ? `${formatRate(snap.averageKiBs)} / ${formatRate(snap.peakKiBs)}` : "—";

  t("capture").textContent = `${snap.captureFps.toFixed(0)} fps`;
  t("decode").textContent = `${snap.decodeFps.toFixed(0)}/s · ${(snap.utilization * 100).toFixed(0)}% busy`;
  t("symbols").textContent = `${snap.symbolFps.toFixed(0)}/s`;
  t("codes").textContent = `${tracker.decodedCount()} (max ${tracker.expected})`;
  t("mode").textContent = running ? `${tracker.mode()} · ${totals.fullScans} scans` : "—";
  t("workers").textContent = `${pool.size} of ${cores} cores`;
  t("tracked").textContent = totals.trackedAttempts
    ? `${((totals.trackedHits / totals.trackedAttempts) * 100).toFixed(0)}% of ${totals.trackedAttempts}` : "—";
  const latency = snap.latency;
  t("latency").textContent = latency.tracked || latency.crop || latency.full
    ? `${ms(latency.tracked)} · ${ms(latency.crop)} · ${ms(latency.full)}` : "—";
  t("latency").title = "Tracked crop · crop · full frame, milliseconds per decode";
  t("dropped").textContent = totals.captures
    ? `${totals.dropped} (${((totals.dropped / totals.captures) * 100).toFixed(0)}%)` : "—";
  const track = stream?.getVideoTracks()[0];
  const settings = track?.getSettings?.();
  if (settings) {
    // The ceiling says whether a higher capture rate is worth asking for.
    const ceiling = Math.round(track.getCapabilities?.().frameRate?.max ?? 0);
    const rate = Math.round(settings.frameRate ?? 0);
    t("camera").textContent = `${settings.width}×${settings.height} @ ${rate}${ceiling > rate ? ` (max ${ceiling})` : ""}`;
    t("camera").title = track.label;
  }
  if (pool.version) $("t-decoder").textContent = `zxing ${pool.version}`;

  els.hint.hidden = !(running && !lastAcceptedAt && now - cameraStartedAt > NO_SIGNAL_MS);
  if (running && stats && !completed) setStatus("Hold steady — receiving.");
}

const ms = (value) => (value ? value.toFixed(1) : "–");

// ------------------------------------------------------------------ misc

function setStatus(text) {
  els.status.textContent = text;
}

function showError(text) {
  els.error.textContent = text;
  els.error.hidden = !text;
}

let wakeLock = null;
async function keepAwake() {
  if (!navigator.wakeLock || wakeLock) return;
  try {
    wakeLock = await navigator.wakeLock.request("screen");
    wakeLock.addEventListener("release", () => { wakeLock = null; });
  } catch {
    // Optional: the transfer works without it.
  }
}
function releaseWakeLock() {
  wakeLock?.release().catch(() => {});
  wakeLock = null;
}
document.addEventListener("visibilitychange", () => {
  if (!document.hidden && running) void keepAwake();
});

els.start.addEventListener("click", () => void start());
els.stop.addEventListener("click", stopReceiving);
els.again.addEventListener("click", () => {
  hideResult();
  void start();
});
window.addEventListener("pagehide", releaseObjectUrl);
render(performance.now(), true);
