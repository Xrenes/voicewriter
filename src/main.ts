import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// ---- Types shared with the Rust backend ----
interface Settings {
  hotkey: string;
  secondaryHotkey: string;
  secondaryLanguage: string;
  speakHotkey: string;
  engine: "auto" | "groq" | "local";
  groqModel: string;
  insertion: "type" | "paste" | "clipboard" | "both";
  micDevice: string;
  language: string;
  model: string;
  polish: boolean;
  autostart: boolean;
}

type Status = "idle" | "recording" | "transcribing" | "ready" | "speaking" | "error";

interface StatusEvent {
  status: Status;
  detail?: string;
}

interface ModelState {
  present: boolean;
  path: string;
  sizeLabel: string;
}

interface DownloadProgress {
  received: number;
  total: number;
  done: boolean;
  error?: string;
}

interface KeyStatus {
  present: boolean;
  masked: string;
}

interface Usage {
  today: string;
  todayRequests: number;
  todayAudioSecs: number;
  totalRequests: number;
  totalAudioSecs: number;
  requestsPerMin: number;
  dailyPct: number;
  lastEngineUsed: string;
  lastError?: string | null;
  lastErrorAt?: string | null;
}

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

const statusDot = $("statusDot");
const statusText = $("statusText");
const hotkeyInput = $<HTMLInputElement>("hotkey");
const hotkeyEdit = $<HTMLButtonElement>("hotkeyEdit");
const hotkey2Input = $<HTMLInputElement>("hotkey2");
const hotkey2Edit = $<HTMLButtonElement>("hotkey2Edit");
const language2Input = $<HTMLInputElement>("language2");
const hotkeySpeakInput = $<HTMLInputElement>("hotkeySpeak");
const hotkeySpeakEdit = $<HTMLButtonElement>("hotkeySpeakEdit");
const speakKeyStatusEl = $("speakKeyStatus");
const speakKeyReplace = $<HTMLButtonElement>("speakKeyReplace");
const speakKeyTest = $<HTMLButtonElement>("speakKeyTest");
const speakKeyClear = $<HTMLButtonElement>("speakKeyClear");
const speakKeyEditRow = $("speakKeyEditRow");
const speakKeyInput = $<HTMLInputElement>("speakKeyInput");
const speakKeySave = $<HTMLButtonElement>("speakKeySave");
const speakKeyHint = $("speakKeyHint");
const keyStatusEl = $("keyStatus");
const keyReplace = $<HTMLButtonElement>("keyReplace");
const keyTest = $<HTMLButtonElement>("keyTest");
const keyClear = $<HTMLButtonElement>("keyClear");
const keyEditRow = $("keyEditRow");
const keyInput = $<HTMLInputElement>("keyInput");
const keySave = $<HTMLButtonElement>("keySave");
const keyHint = $("keyHint");
const uToday = $("uToday");
const uRpm = $("uRpm");
const meterPct = $("meterPct");
const meterFill = $<HTMLDivElement>("meterFill");
const engineSel = $<HTMLSelectElement>("engine");
const groqModelSel = $<HTMLSelectElement>("groqModel");
const micSel = $<HTMLSelectElement>("micDevice");
const langInput = $<HTMLInputElement>("language");
const modelSel = $<HTMLSelectElement>("model");
const modelState = $("modelState");
const modelDownload = $<HTMLButtonElement>("modelDownload");
const progressWrap = $("progressWrap");
const progressBar = $<HTMLDivElement>("progressBar");
const speakStatusRow = $("speakStatusRow");
const speakStatusText = $("speakStatusText");
const speakProgressWrap = $("speakProgressWrap");
const speakProgressBar = $<HTMLDivElement>("speakProgressBar");
const insertionSel = $<HTMLSelectElement>("insertion");
const polishChk = $<HTMLInputElement>("polish");
const autostartChk = $<HTMLInputElement>("autostart");

let settings: Settings;
let capturingHotkey = false;
let usageTimer: number | undefined;

// ---- Status ----
function applyStatus(s: Status, detail?: string) {
  statusDot.className = "dot " + s;
  const labels: Record<Status, string> = {
    idle: "Idle",
    recording: "Listening…",
    transcribing: "Transcribing…",
    ready: "Ready",
    speaking: "Speaking…",
    error: "Error",
  };
  statusText.textContent = detail ? `${labels[s]} — ${detail}` : labels[s];
  if (s === "idle" || s === "error") refreshUsage();
  applySpeakProgress(s, detail);
}

