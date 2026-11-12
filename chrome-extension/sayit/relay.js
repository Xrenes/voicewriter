// Extension side of a dialer page (and of the test page): hands inject.js the
// saved message and settings, and passes toggles between it and the
// extension. (inject.js runs in the page, so it can't reach extension
// storage itself.)
const sayitToPage = (msg, transfer) => window.postMessage(msg, "*", transfer || []);

// After an update the old copy of this script loses the extension until the
// tab is reloaded; don't let that throw inside the page.
function sayitToExtension(msg) {
  try {
    chrome.runtime.sendMessage(msg).catch(() => {});
  } catch {}
}

async function sayitSendConfig() {
  const s = await sayitSettings();
  sayitToPage({ __sayit: "config", hotkey: s.hotkey, muteMic: s.muteMic, hearIt: s.hearIt });
}

async function sayitSendMessage() {
  const { message } = await chrome.storage.local.get("message");
  if (!message) return sayitToPage({ __sayit: "clear" });
  const bytes = sayitBase64ToBuffer(message.data);
  sayitToPage({ __sayit: "clip", bytes }, [bytes]);
}

function sayitSendAll() {
  sayitSendConfig();
  sayitSendMessage();
}

window.addEventListener("message", (e) => {
  const d = e.data;
  if (e.source !== window || !d) return;
  if (d.__sayit === "state" && d.state === "ready") sayitSendAll();
  // The hotkey was pressed in a tab without a call: try the other dialer tabs.
  else if (d.__sayit === "toggle-elsewhere") sayitToExtension({ sayit: "toggle-other-tabs" });
});
sayitSendAll();

chrome.storage.onChanged.addListener((changes, area) => {
  if (area !== "local") return;
  if (changes.settings) sayitSendConfig();
  if (changes.message) sayitSendMessage();
});

chrome.runtime.onMessage.addListener((m) => {
  if (m && m.sayit === "toggle") sayitToPage({ __sayit: "toggle" });
});
