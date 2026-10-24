// Send mode: selection, presets and stream stats. The worker's Rust engine
// produces every module; code-display.js decides where and when they show.

import { CodeDisplay } from "./code-display.js";
import { formatBytes, formatDuration, formatRate } from "./format.js";

const PRESETS = {
  reliable: { bytes: 1465, fps: 24, codes: 1 },
  balanced: { bytes: 2331, fps: 30, codes: 2 },
  fast: { bytes: 2953, fps: 60, codes: 1 },
  max: { bytes: 2953, fps: 60, codes: 4 },
};
const BYTES_OPTIONS = [500, 1000, 1465, 1850, 2331, 2953];
const FPS_OPTIONS = [10, 15, 20, 24, 30, 55, 60, 120];
const CODES_OPTIONS = [1, 2, 4, 6, 9];
const HEADER_BYTES = 22;
const MAX_BYTES = 64 * 1024 * 1024;
const MAX_TEXT = 4 * 1024 * 1024;
const MAX_FILES = 256;

const $ = (id) => document.getElementById(id);
const els = {
  filesPanel: $("tx-files-panel"), textPanel: $("tx-text-panel"),
  drop: $("tx-drop"), files: $("tx-files"), folder: $("tx-folder"), folderButton: $("tx-folder-button"),
  summary: $("tx-summary"), clear: $("tx-clear"), list: $("tx-list"),
  text: $("tx-text"), textCount: $("tx-text-count"),
  bytes: $("tx-bytes"), fps: $("tx-fps"), codes: $("tx-codes"), ecc: $("tx-ecc"), presetRate: $("tx-preset-rate"),
  start: $("tx-start"), stop: $("tx-stop"), status: $("tx-status"), error: $("tx-error"),
  stage: $("tx-stage"), canvas: $("tx-canvas"), fullscreen: $("tx-fullscreen"), exit: $("tx-exit"),
  session: $("s-session"), cycleBar: $("s-cycle-bar"), cycle: $("s-cycle"),
  payload: $("s-payload"), compression: $("s-compression"), code: $("s-code"), k: $("s-k"),
  rate: $("s-rate"), ideal: $("s-ideal"), frames: $("s-frames"), flips: $("s-flips"), display: $("s-display"),
};
const kindInputs = [...document.querySelectorAll('input[name="tx-kind"]')];
const presetInputs = [...document.querySelectorAll('input[name="tx-preset"]')];

for (const [select, options, unit] of [[els.bytes, BYTES_OPTIONS, " B"], [els.fps, FPS_OPTIONS, ""], [els.codes, CODES_OPTIONS, ""]]) {
  for (const value of options) select.append(new Option(`${value}${unit}`, String(value)));
}

let selected = [];
let worker = null;
let info = null;
let settings = null;
let lastStats = 0;

// ------------------------------------------------------------- selection

function kind() {
  return kindInputs.find((input) => input.checked)?.value ?? "files";
}

function addFiles(list, fromFolder) {
  if (worker) return;
  const incoming = [...list];
  if (fromFolder && incoming.length && incoming.some((file) => !file.webkitRelativePath)) {
    showError("This browser did not report folder paths. Choose the files individually or send a ZIP.");
    return;
  }
  const next = [...selected, ...incoming.map((file) => ({ file, path: fromFolder ? file.webkitRelativePath : "" }))];
  if (next.length > MAX_FILES) return showError(`Choose at most ${MAX_FILES} files.`);
  if (next.reduce((sum, item) => sum + item.file.size, 0) > MAX_BYTES) {
    return showError("That is over the 64 MiB limit, including ZIP overhead.");
  }
  showError("");
  selected = next;
  renderSelection();
}

function renderSelection() {
  els.list.replaceChildren(...selected.map((item, index) => {
    const row = document.createElement("li");
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = item.path || item.file.name;
    name.title = name.textContent;
    const size = document.createElement("span");
    size.className = "size";
    size.textContent = formatBytes(item.file.size);
    const remove = document.createElement("button");
    remove.type = "button";
    remove.textContent = "×";
    remove.setAttribute("aria-label", `Remove ${name.textContent}`);
    remove.disabled = Boolean(worker);
    remove.addEventListener("click", () => {
      selected.splice(index, 1);
      renderSelection();
    });
    row.append(name, size, remove);
    return row;
  }));
  const total = selected.reduce((sum, item) => sum + item.file.size, 0);
  const zipped = selected.length > 1 || selected.some((item) => item.path);
  els.summary.textContent = selected.length
    ? `${selected.length} file${selected.length === 1 ? "" : "s"} · ${formatBytes(total)}${zipped ? " → transfer.zip" : ""}`
    : "Nothing selected";
  els.clear.disabled = !selected.length || Boolean(worker);
}

