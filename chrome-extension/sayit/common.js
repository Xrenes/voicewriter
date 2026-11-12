// Shared by the popup, the test page, the content relay and the service worker.
const SAYIT_DEFAULTS = {
  hotkey: { code: "KeyL", key: "L", alt: true, ctrl: false, shift: false, meta: false },
  muteMic: true,
  hearIt: true, // also play the message quietly in your own headset
};

const SAYIT_DIALERS = [
  "https://*.aloware.com/*",
  "https://*.gohighlevel.com/*",
  "https://*.leadconnectorhq.com/*",
];

// Whether a built-in dialer pattern already covers this https origin.
function sayitBuiltIn(origin) {
  const host = new URL(origin).hostname;
  return SAYIT_DIALERS.some((p) => {
    const domain = p.slice("https://*.".length, -"/*".length);
    return host === domain || host.endsWith(`.${domain}`);
  });
}

// "https://app.example.com/*" → "app.example.com"
const sayitSiteName = (pattern) => pattern.replace(/^https:\/\//, "").replace(/\/\*$/, "");

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
