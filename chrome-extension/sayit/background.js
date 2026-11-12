importScripts("common.js");

// The call finder. Every frame that holds a call microphone — in any tab or
// window, embedded or not — reports here over a port (relay.js):
// "tabId:frameId" → { port, calls, live, playing, newestCallAt, liveAt, where }
const frames = new Map();

chrome.runtime.onConnect.addListener((port) => {
  if (port.name !== "sayit-call") return;
  const s = port.sender || {};
  const key = `${s.tab ? s.tab.id : -1}:${s.frameId || 0}`;
  port.onMessage.addListener((m) => {
    if (!m || !m.status) return;
    if (m.status.calls > 0) frames.set(key, { ...m.status, port });
    else frames.delete(key);
  });
  port.onDisconnect.addListener(() => {
    const f = frames.get(key);
    if (f && f.port === port) frames.delete(key);
  });
});

const tell = (f, what) => {
  try {
    f.port.postMessage({ sayit: what });
  } catch {}
};

// One press: stop the message if it's playing anywhere; otherwise play it on
// the call that's connected right now (else on the newest microphone).
function pressed() {
  const all = [...frames.values()];
  const playing = all.filter((f) => f.playing);
  if (playing.length) {
    for (const f of playing) {
      f.playing = false;
      tell(f, "stop");
    }
    return "stopped";
  }
  const target =
    all.filter((f) => f.live > 0).sort((a, b) => b.liveAt - a.liveAt)[0] ||
    all.sort((a, b) => b.newestCallAt - a.newestCallAt)[0];
  if (!target) return "no-call";
  target.playing = true; // until its own report says otherwise
  tell(target, "play");
  return "playing";
}

chrome.commands.onCommand.addListener((command) => {
  if (command === "toggle-message") pressed();
});

chrome.runtime.onMessage.addListener((m, _sender, respond) => {
  if (!m) return;
  if (m.sayit === "hotkey" || m.sayit === "toggle-calls") respond(pressed());
  else if (m.sayit === "calls") {
    respond([...frames.values()].map(({ calls, live, playing, ready, where }) => ({ calls, live, playing, ready, where })));
  }
});

// A changed hotkey or site list reaches every open page at once (pages only
// follow storage themselves where the microphone runs: the message is large).
chrome.storage.onChanged.addListener(async (changes, area) => {
  if (area !== "local" || !changes.settings) return;
  for (const tab of await chrome.tabs.query({})) {
    chrome.tabs.sendMessage(tab.id, { sayit: "settings" }).catch(() => {});
  }
});

// SayIt 0.2 registered extra scripts for added sites; it now runs on every
// page, so remove them (otherwise those pages would get it twice).
chrome.runtime.onInstalled.addListener(async () => {
  try {
    const old = await chrome.scripting.getRegisteredContentScripts();
    if (old.length) await chrome.scripting.unregisterContentScripts({ ids: old.map((s) => s.id) });
  } catch {}
});
