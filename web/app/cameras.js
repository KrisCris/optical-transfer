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
