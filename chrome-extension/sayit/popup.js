const $ = (id) => document.getElementById(id);
const els = {
  state: $("state"),
  record: $("record"),
  listen: $("listen"),
  save: $("save"),
  discard: $("discard"),
  remove: $("remove"),
  playCall: $("playCall"),
  hint: $("hint"),
  allowMic: $("allowMic"),
  error: $("error"),
  player: $("player"),
};

let settings = { ...SAYIT_DEFAULTS };
let saved = null; // { data (base64 WAV), type, secs }
let take = null; // { wav: Uint8Array, secs } — recorded, not saved yet
let recorder = null;
let startedAt = 0;
let tick = 0;

const clock = (s) => {
  const t = Math.round(s);
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
};
const showError = (e) => (els.error.textContent = e ? String(e) : "");

// ---- Message view ----
function render() {
  const recording = !!recorder;
  if (recording) els.state.textContent = `● Recording ${clock((Date.now() - startedAt) / 1000)} — say your message`;
  else if (take) els.state.textContent = `New recording · ${clock(take.secs)} · not saved yet`;
  else if (saved) els.state.textContent = `Saved message · ${clock(saved.secs || 0)}`;
  else els.state.textContent = "No message yet — press Record";
  els.state.classList.toggle("live", recording);

  els.record.textContent = recording ? "■ Stop" : saved || take ? "● Re-record" : "● Record";
  els.listen.hidden = recording || (!take && !saved);
  els.save.hidden = els.discard.hidden = recording || !take;
  els.remove.hidden = recording || !!take || !saved;
  els.playCall.hidden = recording || !!take || !saved;
  els.hint.textContent = saved
    ? `During a call press ${sayitHotkeyLabel(settings.hotkey)} — again to stop.`
    : "";
}

els.record.addEventListener("click", async () => {
  showError("");
  stopListening();
  if (recorder) {
    recorder.stop();
    return;
  }
  let stream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  } catch (e) {
    els.allowMic.hidden = false;
    showError("Chrome hasn't let SayIt use the microphone yet.");
    return;
  }
  els.allowMic.hidden = true;
  const chunks = [];
  recorder = new MediaRecorder(stream);
  recorder.ondataavailable = (e) => e.data.size && chunks.push(e.data);
  recorder.onstop = async () => {
    stream.getTracks().forEach((t) => t.stop());
    const type = recorder.mimeType;
    recorder = null;
    clearInterval(tick);
    try {
      take = await processTake(new Blob(chunks, { type }));
    } catch (e) {
      take = null;
      showError(`Couldn't use that recording: ${e}`);
    }
    render();
  };
  take = null;
  startedAt = Date.now();
  recorder.start();
  tick = setInterval(render, 500);
  render();
});

function stopListening() {
  els.player.pause();
  els.listen.textContent = "▶ Play";
}

els.listen.addEventListener("click", () => {
  if (!els.player.paused) return stopListening();
  els.player.src = take
    ? URL.createObjectURL(new Blob([take.wav], { type: "audio/wav" }))
    : `data:audio/wav;base64,${saved.data}`;
  els.player.play();
  els.listen.textContent = "■ Stop";
});
els.player.addEventListener("ended", stopListening);

els.save.addEventListener("click", async () => {
  stopListening();
  saved = { data: bytesToBase64(take.wav), type: "audio/wav", secs: take.secs };
  await chrome.storage.local.set({ message: saved });
  take = null;
  render();
});

els.discard.addEventListener("click", () => {
  stopListening();
  take = null;
  render();
});

let removeArmed = false;
els.remove.addEventListener("click", async () => {
  if (!removeArmed) {
    removeArmed = true;
    els.remove.textContent = "Sure? Delete";
    setTimeout(() => {
      removeArmed = false;
      els.remove.textContent = "Delete";
    }, 3000);
    return;
  }
  removeArmed = false;
  els.remove.textContent = "Delete";
  stopListening();
  await chrome.storage.local.remove("message");
  saved = null;
  render();
});

els.playCall.addEventListener("click", () => chrome.runtime.sendMessage({ sayit: "toggle-calls" }));
els.allowMic.addEventListener("click", () => chrome.tabs.create({ url: "options.html#mic" }));

