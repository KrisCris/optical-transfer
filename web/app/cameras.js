// Which camera a desktop receiver should open when the user has not picked
// one. A phone or tablet on the same Apple Account shows up on a Mac as an
// ordinary camera named after the device ("SH4D0W Camera", "Ana's iPhone
// Camera"): it is almost always the one to aim at another screen, because
// the built-in camera faces the user. The name is the owner's to choose, so
// "iPhone" in it is a bonus, not a requirement.

const PHONE = /\b(iPhone|iPad)\b/i;
// Part of the Mac or its display, so it faces the person at the keyboard.
const BUILT_IN = /FaceTime|MacBook|iMac|Built-in|Studio Display|Pro Display/i;
// Continuity's second feed: a cropped, top-down view of the desk.
const DESK_VIEW = /Desk View/i;
// Software cameras re-broadcast something else.
const VIRTUAL = /virtual|OBS|Snap Camera|NDI|mmhmm|Loom|Zoom|Teams|Screen/i;

/** The `deviceId` to prefer among `enumerateDevices()` video inputs, or ""
 *  to let the browser choose. Unlabelled devices (permission not yet granted)
 *  carry no information and are never picked. */
export function pickDesktopCamera(cameras) {
  const named = cameras.filter((device) => device.deviceId && device.label && !DESK_VIEW.test(device.label));
  const phone = named.find((device) => PHONE.test(device.label))
    ?? named.find((device) => !BUILT_IN.test(device.label) && !VIRTUAL.test(device.label));
  return phone?.deviceId ?? "";
}

// On an iPhone or iPad, Safari's rear camera is Apple's multi-lens one ("Back
// Triple Camera", "Back Dual Wide Camera"). It switches to the ultra-wide
// whenever the subject is closer than the main lens can focus, and held near
// that distance it hops between lenses: every hop blurs the code and moves
// it. The main lens is listed as a camera of its own ("Back Camera").

/** The main lens of the multi-lens camera `current` (`{deviceId, label}`)
 *  among `enumerateDevices()` video inputs, or null. Labels are localised, so
 *  it is found by shape rather than by name: the shortest other label whose
 *  letters all appear, in order, in the multi-lens camera's own. */
export function mainLens(cameras, current) {
  const own = [...(current.label ?? "")];
  const within = (label) => {
    const letters = [...label];
    let next = 0;
    for (const letter of own) if (letter === letters[next]) next++;
    return next === letters.length && letters.length < own.length;
  };
  return cameras
    .filter((device) => device.deviceId && device.deviceId !== current.deviceId && device.label && within(device.label))
    .sort((a, b) => [...a.label].length - [...b.label].length)[0] ?? null;
}

/** The rear camera to read with on an iPhone or iPad: the main lens on its
 *  own where it can be found, else the browser's choice. `open(deviceId)`
 *  opens a camera, "" meaning the browser's choice for the rear; `list()`
 *  resolves to the video inputs; `known` is the main lens an earlier start
 *  found. Resolves to the stream and the main lens's id, "" if it is not one. */
export async function openMainLens(open, list, known) {
  if (known) {
    const camera = await open(known).catch(unlessRefused);
    if (camera && isSingleRearLens(camera)) return { camera, main: known };
    close(camera);
  }
  const camera = await open("");
  if (!isMultiLens(camera)) return { camera, main: "" };
  const track = camera.getVideoTracks()[0];
  const main = mainLens(await list(), { deviceId: track.getSettings().deviceId, label: track.label });
  if (!main) return { camera, main: "" };
  // Phones will not open a second camera while one is live.
  close(camera);
  const single = await open(main.deviceId).catch(unlessRefused);
  if (single && isSingleRearLens(single)) return { camera: single, main: main.deviceId };
  close(single);
  return { camera: await open(""), main: "" };
}

/** WebKit gives Apple's multi-lens cameras, and only those, a zoom range
 *  that goes below 1: the main lens is 1 and the ultra-wide 0.5. */
function isMultiLens(camera) {
  return camera.getVideoTracks()[0]?.getCapabilities?.().zoom?.min < 1;
}

function isSingleRearLens(camera) {
  const facing = camera.getVideoTracks()[0]?.getCapabilities?.().facingMode ?? [];
  return facing.includes("environment") && !isMultiLens(camera);
}

function close(camera) {
  camera?.getTracks().forEach((track) => track.stop());
}

/** A refused permission is the user's answer; any other failure to open a
 *  lens means falling back to the browser's own choice. */
function unlessRefused(error) {
  if (error?.name === "NotAllowedError") throw error;
  return null;
}
