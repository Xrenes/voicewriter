// The board in its own small window, used while you're on a page
// VoiceWriter can't draw on (Chrome's own pages, the new tab page).
(() => {
  const VW = globalThis.VoiceWriterShared;
  const $ = (id) => document.getElementById(id);
  const cache = {};
  let snap = null;

  function render(snapshot) {
    if (!snapshot) return;
    snap = snapshot;
    VW.renderLive($("feed"), $("empty"), snapshot, cache);
    $("state").classList.toggle("ended", !snapshot.active);
    $("state").classList.toggle("static", snapshot.mode === "board");
    $("stateText").textContent = snapshot.label || (snapshot.active ? "Live" : "Stopped");
    $("notice").hidden = !snapshot.notice;
    $("notice").textContent = snapshot.notice || "";
  }

  chrome.runtime.onMessage.addListener((msg) => {
    if (msg?.type === "vw-live-update") render(msg.snapshot);
    return false;
  });
  chrome.runtime.sendMessage({ type: "liveSnapshot" }).then((r) => r?.ok && render(r.result), () => {});

  let scale = 1;
  try {
    scale = Number(localStorage.getItem("liveScale")) || 1;
  } catch {
    /* storage unavailable */
  }
  const applyScale = () => {
    document.documentElement.style.setProperty("--scale", String(scale));
    try {
      localStorage.setItem("liveScale", String(scale));
    } catch {
      /* storage unavailable */
    }
  };
  applyScale();
  $("smaller").onclick = () => {
    scale = Math.max(0.6, +(scale - 0.1).toFixed(2));
    applyScale();
  };
  $("bigger").onclick = () => {
    scale = Math.min(2.4, +(scale + 0.1).toFixed(2));
    applyScale();
  };
  $("clear").onclick = () => chrome.runtime.sendMessage({ type: "liveClear" }).catch(() => {});
  $("copy").onclick = async () => {
    try {
      await navigator.clipboard.writeText(VW.liveText(snap));
      $("copy").textContent = "Copied";
    } catch {
      $("copy").textContent = "Failed";
    }
    setTimeout(() => ($("copy").textContent = "Copy"), 1200);
  };
  $("close").onclick = () => chrome.runtime.sendMessage({ type: "liveStop" }).catch(() => {});

  // Alt+A (or your board key) works here too.
  VW.loadSettings().then((s) => {
    document.addEventListener("keydown", (e) => {
      if (VW.comboFromEvent(e) !== s.hotkeys.live) return;
      e.preventDefault();
      if (!e.repeat) chrome.runtime.sendMessage({ type: "liveStop" }).catch(() => {});
    });
  });
})();
