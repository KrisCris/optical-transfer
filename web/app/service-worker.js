// Offline shell for the receiver.
//
// The whole point of this application is that no network sits between the two
// devices. A receiver that needs the network to *load* would undercut that at
// the worst possible moment — a basement, a plane, an air-gapped room — so
// every runtime asset is precached and served cache-first.
//
// Cache-first is safe here because the cache name carries the build: a new
// build is a new cache, and the old one is deleted on activate. Nothing is
// served stale across versions, and nothing revalidates over a network that
// may not exist.

const VERSION = "optical-transfer-v12";

// Every file the receiver needs to reach its camera-ready screen. Checked
// against the served tree by scripts/check-web.py, so a new asset that is not
// listed here fails the build rather than failing offline in someone's hands.
const ASSETS = [
  "./",
  "./index.html",
  "./styles.css",
  "./app.js",
  "./receive.js",
  "./capture.js",
  "./decode-worker.js",
  "./telemetry.js",
  "./charts.js",
  "./format.js",
  "./cameras.js",
  "./send.js",
  "./code-display.js",
  "./send-worker.js",
  "./manifest.webmanifest",
  "./icons/icon.svg",
  "./pkg/optical_wasm.js",
  "./pkg/optical_wasm_bg.wasm",
  "./vendor/decimen-codec/decimen_codec.js",
  "./vendor/decimen-codec/decimen_codec.wasm",
];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(VERSION)
      .then((cache) => cache.addAll(ASSETS))
      // A precache that silently half-succeeded is worse than one that failed
      // loudly: it installs a receiver that breaks offline later.
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((key) => key !== VERSION).map((key) => caches.delete(key))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET") return;
  const url = new URL(request.url);
  // Only this origin's own assets. Nothing else should be reaching the network
  // at all, and caching a cross-origin response would hide it if it did.
  if (url.origin !== self.location.origin) return;

  event.respondWith(
    caches.match(request, { ignoreSearch: true }).then((hit) => {
      if (hit) return hit;
      // A navigation to any in-scope path is the app shell. Nothing received
      // over the camera is ever cached — not the file, not the frames.
      if (request.mode === "navigate") {
        return caches.match("./index.html").then((shell) => shell ?? fetch(request));
      }
      return fetch(request);
    }),
  );
});
