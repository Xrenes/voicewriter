// Extension side of the dialer page: hands the saved message to inject.js
// and forwards the hotkey. (inject.js runs in the page and can't reach the
// extension's storage or messages itself.)
const toPage = (msg, transfer) => window.postMessage({ __sayit: "to-page", ...msg }, "*", transfer || []);

function base64ToBytes(b64) {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes.buffer;
}

async function sendMessageAudio() {
  const { message } = await chrome.storage.local.get("message");
  if (!message) return toPage({ type: "clear" });
  const bytes = base64ToBytes(message.data);
  toPage({ type: "clip", bytes }, [bytes]);
}

window.addEventListener("message", (e) => {
  if (e.source === window && e.data && e.data.__sayit === "from-page" && e.data.state === "ready") {
    sendMessageAudio();
  }
});
sendMessageAudio();

chrome.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && changes.message) sendMessageAudio();
});

chrome.runtime.onMessage.addListener((m) => {
  if (m && m.sayit === "toggle") toPage({ type: "toggle" });
});
