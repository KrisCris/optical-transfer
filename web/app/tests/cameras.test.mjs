import assert from "node:assert/strict";
import test from "node:test";
import { pickDesktopCamera } from "../cameras.js";

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
