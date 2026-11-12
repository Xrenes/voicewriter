// Extension side of every page (and of the test page). It hands inject.js the
// settings — and, where SayIt's microphone runs, the saved message — and links
// pages holding a call to the background, which finds the call wherever it is.
// (inject.js runs in the page, so it can't reach the extension itself.)
(() => {
  if (window.__sayitRelay) return;
  window.__sayitRelay = true;

  const toPage = (msg, transfer) => window.postMessage(msg, "*", transfer || []);
  // After an update this copy loses the extension until the tab is reloaded.
  const alive = () => {
    try {
      return !!chrome.runtime.id;
    } catch {
      return false;
    }
  };

  let covered = false;
  let clipSent = false;

  async function loadSettings() {
    if (!alive()) return;
    const s = await sayitSettings();
    const was = covered;
    covered = location.protocol === "chrome-extension:" || sayitCovers(sayitFrameOrigins(), s);
    toPage({ __sayit: "config", hotkey: s.hotkey, muteMic: s.muteMic, hearIt: s.hearIt, covered });
    if (covered && !clipSent) sendClip();
    if (covered && !was) watchStorage();
  }

  async function sendClip() {
    if (!alive()) return;
    clipSent = true;
    const { message } = await chrome.storage.local.get("message");
    if (!message) return toPage({ __sayit: "clear" });
    const bytes = sayitBase64ToBuffer(message.data);
    toPage({ __sayit: "clip", bytes }, [bytes]);
  }

  // Only pages where the microphone runs follow changes live (the saved
  // message is large); the others re-read the settings when you come back.
  let watching = false;
  function watchStorage() {
    if (watching) return;
    watching = true;
    chrome.storage.onChanged.addListener((changes, area) => {
      if (area !== "local" || !alive()) return;
      if (changes.settings) loadSettings();
      if (changes.message) sendClip();
    });
  }
  window.addEventListener("focus", loadSettings);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") loadSettings();
  });

  // A page holding a call keeps a line open to the background: its status
  // goes up (which also keeps the background awake for an instant response),
  // and play / stop come down.
  let port = null;
  let last = null;
  function connect() {
    if (!alive()) return false;
    try {
      port = chrome.runtime.connect({ name: "sayit-call" });
    } catch {
      port = null;
      return false;
    }
    port.onMessage.addListener((m) => {
      if (m && (m.sayit === "play" || m.sayit === "stop")) toPage({ __sayit: m.sayit });
    });
    port.onDisconnect.addListener(() => {
      port = null;
      // The background restarted: tell it again where the call is.
      setTimeout(() => {
        if (!port && last && last.calls > 0) sendStatus(last);
      }, 50);
    });
    return true;
  }
  function sendStatus(st) {
    last = st;
    if (!port) {
      if (!st.calls || !connect()) return;
    }
    try {
      port.postMessage({ status: st });
    } catch {}
    if (!st.calls) {
      try {
        port.disconnect();
      } catch {}
      port = null;
    }
  }

  window.addEventListener("message", (e) => {
    const d = e.data;
    if (e.source !== window || !d) return;
    if (d.__sayit === "state" && d.state === "ready") loadSettings();
    else if (d.__sayit === "status") sendStatus(d.status);
    // The hotkey was pressed here, but the connected call isn't in this frame.
    else if (d.__sayit === "hotkey" && alive()) {
      chrome.runtime
        .sendMessage({ sayit: "hotkey" })
        .then((r) => r === "no-call" && toPage({ __sayit: "no-call" }))
        .catch(() => {});
    }
  });

  chrome.runtime.onMessage.addListener((m, _sender, respond) => {
    if (!m) return;
    if (m.sayit === "settings") loadSettings(); // changed in the popup
    else if (m.sayit === "ping") respond(true); // the popup checks SayIt runs in a tab
  });

  loadSettings();
})();
