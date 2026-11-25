// VoiceWriter welcome / setup page. Opened on install, and again when
// dictation needs microphone access (Chrome can only show the microphone
// prompt on a visible extension page, not in the background).
const VW = globalThis.VoiceWriterShared;
const $ = (id) => document.getElementById(id);

function hint(el, text, cls = "") {
  el.textContent = text;
  el.className = `hint ${cls}`.trim();
}

async function renderKey() {
  const { apiKey } = await VW.loadSettings();
  const st = $("keyState");
  st.textContent = apiKey ? "Saved" : "Not set";
  st.classList.toggle("ok", !!apiKey);
}

async function saveKey() {
  const key = $("keyInput").value.trim();
  if (!key) {
    $("keyInput").focus();
    return;
  }
  await chrome.storage.local.set({ apiKey: key });
  $("keyInput").value = "";
  renderKey();
  hint($("keyHint"), "Saved. Testing the key…");
  let r;
  try {
    r = await chrome.runtime.sendMessage({ type: "testKey", key });
  } catch (e) {
    r = { ok: false, error: e.message };
  }
  if (r?.ok) hint($("keyHint"), `✓ ${r.result}. You're set.`, "good");
  else hint($("keyHint"), r?.error || "The key couldn't be tested.", "err");
}

async function micState() {
  try {
    return (await navigator.permissions.query({ name: "microphone" })).state;
  } catch {
    return "prompt";
  }
}

async function renderMic() {
  const state = await micState();
  const st = $("micState");
  st.textContent = state === "granted" ? "Allowed" : state === "denied" ? "Blocked" : "Not allowed yet";
  st.classList.toggle("ok", state === "granted");
  $("micBtn").hidden = state === "granted";
  $("micSettings").hidden = state !== "denied";
  if (state === "granted") hint($("micHint"), "✓ Microphone allowed. Hold the dictation key in any text field to try it.", "good");
  else if (state === "denied")
    hint($("micHint"), "Chrome is blocking the microphone for VoiceWriter. Open the microphone settings, set Microphone to Allow, then come back.", "err");
  else hint($("micHint"), "");
}

async function askMic() {
  try {
    const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    stream.getTracks().forEach((t) => t.stop());
  } catch (e) {
    if (e.name === "NotFoundError") hint($("micHint"), "No microphone was found. Plug one in and try again.", "err");
  }
  renderMic();
}

async function renderKeys() {
  const { hotkeys } = await VW.loadSettings();
  document.querySelectorAll("kbd[data-key]").forEach((k) => {
    k.textContent = hotkeys[k.dataset.key] || "(off)";
  });
  const sayit = await VW.SAYIT.settings();
  $("sayitKey").textContent = VW.SAYIT.combo(sayit.hotkey) || "(off)";
}

async function init() {
  $("version").textContent = `VoiceWriter v${chrome.runtime.getManifest().version}`;
  $("keySave").onclick = saveKey;
  $("keyInput").addEventListener("keydown", (e) => {
    if (e.key === "Enter") saveKey();
  });
  $("micBtn").onclick = askMic;
  $("micSettings").onclick = () =>
    chrome.tabs.create({ url: `chrome://settings/content/siteDetails?site=${encodeURIComponent(location.origin)}` });

  renderKey();
  renderKeys();
  await renderMic();
  try {
    (await navigator.permissions.query({ name: "microphone" })).onchange = renderMic;
  } catch {
    /* older Chrome */
  }
  chrome.storage.onChanged.addListener((changes, area) => {
    if (area !== "local") return;
    if (changes.apiKey) renderKey();
    if (changes.hotkeys || changes.sayit) renderKeys();
  });

  if (location.hash === "#microphone") {
    const step = $("stepMic");
    step.classList.add("highlight");
    step.scrollIntoView({ block: "center" });
    if ((await micState()) === "prompt") $("micBtn").focus();
  }
}

init();
