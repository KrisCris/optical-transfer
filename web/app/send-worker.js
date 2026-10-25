// Packing, fountain encoding and QR generation, off the UI thread. The Rust
// engine owns every byte on the wire; this file only moves data across.

import init, { WasmSender } from "./pkg/optical_wasm.js";

const ready = init();
let sender = null;
let frameSize = 0;

self.onmessage = async ({ data }) => {
  try {
    await ready;
    if (data.type === "prepare") {
      sender?.free();
      sender = new WasmSender();
      const { frameBytes, fps, ecc, wire, color } = data.settings;
      const session = new Uint16Array(1);
      do crypto.getRandomValues(session); while (session[0] === 0);
      let info;
      if (data.kind === "text") {
        info = sender.prepareText(data.text, frameBytes, fps, ecc, session[0], wire, color);
      } else {
        for (const [index, file] of data.files.entries()) {
          self.postMessage({ type: "progress", message: `Reading ${index + 1} of ${data.files.length}: ${file.name}` });
          const bytes = new Uint8Array(await file.arrayBuffer());
          const path = data.paths[index];
          if (path) sender.addDirectoryFile(file.name, file.type, path, bytes);
          else sender.addFile(file.name, file.type, bytes);
        }
        self.postMessage({ type: "progress", message: "Compressing and hashing…" });
        info = sender.prepareFiles(frameBytes, fps, ecc, session[0], wire, color);
      }
      frameSize = info.qrSize * info.qrSize;
      self.postMessage({ type: "ready", info });
    } else if (data.type === "frames" && sender) {
      const started = performance.now();
      const modules = sender.nextFrames(data.count);
      self.postMessage(
        { type: "frames", modules, count: data.count, frameSize, ms: performance.now() - started },
        [modules.buffer],
      );
    }
  } catch (error) {
    sender?.free();
    sender = null;
    self.postMessage({ type: "error", message: String(error?.message ?? error) });
  }
};