// Mono, trimmed of the silence around it (so it starts the instant the
// hotkey is pressed), leveled to about -1 dBFS, stored as 24 kHz WAV.
async function processTake(blob) {
  const RATE = 24000;
  const decoded = await new OfflineAudioContext(1, 1, RATE).decodeAudioData(await blob.arrayBuffer());
  const n = decoded.length;
  const mono = new Float32Array(n);
  for (let c = 0; c < decoded.numberOfChannels; c++) {
    const d = decoded.getChannelData(c);
    for (let i = 0; i < n; i++) mono[i] += d[i] / decoded.numberOfChannels;
  }
  const out = trimAndLevel(mono, RATE);
  if (out.length < RATE * 0.3) throw new Error("it was too short — record again");
  return { wav: encodeWav(out, RATE), secs: out.length / RATE };
}

function trimAndLevel(s, rate) {
  const frame = Math.max(1, Math.round(rate / 50)); // 20 ms
  const rms = [];
  for (let i = 0; i < s.length; i += frame) {
    let acc = 0;
    const end = Math.min(s.length, i + frame);
    for (let j = i; j < end; j++) acc += s[j] * s[j];
    rms.push(Math.sqrt(acc / (end - i)));
  }
  const loudest = Math.max(0, ...rms);
  if (loudest <= 0) return s;
  const voiced = (r) => r >= loudest * 0.06;
  const first = rms.findIndex(voiced);
  let last = rms.length - 1;
  while (last > 0 && !voiced(rms[last])) last--;
  const pad = Math.ceil((rate * 0.12) / frame);
  const start = Math.max(0, first - pad) * frame;
  const end = Math.min(s.length, (last + 1 + pad) * frame);
  const out = s.slice(start, end);
  let peak = 0;
  for (const v of out) peak = Math.max(peak, Math.abs(v));
  if (peak > 1e-4) {
    const gain = Math.min(0.89 / peak, 8);
    for (let i = 0; i < out.length; i++) out[i] *= gain;
  }
  return out;
}

function encodeWav(samples, rate) {
  const buf = new ArrayBuffer(44 + samples.length * 2);
  const v = new DataView(buf);
  const str = (o, t) => [...t].forEach((c, i) => v.setUint8(o + i, c.charCodeAt(0)));
  str(0, "RIFF");
  v.setUint32(4, 36 + samples.length * 2, true);
  str(8, "WAVE");
  str(12, "fmt ");
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true); // PCM
  v.setUint16(22, 1, true); // mono
  v.setUint32(24, rate, true);
  v.setUint32(28, rate * 2, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  str(36, "data");
  v.setUint32(40, samples.length * 2, true);
  for (let i = 0; i < samples.length; i++) {
    v.setInt16(44 + i * 2, Math.max(-1, Math.min(1, samples[i])) * 0x7fff, true);
  }
  return new Uint8Array(buf);
}

function bytesToBase64(bytes) {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  }
  return btoa(bin);
}

// ---- Settings view ----
const hotkeyLabel = $("hotkeyLabel");
const changeHotkey = $("changeHotkey");
let capturing = false;

function renderSettings() {
  hotkeyLabel.textContent = sayitHotkeyLabel(settings.hotkey);
  $("muteMic").checked = settings.muteMic;
  $("hearIt").checked = settings.hearIt;
}

async function saveSettings(patch) {
  settings = { ...settings, ...patch };
  await chrome.storage.local.set({ settings });
  renderSettings();
  render();
}

changeHotkey.addEventListener("click", () => {
  capturing = true;
  hotkeyLabel.textContent = "Press keys… (Esc cancels)";
  changeHotkey.disabled = true;
});

document.addEventListener(
  "keydown",
  (e) => {
    if (!capturing) return;
    e.preventDefault();
    e.stopPropagation();
    if (["Control", "Alt", "Shift", "Meta"].includes(e.key)) return;
    capturing = false;
    changeHotkey.disabled = false;
    if (e.key === "Escape") return renderSettings();
    const isFKey = /^F\d{1,2}$/.test(e.code);
    if (!e.ctrlKey && !e.altKey && !e.metaKey && !isFKey) {
      renderSettings();
      $("hotkeyHint").textContent = "Use Ctrl, Alt or Win with a key (or an F-key), so normal typing never triggers it.";
      return;
    }
    saveSettings({
      hotkey: {
        code: e.code,
        key: e.key.length === 1 ? e.key.toUpperCase() : e.key,
        alt: e.altKey,
        ctrl: e.ctrlKey,
        shift: e.shiftKey,
        meta: e.metaKey,
      },
    });
  },
  true,
);