els.files.addEventListener("change", () => {
  addFiles(els.files.files, false);
  els.files.value = "";
});
els.folder.addEventListener("change", () => {
  addFiles(els.folder.files, true);
  els.folder.value = "";
});
if (!("webkitdirectory" in els.folder)) els.folderButton.hidden = true;
els.clear.addEventListener("click", () => {
  selected = [];
  renderSelection();
});
for (const type of ["dragenter", "dragover"]) {
  els.drop.addEventListener(type, (event) => {
    event.preventDefault();
    els.drop.classList.add("over");
  });
}
for (const type of ["dragleave", "drop"]) els.drop.addEventListener(type, () => els.drop.classList.remove("over"));
els.drop.addEventListener("drop", (event) => {
  event.preventDefault();
  addFiles(event.dataTransfer?.files ?? [], false);
});
for (const input of kindInputs) {
  input.addEventListener("change", () => {
    els.filesPanel.hidden = kind() !== "files";
    els.textPanel.hidden = kind() !== "text";
    showError("");
  });
}
els.text.addEventListener("input", () => {
  const bytes = new TextEncoder().encode(els.text.value).length;
  els.textCount.textContent = formatBytes(bytes);
  els.textCount.classList.toggle("over", bytes > MAX_TEXT);
});

// --------------------------------------------------------------- settings

function readSettings() {
  return {
    frameBytes: Number(els.bytes.value),
    fps: Number(els.fps.value),
    codes: Number(els.codes.value),
    ecc: els.ecc.value,
  };
}

function applyPreset(name) {
  const preset = PRESETS[name];
  if (!preset) return;
  els.bytes.value = String(preset.bytes);
  els.fps.value = String(preset.fps);
  els.codes.value = String(preset.codes);
  els.ecc.value = "L";
  settingsChanged();
}

function settingsChanged() {
  const next = readSettings();
  const match = Object.entries(PRESETS).find(([, preset]) =>
    preset.bytes === next.frameBytes && preset.fps === next.fps && preset.codes === next.codes && next.ecc === "L");
  for (const input of presetInputs) input.checked = match?.[0] === input.value;
  const raw = (next.fps * next.codes * (next.frameBytes - HEADER_BYTES)) / 1024;
  els.presetRate.textContent = `${formatRate(raw)} on the wire`;
  if (!worker || !settings) return;
  // Bytes per code and ECC are baked into every frame: re-encode. Frame rate
  // and code count only change the display, so the stream keeps its place.
  if (next.frameBytes !== settings.frameBytes || next.ecc !== settings.ecc) {
    stopSending();
    void startSending();
  } else {
    settings = next;
    display.configure(settings, info);
    showStreamInfo();
  }
}

for (const input of presetInputs) input.addEventListener("change", () => applyPreset(input.value));
for (const select of [els.bytes, els.fps, els.codes, els.ecc]) select.addEventListener("change", settingsChanged);
applyPreset("fast");

// ---------------------------------------------------------------- display

const display = new CodeDisplay(els.canvas, els.stage);
window.addEventListener("resize", () => display.layout());

// -------------------------------------------------------------- streaming

async function startSending() {
  if (worker) return;
  showError("");
  const payloadKind = kind();
  if (payloadKind === "files" && !selected.length) return showError("Choose at least one file first.");
  if (payloadKind === "text") {
    if (!els.text.value.trim()) return showError("Type or paste some text first.");
    if (new TextEncoder().encode(els.text.value).length > MAX_TEXT) return showError("Text is limited to 4 MiB; send it as a file instead.");
  }
  settings = readSettings();
  lock(true);
  setStatus("Preparing…");
  const current = new Worker(new URL("./send-worker.js", import.meta.url), { type: "module" });
  worker = current;
  current.onerror = () => {
    if (worker !== current) return;
    stopSending();
    showError("The sender could not load. Reload the page and try again.");
  };
  current.onmessage = ({ data }) => {
    if (worker !== current) return;
    if (data.type === "error") {
      stopSending();
      showError(data.message);
    } else if (data.type === "progress") {
      setStatus(data.message);
    } else if (data.type === "ready") {
      info = data.info;
      els.stage.dataset.state = "live";
      display.configure(settings, info);
      display.start((count) => current.postMessage({ type: "frames", count }));
      els.fullscreen.disabled = false;
      // On a phone the stage sits below the controls; bring it into view.
      if (matchMedia("(max-width: 899px)").matches) els.stage.scrollIntoView({ behavior: "smooth", block: "start" });
      setStatus(`Sending ${info.name}. Stop once the receiver shows the file.`);
      showStreamInfo();
      void keepAwake();
      statsLoop();
    } else if (data.type === "frames") {
      display.receive(data.modules, data.count, data.frameSize);
    }
  };
  current.postMessage({
    type: "prepare",
    kind: payloadKind,
    files: payloadKind === "files" ? selected.map((item) => item.file) : [],
    // The relative path does not reliably survive structured cloning.
    paths: payloadKind === "files" ? selected.map((item) => item.path) : [],
    text: els.text.value,
    settings,
  });
}

