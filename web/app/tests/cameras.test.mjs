import assert from "node:assert/strict";
import test from "node:test";
import { mainLens, openMainLens, pickDesktopCamera } from "../cameras.js";

const camera = (deviceId, label) => ({ kind: "videoinput", deviceId, label });

test("a Continuity Camera is found by its device name, not the word iPhone", () => {
  const cameras = [
    camera("built-in", "FaceTime HD Camera"),
    camera("desk", "SH4D0W Desk View Camera"),
    camera("phone", "SH4D0W Camera"),
  ];
  assert.equal(pickDesktopCamera(cameras), "phone");
});

test("a camera named as an iPhone wins over any other external one", () => {
  const cameras = [
    camera("usb", "Logitech BRIO"),
    camera("phone", "Ana's iPhone Camera"),
    camera("built-in", "MacBook Pro Camera"),
  ];
  assert.equal(pickDesktopCamera(cameras), "phone");
});

test("built-in, Desk View and virtual cameras are never preferred", () => {
  const cameras = [
    camera("built-in", "FaceTime HD Camera (Built-in)"),
    camera("desk", "Desk View Camera"),
    camera("obs", "OBS Virtual Camera"),
    camera("display", "Studio Display Camera"),
  ];
  assert.equal(pickDesktopCamera(cameras), "");
});

test("unlabelled devices from before permission are left to the browser", () => {
  assert.equal(pickDesktopCamera([camera("", ""), camera("abc", "")]), "");
  assert.equal(pickDesktopCamera([]), "");
});

// Safari's camera list on a Pro iPhone, in English.
const iphone = [
  camera("front", "Front Camera"),
  camera("triple", "Back Triple Camera"),
  camera("dual-wide", "Back Dual Wide Camera"),
  camera("dual", "Back Dual Camera"),
  camera("ultra", "Back Ultra Wide Camera"),
  camera("wide", "Back Camera"),
  camera("tele", "Back Telephoto Camera"),
];

test("the main lens of a multi-lens iPhone camera is the plain back camera", () => {
  assert.equal(mainLens(iphone, iphone[1])?.deviceId, "wide");
  // Dual Wide spells out Back Dual Camera too; the shorter, plainer one wins.
  assert.equal(mainLens(iphone, iphone[2])?.deviceId, "wide");
});

test("the main lens is found by the shape of localised labels, not English words", () => {
  const cameras = [
    camera("front", "前置相机"),
    camera("triple", "后置三镜头相机"),
    camera("ultra", "后置超广角相机"),
    camera("wide", "后置相机"),
    camera("tele", "后置长焦相机"),
  ];
  assert.equal(mainLens(cameras, cameras[1])?.deviceId, "wide");
});

test("no main lens is invented when no label fits", () => {
  assert.equal(mainLens(iphone, iphone[5]), null);
  assert.equal(mainLens([iphone[0], iphone[1]], iphone[1]), null);
  assert.equal(mainLens([camera("", ""), camera("x", "")], iphone[1]), null);
  assert.equal(mainLens([], iphone[1]), null);
});

/** A phone that opens one camera at a time and reports, for each, what
 *  WebKit does: its facing, and a zoom reaching 0.5 on the multi-lens ones. */
function fakePhone(devices, { rear = "triple", broken = [] } = {}) {
  const opened = [];
  let live = 0;
  const open = async (deviceId) => {
    assert.equal(live, 0, "a second camera opened while one was live");
    opened.push(deviceId);
    const id = deviceId || rear;
    if (broken.includes(id)) throw Object.assign(new Error("busy"), { name: "NotReadableError" });
    const device = devices.find((candidate) => candidate.deviceId === id);
    if (!device) throw Object.assign(new Error("gone"), { name: "OverconstrainedError" });
    live++;
    const track = {
      label: device.label,
      stop: () => { live--; },
      getSettings: () => ({ deviceId: id }),
      getCapabilities: () => ({
        facingMode: [id === "front" ? "user" : "environment"],
        zoom: { min: /Triple|Dual Wide/.test(device.label) ? 0.5 : 1, max: 10 },
      }),
    };
    return { id, getVideoTracks: () => [track], getTracks: () => [track] };
  };
  return { open, list: async () => devices, opened, live: () => live };
}

test("an iPhone reads with its main lens, not the camera that hops between lenses", async () => {
  const phone = fakePhone(iphone);
  const { camera, main } = await openMainLens(phone.open, phone.list, "");
  assert.equal(camera.id, "wide");
  assert.equal(main, "wide");
  assert.deepEqual(phone.opened, ["", "wide"]);
  assert.equal(phone.live(), 1);
});

test("a main lens found before is opened straight away", async () => {
  const phone = fakePhone(iphone);
  const { camera } = await openMainLens(phone.open, phone.list, "wide");
  assert.equal(camera.id, "wide");
  assert.deepEqual(phone.opened, ["wide"]);
});

test("a remembered lens that is gone is found again", async () => {
  const phone = fakePhone(iphone);
  const { camera, main } = await openMainLens(phone.open, phone.list, "stale");
  assert.equal(camera.id, "wide");
  assert.equal(main, "wide");
  assert.deepEqual(phone.opened, ["stale", "", "wide"]);
});

test("a phone with one rear lens keeps the browser's choice", async () => {
  const phone = fakePhone([camera("front", "Front Camera"), camera("wide", "Back Camera")], { rear: "wide" });
  const { camera: opened, main } = await openMainLens(phone.open, phone.list, "");
  assert.equal(opened.id, "wide");
  assert.equal(main, "");
  assert.deepEqual(phone.opened, [""]);
});

test("the browser's choice comes back when the main lens will not open or cannot be named", async () => {
  const busy = fakePhone(iphone, { broken: ["wide"] });
  const first = await openMainLens(busy.open, busy.list, "");
  assert.equal(first.camera.id, "triple");
  assert.equal(first.main, "");
  assert.equal(busy.live(), 1);

  const unnamed = fakePhone([camera("front", "Front Camera"), camera("triple", "Back Triple Camera")]);
  const second = await openMainLens(unnamed.open, unnamed.list, "");
  assert.equal(second.camera.id, "triple");
  assert.deepEqual(unnamed.opened, [""]);
});

test("a refused camera permission is passed on, not retried", async () => {
  const refuse = async () => { throw Object.assign(new Error("no"), { name: "NotAllowedError" }); };
  await assert.rejects(openMainLens(refuse, async () => iphone, "wide"), { name: "NotAllowedError" });
});