$("resetHotkey").addEventListener("click", () => saveSettings({ hotkey: SAYIT_DEFAULTS.hotkey }));
$("muteMic").addEventListener("change", (e) => saveSettings({ muteMic: e.target.checked }));
$("hearIt").addEventListener("change", (e) => saveSettings({ hearIt: e.target.checked }));
$("shortcuts").addEventListener("click", (e) => {
  e.preventDefault();
  chrome.tabs.create({ url: "chrome://extensions/shortcuts" });
});
$("openTest").addEventListener("click", () => chrome.tabs.create({ url: "options.html" }));

// ---- This tab, and the sites SayIt runs on ----
let tabOrigin = null; // the active tab's https origin, if any
let addedSites = []; // match patterns of the sites added below

async function loadSites() {
  const { origins = [] } = await chrome.permissions.getAll();
  addedSites = origins.filter((o) => !SAYIT_DIALERS.includes(o));
}

const coversTab = () => !!tabOrigin && (sayitBuiltIn(tabOrigin) || addedSites.includes(`${tabOrigin}/*`));

async function checkTab() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  try {
    const u = new URL((tab && tab.url) || "");
    if (u.protocol === "https:") tabOrigin = u.origin;
  } catch {}
  await loadSites();
  renderSites();
  let frames = [];
  if (coversTab()) {
    try {
      frames = await chrome.scripting.executeScript({
        target: { tabId: tab.id, allFrames: true },
        world: "MAIN",
        func: () => (window.__sayitStatus ? window.__sayitStatus() : null),
      });
    } catch {}
  }
  renderTab(frames.map((f) => f.result).filter(Boolean));
}

function renderTab(states) {
  let text = "";
  let tone = "";
  if (states.length) {
    const call = states.some((s) => s.call);
    if (states.some((s) => s.playing)) [text, tone] = ["This tab: playing your message", "ok"];
    else if (call && saved && !states.some((s) => s.ready))
      [text, tone] = ["This tab: call connected, but the message didn't load — reload the tab", "warn"];
    else if (call) [text, tone] = [`This tab: call connected — ${sayitHotkeyLabel(settings.hotkey)} plays it`, "ok"];
    else text = "This tab: SayIt is on · no call yet";
  } else if (coversTab()) {
    [text, tone] = ["This tab: reload it once to turn SayIt on", "warn"];
  } else if (tabOrigin) {
    text = "SayIt is off on this site — add it in Settings if your dialer is here";
  }
  const el = $("tabState");
  el.textContent = text;
  el.className = `tabstate ${tone}`;
  el.hidden = !text;
}

function renderSites() {
  $("siteList").replaceChildren(
    ...addedSites.map((pattern) => {
      const li = document.createElement("li");
      const name = document.createElement("span");
      name.textContent = sayitSiteName(pattern);
      const remove = document.createElement("button");
      remove.className = "small";
      remove.textContent = "Remove";
      remove.addEventListener("click", async () => {
        await chrome.permissions.remove({ origins: [pattern] });
        await loadSites();
        renderSites();
      });
      li.append(name, remove);
      return li;
    }),
  );
  const add = $("addSite");
  add.hidden = !tabOrigin || coversTab();
  if (tabOrigin) add.textContent = `+ Add ${new URL(tabOrigin).host}`;
}

$("addSite").addEventListener("click", () => {
  // Chrome asks to confirm; the background starts SayIt there once allowed.
  const host = new URL(tabOrigin).host;
  chrome.permissions
    .request({ origins: [`${tabOrigin}/*`] })
    .then(async (ok) => {
      if (!ok) return;
      await loadSites();
      renderSites();
      $("siteHint").textContent = `Added — reload ${host} once (not during a call).`;
    })
    .catch((e) => ($("siteHint").textContent = String(e)));
});

// ---- Tabs ----
function showView(name) {
  $("viewMessage").hidden = name !== "message";
  $("viewSettings").hidden = name !== "settings";
  $("tabMessage").classList.toggle("on", name === "message");
  $("tabSettings").classList.toggle("on", name === "settings");
}
$("tabMessage").addEventListener("click", () => showView("message"));
$("tabSettings").addEventListener("click", () => showView("settings"));

(async () => {
  settings = await sayitSettings();
  ({ message: saved = null } = await chrome.storage.local.get("message"));
  renderSettings();
  render();
  checkTab();
})();
