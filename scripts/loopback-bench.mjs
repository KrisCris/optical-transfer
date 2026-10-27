#!/usr/bin/env node
// Run web/app/tests/loopback.html in headless Chrome over a matrix of settings
// and print one line per run. No dependencies: Chrome's DevTools protocol over
// Node's built-in WebSocket.
//
//   python3 scripts/serve-web.py --port 8765 &
//   node scripts/loopback-bench.mjs [--base http://localhost:8765] \
//     [--runs "codes=1&fps=60" "codes=4&fps=60&wire=3"] [--size 1048576] [--repeat 2]
//
// Each run is a loopback.html query string; see that page for parameters
// (wire format, colour, the camera model).
//
// Headless, so animation frames are not throttled the way a hidden tab's are.

import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const args = process.argv.slice(2);
const option = (name, fallback) => {
  const index = args.indexOf(`--${name}`);
  return index === -1 ? fallback : args[index + 1];
};
const base = option("base", "http://localhost:8765");
const size = option("size", "1048576");
const repeat = Number(option("repeat", "1"));
const runsIndex = args.indexOf("--runs");
// Everything after --runs up to the next option: the option's own value, as
// in `--repeat 2`, is not a run.
const runsEnd = args.findIndex((value, index) => index > runsIndex && value.startsWith("--"));
const runs = runsIndex === -1
  ? ["codes=1&fps=60&wire=3", "codes=1&fps=60", "codes=4&fps=60&wire=3", "codes=4&fps=60",
    "codes=1&fps=60&color=1&chroma=1"]
  : args.slice(runsIndex + 1, runsEnd === -1 ? undefined : runsEnd);
const chrome = option("chrome", process.platform === "darwin"
  ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" : "google-chrome");

const profile = mkdtempSync(join(tmpdir(), "loopback-"));
const port = 9300 + Math.floor(Math.random() * 500);
const browser = spawn(chrome, [
  "--headless=new", `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`,
  "--autoplay-policy=no-user-gesture-required", "--disable-background-timer-throttling",
  "--disable-renderer-backgrounding", "--disable-backgrounding-occluded-windows",
  "--use-angle=metal", "--ignore-gpu-blocklist", "--enable-gpu-rasterization",
  "--window-size=1600,1200", "about:blank",
], { stdio: "ignore" });

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function json(path, init) {
  for (let attempt = 0; attempt < 50; attempt++) {
    try {
      return await (await fetch(`http://127.0.0.1:${port}${path}`, init)).json();
    } catch {
      await sleep(200);
    }
  }
  throw new Error("Chrome did not open its debugging port");
}

function session(url) {
  const socket = new WebSocket(url);
  let id = 0;
  const waiting = new Map();
  socket.onmessage = ({ data }) => {
    const message = JSON.parse(data);
    waiting.get(message.id)?.(message);
    waiting.delete(message.id);
  };
  const opened = new Promise((resolve) => { socket.onopen = resolve; });
  return {
    async send(method, params = {}) {
      await opened;
      const message = { id: ++id, method, params };
      return new Promise((resolve) => {
        waiting.set(message.id, resolve);
        socket.send(JSON.stringify(message));
      });
    },
    close: () => socket.close(),
  };
}

try {
  console.log("policy   wire colour codes fps  bytes   seconds  avg KiB/s  peak    catch  overhead  capture decode  tracked-ms  hz  drop%  util  ok");
  for (const run of runs) {
    for (let attempt = 0; attempt < repeat; attempt++) {
      const target = await json(`/json/new?${encodeURIComponent(`${base}/tests/loopback.html?${run}&size=${size}`)}`, { method: "PUT" });
      const page = session(target.webSocketDebuggerUrl);
      let result = null;
      // Half-second polls until the page's own timeout, plus a minute of
      // headroom for loading and preparing a large file.
      const timeout = Number(new URLSearchParams(run).get("timeout") ?? 120);
      for (let tick = 0; tick < (timeout + 60) * 2 && !result?.final; tick++) {
        await sleep(500);
        const reply = await page.send("Runtime.evaluate", { expression: "JSON.stringify(window.benchResult ?? null)", returnByValue: true });
        result = JSON.parse(reply.result?.result?.value ?? "null");
      }
      page.close();
      await fetch(`http://127.0.0.1:${port}/json/close/${target.id}`).catch(() => {});
      if (!result) {
        console.log(`${run}: no result`);
        continue;
      }
      const c = result.config;
      const colour = c.color ? (c.chroma ? "4:2:0" : "rgb") : c.chroma ? "mono*" : "mono";
      console.log([
        c.policy.padEnd(8), `v${result.wire ?? 3}`.padStart(4), colour.padStart(6),
        String(c.codes).padStart(5), String(c.fps).padStart(4), String(c.bytes).padStart(6),
        String(result.seconds).padStart(9), String(result.averageKiBs).padStart(10), String(result.peakKiBs).padStart(7),
        String(result.catchRate).padStart(7), String(result.overhead).padStart(9),
        String(result.captureFps).padStart(8), String(result.decodeFps).padStart(6),
        String(result.latency.tracked).padStart(11), String(Math.round(result.senderHz ?? 0)).padStart(4),
        String(Math.round((100 * result.totals.dropped) / Math.max(1, result.totals.crops))).padStart(6),
        String(result.utilization).padStart(5), result.verified ? " yes" : result.final ? " TIMEOUT" : " ?",
      ].join(" "));
    }
  }
} finally {
  // A loaded machine can take seconds to stop Chrome, whose helpers write to
  // the profile until they exit.
  const exited = browser.exitCode !== null || browser.signalCode !== null
    ? Promise.resolve()
    : new Promise((resolve) => browser.once("exit", resolve));
  browser.kill();
  await Promise.race([exited, sleep(10_000)]);
  rmSync(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
