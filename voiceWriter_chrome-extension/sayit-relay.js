// SayIt's extension side of every page (and of the SayIt test page). It hands
// sayit-inject.js the settings and, where SayIt's microphone runs, the saved
// message, and links pages holding a call to the background, which finds the
// call wherever it is. (sayit-inject.js runs in the page, so it can't reach
// the extension itself.) Ported from SayIt for Chrome 0.3.0.
(() => {
  const VW = globalThis.VoiceWriterShared;
  const S = VW.SAYIT;

  // After an extension update, the copy injected into this tab takes over
  // from the old one, which can no longer reach the extension.
  const TAKEOVER = `vw-sayit-relay-${chrome.runtime.id}`;
  document.dispatchEvent(new Event(TAKEOVER));
  const lifetime = new AbortController();
  const on = (target, type, fn, opts = {}) => target.addEventListener(type, fn, { ...opts, signal: lifetime.signal });

  const toPage = (msg, transfer) => window.postMessage(msg, "*", transfer || []);
  const alive = () => {
    try {
      return !lifetime.signal.aborted && !!chrome.runtime.id;
    } catch {
      return false;
    }
  };

  let covered = false;
  let clipSent = false;

  async function loadSettings() {
    if (!alive()) return;
    const s = await S.settings();
    const was = covered;
    covered = location.protocol === "chrome-extension:" || S.covers(S.frameOrigins(), s);
    toPage({ __vwsayit: "config", hotkey: s.hotkey, muteMic: s.muteMic, hearIt: s.hearIt, ownNoiseCancel: !!s.ownNoiseCancel, covered });
    if (covered && !clipSent) sendClip();
    if (covered && !was) watchStorage();
  }

  async function sendClip() {
    if (!alive()) return;
    clipSent = true;
    activeId = (await S.library()).activeId;
    const message = await S.audioOf(activeId);
    if (!message) return toPage({ __vwsayit: "clear" });
    const bytes = S.base64ToBuffer(message.data);
    toPage({ __vwsayit: "clip", bytes }, [bytes]);
  }
  let activeId = ""; // the message this page has

  // Only pages where the microphone runs follow changes live (the saved
  // message is large); the others re-read the settings when you come back.
  let watching = false;
  async function onStorage(changes, area) {
    if (area !== "local" || !alive()) return;
    if (changes.sayit) loadSettings();
    // Another message picked, or the picked one replaced / moved over.
    if (changes.sayitActiveId || changes.sayitMessage || (activeId && changes[S.audioKey(activeId)])) return sendClip();
    // A changed list can change which one plays (a lone message always does).
    if (changes.sayitMessages && (await S.library()).activeId !== activeId) sendClip();
  }
  function watchStorage() {
    if (watching) return;
    watching = true;
    chrome.storage.onChanged.addListener(onStorage);
  }
  on(window, "focus", loadSettings);
  on(document, "visibilitychange", () => {
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
      port = chrome.runtime.connect({ name: "vw-sayit-call" });
    } catch {
      port = null;
      return false;
    }
    port.onMessage.addListener((m) => {
      if (m && (m.sayit === "play" || m.sayit === "stop")) toPage({ __vwsayit: m.sayit });
    });
    port.onDisconnect.addListener(() => {
      port = null;
      // The background restarted: tell it again where the call is.
      setTimeout(() => {
        if (!port && last && last.calls > 0 && alive()) sendStatus(last);
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
    } catch {
      /* the background is restarting */
    }
    if (!st.calls) {
      try {
        port.disconnect();
      } catch {
        /* already closed */
      }
      port = null;
    }
  }

  on(window, "message", (e) => {
    const d = e.data;
    if (e.source !== window || !d || !d.__vwsayit) return;
    if (d.__vwsayit === "state" && d.state === "ready") loadSettings();
    else if (d.__vwsayit === "status") sendStatus(d.status);
    // The hotkey was pressed here, but the connected call isn't in this frame.
    else if (d.__vwsayit === "hotkey" && alive()) {
      chrome.runtime
        .sendMessage({ sayit: "hotkey" })
        .then((r) => r === "no-call" && toPage({ __vwsayit: "no-call" }))
        .catch(() => {});
    }
  });

  function onMessage(m, _sender, respond) {
    if (!m || !alive()) return;
    if (m.sayit === "settings") loadSettings(); // changed in the popup
    else if (m.sayit === "report") toPage({ __vwsayit: "report" }); // the background lost track of calls
    else if (m.sayit === "ping") respond(true); // the popup checks SayIt runs in a tab
  }
  chrome.runtime.onMessage.addListener(onMessage);

  on(
    document,
    TAKEOVER,
    () => {
      lifetime.abort();
      try {
        port?.disconnect();
      } catch {
        /* already gone */
      }
      try {
        chrome.runtime.onMessage.removeListener(onMessage);
        chrome.storage.onChanged.removeListener(onStorage);
      } catch {
        /* this copy's extension context is gone */
      }
    },
    { once: true },
  );

  loadSettings();
})();