// ---- Speak-selection request status bar ----
// Two phases: a fixed-width fill while capturing the selection / requesting
// speech from Groq (duration unknown up front), then a real percentage of
// actual playback position once audio starts ("playing… N%" from the backend).
const SPEAK_STAGE_PCT: Record<string, number> = {
  "capturing selection…": 20,
  "requesting speech…": 45,
};

function applySpeakProgress(s: Status, detail?: string) {
  if (s === "speaking" && detail) {
    speakStatusRow.hidden = false;
    speakProgressWrap.hidden = false;
    speakProgressBar.classList.remove("error");

    const playingMatch = /^playing… (\d+)%$/.exec(detail);
    if (playingMatch) {
      speakStatusText.textContent = `Playing… ${playingMatch[1]}%`;
      speakProgressBar.style.width = playingMatch[1] + "%";
    } else {
      speakStatusText.textContent = detail;
      speakProgressBar.style.width = (SPEAK_STAGE_PCT[detail] ?? 50) + "%";
    }
    return;
  }
  if (s === "error" && speakProgressWrap.hidden === false) {
    speakStatusText.textContent = "Error";
    speakProgressBar.classList.add("error");
    return;
  }
  // idle/ready/other: hide once any in-flight speak attempt has resolved.
  speakStatusRow.hidden = true;
  speakProgressWrap.hidden = true;
  speakProgressBar.style.width = "0%";
  speakProgressBar.classList.remove("error");
}

// ---- Settings ----
async function loadSettings() {
  settings = await invoke<Settings>("get_settings");
  hotkeyInput.value = settings.hotkey;
  hotkey2Input.value = settings.secondaryHotkey || "(disabled)";
  language2Input.value = settings.secondaryLanguage;
  hotkeySpeakInput.value = settings.speakHotkey || "(disabled)";
  engineSel.value = settings.engine;
  groqModelSel.value = settings.groqModel;
  langInput.value = settings.language;
  modelSel.value = settings.model;
  insertionSel.value = settings.insertion;
  polishChk.checked = settings.polish;
  autostartChk.checked = settings.autostart;
}

async function save(patch: Partial<Settings>) {
  settings = { ...settings, ...patch };
  await invoke("update_settings", { settings });
}

// ---- Groq key ----
async function refreshKey() {
  const st = await invoke<KeyStatus>("groq_key_status");
  if (st.present) {
    keyStatusEl.textContent = st.masked;
    keyStatusEl.classList.add("ok");
    keyClear.hidden = false;
    keyTest.hidden = false;
    keyHint.textContent = "Dictation uses Groq. Remove the key to switch to local only.";
  } else {
    keyStatusEl.textContent = "No key — using local model";
    keyStatusEl.classList.remove("ok");
    keyClear.hidden = true;
    keyTest.hidden = true;
    keyHint.textContent =
      "Paste a Groq key (console.groq.com) for best accuracy. Without one, the local model is used.";
  }
}

keyReplace.addEventListener("click", () => {
  keyEditRow.hidden = !keyEditRow.hidden;
  if (!keyEditRow.hidden) keyInput.focus();
});

keySave.addEventListener("click", async () => {
  const v = keyInput.value.trim();
  if (!v) return;
  keySave.disabled = true;
  try {
    await invoke("set_groq_key", { key: v });
    keyInput.value = "";
    keyEditRow.hidden = true;
    await refreshKey();
  } catch (e) {
    keyHint.textContent = String(e);
  } finally {
    keySave.disabled = false;
  }
});

keyClear.addEventListener("click", async () => {
  await invoke("clear_groq_key");
  await refreshKey();
});

keyTest.addEventListener("click", async () => {
  keyTest.disabled = true;
  keyStatusEl.textContent = "Testing…";
  try {
    const r = await invoke<string>("test_groq_key");
    keyStatusEl.textContent = r;
  } catch (e) {
    keyStatusEl.textContent = "Test failed: " + String(e);
  } finally {
    keyTest.disabled = false;
    setTimeout(refreshKey, 2500);
  }
});

