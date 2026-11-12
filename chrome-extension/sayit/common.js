// Shared by the popup, the test page, the content relay and the service worker.
const SAYIT_DEFAULTS = {
  hotkey: { code: "KeyL", key: "L", alt: true, ctrl: false, shift: false, meta: false },
  muteMic: true,
  hearIt: true, // also play the message in your own headset
  sites: [], // origins added in Settings, e.g. "https://app.agency.com"
  everywhere: false, // SayIt's microphone on every website
};

// Dialers SayIt's microphone runs on — and every frame embedded in them, so a
// dialer inside a CRM or a calling window is covered whatever its address.
// (inject.js has its own copy of this list.)
const SAYIT_BUILT_IN = ["aloware.com", "gohighlevel.com", "leadconnectorhq.com", "hubspot.com"];

function sayitHost(origin) {
  try {
    return new URL(origin).hostname;
  } catch {
    return "";
  }
}

const sayitBuiltIn = (origin) => {
  const h = sayitHost(origin);
  return SAYIT_BUILT_IN.some((d) => h === d || h.endsWith(`.${d}`));
};

// This frame and the frames it's embedded in, innermost first.
const sayitFrameOrigins = () => [location.origin, ...Array.from(location.ancestorOrigins || [])];

// Whether SayIt's microphone runs in a frame with these origins.
const sayitCovers = (origins, s) =>
  !!s.everywhere || origins.some((o) => sayitBuiltIn(o) || (s.sites || []).includes(o));

const sayitHotkeyMatches = (e, h) =>
  !!h &&
  !!h.code &&
  e.code === h.code &&
  e.altKey === !!h.alt &&
  e.ctrlKey === !!h.ctrl &&
  e.shiftKey === !!h.shift &&
  e.metaKey === !!h.meta;

function sayitHotkeyLabel(h) {
  if (!h || !h.code) return "not set";
  const parts = [];
  if (h.ctrl) parts.push("Ctrl");
  if (h.alt) parts.push("Alt");
  if (h.shift) parts.push("Shift");
  if (h.meta) parts.push("Win");
  parts.push(h.key || h.code);
  return parts.join("+");
}

async function sayitSettings() {
  const { settings } = await chrome.storage.local.get("settings");
  return { ...SAYIT_DEFAULTS, ...(settings || {}) };
}

function sayitBase64ToBuffer(b64) {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes.buffer;
}
