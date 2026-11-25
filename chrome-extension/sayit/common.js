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

// ---- Saved messages ----
// In chrome.storage.local:
//   messages    [{ id, name, secs, added }] — in the order they were added
//   activeId    the message the hotkey plays ("" = none picked)
//   msg:<id>    { data: base64 WAV, type } — each message's audio
// SayIt 0.3 kept a single message under "message"; it becomes "My message".
const SAYIT_LEGACY_ID = "legacy";
const sayitAudioKey = (id) => `msg:${id}`;

// The list and the message the hotkey plays. A lone message always plays.
// Until the old single message is moved over, it counts as the list.
async function sayitLibrary() {
  const { messages, activeId, message } = await chrome.storage.local.get(["messages", "activeId", "message"]);
  if (!Array.isArray(messages) && message) {
    const legacy = { id: SAYIT_LEGACY_ID, name: "My message", secs: message.secs || 0, added: 0 };
    return { messages: [legacy], activeId: SAYIT_LEGACY_ID };
  }
  const list = Array.isArray(messages) ? messages : [];
  let active = list.some((m) => m.id === activeId) ? activeId : "";
  if (!active && list.length === 1) active = list[0].id;
  return { messages: list, activeId: active };
}

// A message's audio: { data (base64 WAV), type }, or null.
async function sayitAudioOf(id) {
  if (!id) return null;
  const key = sayitAudioKey(id);
  const got = await chrome.storage.local.get([key, "message"]);
  return got[key] || (id === SAYIT_LEGACY_ID && got.message) || null;
}

async function sayitActiveAudio() {
  return sayitAudioOf((await sayitLibrary()).activeId);
}

// Move SayIt 0.3's single message into the list. Safe to run more than once,
// even at the same time: it always lands under the same id.
async function sayitMigrate() {
  const { message, messages } = await chrome.storage.local.get(["message", "messages"]);
  if (!message) return;
  if (!Array.isArray(messages)) {
    await chrome.storage.local.set({
      [sayitAudioKey(SAYIT_LEGACY_ID)]: { data: message.data, type: message.type || "audio/wav" },
      messages: [{ id: SAYIT_LEGACY_ID, name: "My message", secs: message.secs || 0, added: Date.now() }],
      activeId: SAYIT_LEGACY_ID,
    });
  }
  await chrome.storage.local.remove("message");
}

function sayitBase64ToBuffer(b64) {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes.buffer;
}