// ---- Speak-aloud key (separate from the dictation key above) ----
async function refreshSpeakKey() {
  const st = await invoke<KeyStatus>("speak_key_status");
  if (st.present) {
    speakKeyStatusEl.textContent = st.masked;
    speakKeyStatusEl.classList.add("ok");
    speakKeyClear.hidden = false;
    speakKeyTest.hidden = false;
    speakKeyHint.textContent = "Speak-aloud uses this key. Remove it to disable the feature.";
  } else {
    speakKeyStatusEl.textContent = "No key — speak-aloud disabled";
    speakKeyStatusEl.classList.remove("ok");
    speakKeyClear.hidden = true;
    speakKeyTest.hidden = true;
    speakKeyHint.textContent =
      "Separate Groq key used only for \"speak selected text aloud.\" Required for that feature.";
  }
}

speakKeyReplace.addEventListener("click", () => {
  speakKeyEditRow.hidden = !speakKeyEditRow.hidden;
  if (!speakKeyEditRow.hidden) speakKeyInput.focus();
});

speakKeySave.addEventListener("click", async () => {
  const v = speakKeyInput.value.trim();
  if (!v) return;
  speakKeySave.disabled = true;
  try {
    await invoke("set_speak_key", { key: v });
    speakKeyInput.value = "";
    speakKeyEditRow.hidden = true;
    await refreshSpeakKey();
  } catch (e) {
    speakKeyHint.textContent = String(e);
  } finally {
    speakKeySave.disabled = false;
  }
});

speakKeyClear.addEventListener("click", async () => {
  await invoke("clear_speak_key");
  await refreshSpeakKey();
});

speakKeyTest.addEventListener("click", async () => {
  speakKeyTest.disabled = true;
  speakKeyStatusEl.textContent = "Testing…";
  try {
    const r = await invoke<string>("test_speak_key");
    speakKeyStatusEl.textContent = r;
  } catch (e) {
    speakKeyStatusEl.textContent = "Test failed: " + String(e);
  } finally {
    speakKeyTest.disabled = false;
    setTimeout(refreshSpeakKey, 2500);
  }
});

// ---- Usage ----
async function refreshUsage() {
  try {
    const u = await invoke<Usage>("get_usage");
    uToday.textContent = String(u.todayRequests);
    uRpm.textContent = String(u.requestsPerMin);

    const pct = Math.round(u.dailyPct || 0);
    meterPct.textContent = pct + "%";
    meterFill.style.width = Math.max(pct, 1.5) + "%";
    meterFill.classList.toggle("warn", pct >= 60 && pct < 80);
    meterFill.classList.toggle("crit", pct >= 80);
  } catch (e) {
    console.error("get_usage failed", e);
  }
}

// ---- Mic list ----
async function loadMics() {
  try {
    const devices = await invoke<string[]>("list_input_devices");
    for (const d of devices) {
      const opt = document.createElement("option");
      opt.value = d;
      opt.textContent = d;
      micSel.appendChild(opt);
    }
    micSel.value = settings.micDevice;
  } catch (e) {
    console.error("list_input_devices failed", e);
  }
}

// ---- Local model ----
async function refreshModelState() {
  const st = await invoke<ModelState>("model_state", { model: modelSel.value });
  if (st.present) {
    modelState.textContent = `Installed (${st.sizeLabel})`;
    modelDownload.hidden = true;
  } else {
    modelState.textContent = `Not downloaded (${st.sizeLabel})`;
    modelDownload.hidden = false;
  }
}

modelDownload.addEventListener("click", async () => {
  modelDownload.disabled = true;
  progressWrap.hidden = false;
  progressBar.style.width = "0%";
  try {
    await invoke("download_model", { model: modelSel.value });
  } catch (e) {
    modelState.textContent = "Download failed: " + String(e);
  } finally {
    modelDownload.disabled = false;
  }
});

// ---- Field bindings ----
engineSel.addEventListener("change", () =>
  save({ engine: engineSel.value as Settings["engine"] }),
);
groqModelSel.addEventListener("change", () => save({ groqModel: groqModelSel.value }));
micSel.addEventListener("change", () => save({ micDevice: micSel.value }));
langInput.addEventListener("change", () => save({ language: langInput.value.trim() || "en" }));
modelSel.addEventListener("change", async () => {
  await save({ model: modelSel.value });
  await refreshModelState();
});
insertionSel.addEventListener("change", () => save({ insertion: insertionSel.value as Settings["insertion"] }));
polishChk.addEventListener("change", () => save({ polish: polishChk.checked }));
autostartChk.addEventListener("change", async () => {
  await save({ autostart: autostartChk.checked });
  await invoke("set_autostart", { enabled: autostartChk.checked });
});

