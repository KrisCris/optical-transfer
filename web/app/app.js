// Entry point: two modes on one page. Switching releases whatever the other
// mode held — the camera, its decode workers, or the sender's worker.

import { stopReceiving } from "./receive.js";
import { stopSending } from "./send.js";

const modes = ["receive", "send"];
let current = "receive";

function switchMode(mode) {
  if (mode === current) return;
  if (current === "receive") stopReceiving();
  else stopSending();
  current = mode;
  for (const name of modes) {
    const tab = document.getElementById(`mode-${name}`);
    tab.setAttribute("aria-selected", String(name === mode));
    tab.tabIndex = name === mode ? 0 : -1;
    document.getElementById(`panel-${name}`).hidden = name !== mode;
  }
  history.replaceState(null, "", mode === "send" ? "#send" : "#");
}

for (const mode of modes) {
  const tab = document.getElementById(`mode-${mode}`);
  tab.addEventListener("click", () => switchMode(mode));
  tab.addEventListener("keydown", (event) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const next = modes.find((name) => name !== mode);
    switchMode(next);
    document.getElementById(`mode-${next}`).focus();
  });
}
if (location.hash === "#send") switchMode("send");

if ("serviceWorker" in navigator) {
  // Failure costs offline use, not the transfer.
  navigator.serviceWorker.register("./service-worker.js").catch(() => {});
}