/** Mode switches and Stop: kill the worker, clear the stage. */
export function stopSending() {
  const wasSending = Boolean(worker);
  worker?.terminate();
  worker = null;
  info = null;
  display.stop();
  setFullscreen(false);
  els.stage.dataset.state = "idle";
  els.fullscreen.disabled = true;
  lock(false);
  releaseWakeLock();
  if (wasSending) setStatus("Stopped.");
}

function lock(locked) {
  els.start.hidden = locked;
  els.stop.hidden = !locked;
  for (const input of [els.files, els.folder, els.text, ...kindInputs]) input.disabled = locked;
  els.drop.classList.toggle("disabled", locked);
  renderSelection();
}

function showStreamInfo() {
  if (!info) return;
  const blockLen = info.blockLen;
  els.session.textContent = `session ${info.sessionId.toString(16).padStart(4, "0")}`;
  els.payload.textContent = `${formatBytes(info.originalBytes)}${info.fileCount > 1 ? ` · ${info.fileCount} files` : ""}`;
  els.compression.textContent = info.compressed ? `gzip → ${formatBytes(info.transmittedBytes)}` : "none (incompressible)";
  els.code.textContent = `V${info.qrVersion} ×${settings.codes} · ECC ${settings.ecc}`;
  els.k.textContent = `${info.sourceBlocks.toLocaleString()} × ${blockLen} B`;
  const perSecond = settings.fps * settings.codes;
  els.rate.textContent = `${formatRate((perSecond * blockLen) / 1024)} · ${perSecond} codes/s`;
  const ideal = info.sourceBlocks / perSecond;
  els.ideal.textContent = ideal < 0.1 ? "one frame" : formatDuration(ideal);
  els.ideal.title = "One clean sweep with no missed frames";
}

function statsLoop() {
  if (!worker || !info) return;
  requestAnimationFrame(statsLoop);
  const now = performance.now();
  if (now - lastStats < 200) return;
  lastStats = now;
  const cycle = info.cycleFrames;
  const position = display.shown % cycle;
  const k = info.sourceBlocks;
  const sweep = position < k;
  els.cycleBar.style.width = `${(position / cycle) * 100}%`;
  els.cycle.textContent = `${sweep ? "Systematic sweep" : "Repair frames"} ${(sweep ? position : position - k) + 1} / ${k} · carousel ${Math.floor(display.shown / cycle) + 1}`;
  els.frames.textContent = display.shown.toLocaleString();
  const target = settings.fps * settings.codes;
  const actual = display.flipTimes.length;
  els.flips.textContent = `${actual} / ${target} per s`;
  els.flips.classList.toggle("warn", actual < target * 0.9 && display.shown > target);
  const hz = display.refreshHz();
  const note = !hz ? "" : hz < settings.fps * 0.95 ? ` · caps ${Math.round(hz)} fps`
    : hz < settings.fps * 1.5 && settings.fps > 30 ? " · 1 refresh per frame" : "";
  els.display.textContent = hz ? `${Math.round(hz)} Hz${note}` : "—";
  els.display.classList.toggle("warn", Boolean(hz) && hz < settings.fps * 0.95);
  if (display.stall) setStatus(`This window stalled for ${(display.stall / 1000).toFixed(1)} s. Keep it visible and in front.`);
}

// -------------------------------------------------------------- full screen

function setFullscreen(on) {
  if (on === document.body.classList.contains("qr-full")) return;
  document.body.classList.toggle("qr-full", on);
  els.exit.hidden = !on;
  if (on) document.documentElement.requestFullscreen?.().catch(() => {});
  else if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
  display.layout();
}
els.fullscreen.addEventListener("click", () => setFullscreen(true));
els.exit.addEventListener("click", (event) => {
  event.stopPropagation();
  setFullscreen(false);
});
els.stage.addEventListener("click", () => {
  if (worker && info) setFullscreen(!document.body.classList.contains("qr-full"));
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") setFullscreen(false);
});
document.addEventListener("fullscreenchange", () => {
  if (!document.fullscreenElement && document.body.classList.contains("qr-full")) setFullscreen(false);
});

// ------------------------------------------------------------------ misc

let wakeLock = null;
async function keepAwake() {
  if (!navigator.wakeLock || wakeLock || document.hidden) return;
  try {
    wakeLock = await navigator.wakeLock.request("screen");
    wakeLock.addEventListener("release", () => { wakeLock = null; });
  } catch {
    // Optional: keep the screen awake by hand if the device refuses.
  }
}
function releaseWakeLock() {
  wakeLock?.release().catch(() => {});
  wakeLock = null;
}
document.addEventListener("visibilitychange", () => {
  if (!document.hidden && worker) void keepAwake();
});

function setStatus(text) {
  els.status.textContent = text;
}
function showError(text) {
  els.error.textContent = text;
  els.error.hidden = !text;
}

els.start.addEventListener("click", () => void startSending());
els.stop.addEventListener("click", stopSending);
window.addEventListener("pagehide", stopSending);
renderSelection();