// ---- Hotkey capture (shared by both hotkey fields) ----
interface HotkeyBinding {
  input: HTMLInputElement;
  editBtn: HTMLButtonElement;
  command: "set_hotkey" | "set_secondary_hotkey" | "set_speak_hotkey";
  get: () => string;
  set: (v: string) => void;
}

function bindHotkey(b: HotkeyBinding) {
  b.editBtn.addEventListener("click", () => {
    if (capturingHotkey) return;
    capturingHotkey = true;
    b.input.value = "Press keys… (Esc to clear)";
    b.editBtn.textContent = "…";

    const onKey = (ev: KeyboardEvent) => {
      ev.preventDefault();
      ev.stopPropagation();
      if (["Control", "Shift", "Alt", "Meta"].includes(ev.key)) return;

      window.removeEventListener("keydown", onKey, true);
      capturingHotkey = false;
      b.editBtn.textContent = "Change";

      // Esc clears the binding (allowed for the secondary hotkey).
      const combo =
        ev.key === "Escape"
          ? ""
          : (() => {
              const parts: string[] = [];
              if (ev.ctrlKey) parts.push("Control");
              if (ev.shiftKey) parts.push("Shift");
              if (ev.altKey) parts.push("Alt");
              if (ev.metaKey) parts.push("Super");
              parts.push(ev.key.length === 1 ? ev.key.toUpperCase() : ev.key);
              return parts.join("+");
            })();

      invoke<boolean>(b.command, { hotkey: combo })
        .then((ok) => {
          if (ok) {
            b.input.value = combo || "(disabled)";
            b.set(combo);
          } else {
            b.input.value = b.get() || "(disabled)";
            applyStatus("error", "hotkey in use");
          }
        })
        .catch(() => {
          b.input.value = b.get() || "(disabled)";
        });
    };
    window.addEventListener("keydown", onKey, true);
  });
}

bindHotkey({
  input: hotkeyInput,
  editBtn: hotkeyEdit,
  command: "set_hotkey",
  get: () => settings.hotkey,
  set: (v) => (settings.hotkey = v),
});
bindHotkey({
  input: hotkey2Input,
  editBtn: hotkey2Edit,
  command: "set_secondary_hotkey",
  get: () => settings.secondaryHotkey,
  set: (v) => (settings.secondaryHotkey = v),
});
bindHotkey({
  input: hotkeySpeakInput,
  editBtn: hotkeySpeakEdit,
  command: "set_speak_hotkey",
  get: () => settings.speakHotkey,
  set: (v) => (settings.speakHotkey = v),
});

language2Input.addEventListener("change", () =>
  save({ secondaryLanguage: language2Input.value.trim() || "bn" }),
);

// Note: click-outside-to-hide is handled in the Rust backend via the window's
// Focused(false) event, so it works even when focus goes to a native surface.

// ---- Boot ----
async function boot() {
  await listen<StatusEvent>("status", (e) =>
    applyStatus(e.payload.status, e.payload.detail),
  );
  await listen<DownloadProgress>("model-progress", (e) => {
    const p = e.payload;
    if (p.error) {
      modelState.textContent = "Download failed: " + p.error;
      progressWrap.hidden = true;
      return;
    }
    const pct = p.total > 0 ? Math.round((p.received / p.total) * 100) : 0;
    progressBar.style.width = pct + "%";
    modelState.textContent = `Downloading… ${pct}%`;
    if (p.done) {
      progressWrap.hidden = true;
      refreshModelState();
    }
  });

  await loadSettings();
  await loadMics();
  await refreshModelState();
  await refreshKey();
  await refreshSpeakKey();
  await refreshUsage();
  applyStatus("idle");
  invoke("ui_ready").catch(() => {});

  // Refresh usage while the window is open.
  usageTimer = window.setInterval(refreshUsage, 2000);
  window.addEventListener("beforeunload", () => {
    if (usageTimer) clearInterval(usageTimer);
  });
}

boot().catch((e) => console.error("boot failed", e));
