import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";

// ---- Types shared with the Rust backend ----
interface Settings {
  hotkey: string;
  secondaryHotkey: string;
  secondaryLanguage: string;
  speakHotkey: string;
  wheelHotkey: string;
  webHotkey: string;
  engine: "auto" | "groq" | "local";
  groqModel: string;
  insertion: "type" | "paste" | "clipboard" | "both";
  micDevice: string;
  loopbackDevice: string;
  language: string;
  model: string;
  polish: boolean;
  autostart: boolean;
  hideTray: boolean;
  ttsVoice: string;
  ttsSpeed: number;
  visionModel: string;
}

type Status = "idle" | "recording" | "transcribing" | "ready" | "speaking" | "error";

interface StatusEvent {
  status: Status;
  detail?: string;
}

interface ModelInfo {
  id: string;
  kind: "stt" | "tts";
  label: string;
  present: boolean;
  sizeLabel: string;
  bytesOnDisk: number;
}

interface DownloadProgress {
  model: string;
  received: number;
  total: number;
  done: boolean;
  error?: string;
}

interface KeyStatus {
  present: boolean;
  masked: string;
}

interface UsageSnapshot {
  purpose: string;
  periodLabel: string;
  periodValue: number;
  periodCap: number;
  pct: number;
  requestsPerMin: number;
  totalRequests: number;
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
const hotkeyWheelInput = $<HTMLInputElement>("hotkeyWheel");
const hotkeyWheelEdit = $<HTMLButtonElement>("hotkeyWheelEdit");
const hotkeyWebInput = $<HTMLInputElement>("hotkeyWeb");
const hotkeyWebEdit = $<HTMLButtonElement>("hotkeyWebEdit");
const visionKeyStatusEl = $("visionKeyStatus");
const visionKeyReplace = $<HTMLButtonElement>("visionKeyReplace");
const visionKeyTest = $<HTMLButtonElement>("visionKeyTest");
const visionKeyClear = $<HTMLButtonElement>("visionKeyClear");
const visionKeyEditRow = $("visionKeyEditRow");
const visionKeyInput = $<HTMLInputElement>("visionKeyInput");
const visionKeySave = $<HTMLButtonElement>("visionKeySave");
const visionKeyHint = $("visionKeyHint");
const keyStatusEl = $("keyStatus");
const keyReplace = $<HTMLButtonElement>("keyReplace");
const keyTest = $<HTMLButtonElement>("keyTest");
const keyClear = $<HTMLButtonElement>("keyClear");
const keyEditRow = $("keyEditRow");
const keyInput = $<HTMLInputElement>("keyInput");
const keySave = $<HTMLButtonElement>("keySave");
const keyHint = $("keyHint");
const dictUToday = $("dictUToday");
const dictURpm = $("dictURpm");
const dictMeterPct = $("dictMeterPct");
const dictMeterFill = $<HTMLDivElement>("dictMeterFill");
const visionUToday = $("visionUToday");
const visionURpm = $("visionURpm");
const visionMeterPct = $("visionMeterPct");
const visionMeterFill = $<HTMLDivElement>("visionMeterFill");
const engineSel = $<HTMLSelectElement>("engine");
const groqModelSel = $<HTMLSelectElement>("groqModel");
const micSel = $<HTMLSelectElement>("micDevice");
const loopbackSel = $<HTMLSelectElement>("loopbackDevice");
const langInput = $<HTMLInputElement>("language");
const modelSel = $<HTMLSelectElement>("model");
const speakStatusRow = $("speakStatusRow");
const speakStatusText = $("speakStatusText");
const speakProgressWrap = $("speakProgressWrap");
const speakProgressBar = $<HTMLDivElement>("speakProgressBar");
const insertionSel = $<HTMLSelectElement>("insertion");
const polishChk = $<HTMLInputElement>("polish");
const autostartChk = $<HTMLInputElement>("autostart");
const hideTrayChk = $<HTMLInputElement>("hideTray");

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
  hotkeyWheelInput.value = settings.wheelHotkey || "(disabled)";
  hotkeyWebInput.value = settings.webHotkey || "(disabled)";
  engineSel.value = settings.engine;
  groqModelSel.value = settings.groqModel;
  langInput.value = settings.language;
  modelSel.value = settings.model;
  insertionSel.value = settings.insertion;
  polishChk.checked = settings.polish;
  autostartChk.checked = settings.autostart;
  hideTrayChk.checked = settings.hideTray;
  ttsSpeedInput.value = String(settings.ttsSpeed || 1.0);
  ttsSpeedVal.textContent = Number(ttsSpeedInput.value).toFixed(1) + "×";
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

// Vision model picker: the list is fetched live from the account's own
// available models (not hardcoded — Groq's vision model lineup has shifted
// more than once, deprecating/renaming models within a week of each other).
// Loads once a key is saved; selecting an option persists it as the model
// AI chat actually sends requests to.
const visionModelSelect = $<HTMLSelectElement>("visionModelSelect");
const visionModelHint = $("visionModelHint");

async function loadVisionModels(preferSelected?: string) {
  visionModelSelect.disabled = true;
  const loadingOpt = document.createElement("option");
  loadingOpt.textContent = "Loading models…";
  visionModelSelect.replaceChildren(loadingOpt);
  try {
    const listing = await invoke<string>("debug_list_groq_models");
    const models = listing.split("\n").map((m) => m.trim()).filter(Boolean);
    if (models.length === 0) {
      const opt = document.createElement("option");
      opt.textContent = "No models returned";
      visionModelSelect.replaceChildren(opt);
      return;
    }
    const selected = preferSelected ?? settings.visionModel;
    const opts = models.map((m) => {
      const opt = document.createElement("option");
      opt.value = m;
      opt.textContent = m;
      if (m === selected) opt.selected = true;
      return opt;
    });
    visionModelSelect.replaceChildren(...opts);
    visionModelHint.textContent = `${models.length} models available to this key.`;
    visionModelSelect.disabled = false;
  } catch (e) {
    const opt = document.createElement("option");
    opt.textContent = "Couldn't load models";
    visionModelSelect.replaceChildren(opt);
    visionModelHint.textContent = "Failed: " + String(e);
  }
}

visionModelSelect.addEventListener("change", () => {
  save({ visionModel: visionModelSelect.value });
});

async function refreshVisionKey() {
  const st = await invoke<KeyStatus>("vision_key_status");
  if (st.present) {
    visionKeyStatusEl.textContent = st.masked;
    visionKeyStatusEl.classList.add("ok");
    visionKeyClear.hidden = false;
    visionKeyTest.hidden = false;
    visionKeyHint.textContent =
      "The wheel's \"AI\" chat wedge uses this key. Same Groq account as Dictation works fine.";
    await loadVisionModels();
  } else {
    visionKeyStatusEl.textContent = "No key — AI chat disabled";
    visionKeyStatusEl.classList.remove("ok");
    visionKeyClear.hidden = true;
    visionKeyTest.hidden = true;
    visionKeyHint.textContent =
      "Separate Groq key used only for the wheel's \"AI\" chat wedge. You can paste the " +
      "same key as Dictation above, or use a different Groq account/key.";
    const opt = document.createElement("option");
    opt.textContent = "Save a key below to load models…";
    visionModelSelect.replaceChildren(opt);
    visionModelSelect.disabled = true;
    visionModelHint.textContent = "";
  }
}

visionKeyReplace.addEventListener("click", () => {
  visionKeyEditRow.hidden = !visionKeyEditRow.hidden;
  if (!visionKeyEditRow.hidden) visionKeyInput.focus();
});

visionKeySave.addEventListener("click", async () => {
  const v = visionKeyInput.value.trim();
  if (!v) return;
  visionKeySave.disabled = true;
  try {
    await invoke("set_vision_key", { key: v });
    visionKeyInput.value = "";
    visionKeyEditRow.hidden = true;
    await refreshVisionKey();
  } catch (e) {
    visionKeyHint.textContent = String(e);
  } finally {
    visionKeySave.disabled = false;
  }
});

visionKeyClear.addEventListener("click", async () => {
  await invoke("clear_vision_key");
  await refreshVisionKey();
});

visionKeyTest.addEventListener("click", async () => {
  visionKeyTest.disabled = true;
  visionKeyStatusEl.textContent = "Testing…";
  try {
    const r = await invoke<string>("test_vision_key");
    visionKeyStatusEl.textContent = r;
  } catch (e) {
    visionKeyStatusEl.textContent = "Test failed: " + String(e);
  } finally {
    visionKeyTest.disabled = false;
    setTimeout(refreshVisionKey, 2500);
  }
});

const resetCapturePermissionBtn = $<HTMLButtonElement>("resetCapturePermission");
resetCapturePermissionBtn.addEventListener("click", async () => {
  resetCapturePermissionBtn.disabled = true;
  try {
    await invoke("reset_capture_permission");
    resetCapturePermissionBtn.textContent = "Will ask again";
  } catch {
    resetCapturePermissionBtn.textContent = "Failed";
  } finally {
    setTimeout(() => {
      resetCapturePermissionBtn.textContent = "Ask again next time";
      resetCapturePermissionBtn.disabled = false;
    }, 1800);
  }
});

// ---- Usage (one meter per API key, since each has its own quota/unit) ----
function applyUsageMeter(
  u: UsageSnapshot,
  valueEl: HTMLElement,
  labelEl: HTMLElement | null,
  rpmEl: HTMLElement,
  pctEl: HTMLElement,
  fillEl: HTMLDivElement,
) {
  valueEl.textContent = Math.round(u.periodValue).toLocaleString();
  if (labelEl) labelEl.textContent = u.periodLabel;
  rpmEl.textContent = String(u.requestsPerMin);

  const pct = Math.round(u.pct || 0);
  pctEl.textContent = pct + "%";
  fillEl.style.width = Math.max(pct, 1.5) + "%";
  fillEl.classList.toggle("warn", pct >= 60 && pct < 80);
  fillEl.classList.toggle("crit", pct >= 80);
}

const dictULabel = $("dictULabel");

async function refreshUsage() {
  try {
    const [dict, vision] = await Promise.all([
      invoke<UsageSnapshot>("get_usage_dictation"),
      invoke<UsageSnapshot>("get_usage_vision"),
    ]);
    applyUsageMeter(dict, dictUToday, dictULabel, dictURpm, dictMeterPct, dictMeterFill);
    applyUsageMeter(vision, visionUToday, null, visionURpm, visionMeterPct, visionMeterFill);
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

// ---- System-audio (loopback) output-device list, for call recording ----
async function loadLoopbackDevices() {
  try {
    const devices = await invoke<string[]>("list_output_devices");
    for (const d of devices) {
      const opt = document.createElement("option");
      opt.value = d;
      opt.textContent = d;
      loopbackSel.appendChild(opt);
    }
    loopbackSel.value = settings.loopbackDevice;
  } catch (e) {
    console.error("list_output_devices failed", e);
  }
}

// ---- Voice Models (STT + TTS) ----
const voiceModelsTotalSizeEl = $("voiceModelsTotalSize");
const sttModelListEl = $("sttModelList");
const ttsModelListEl = $("ttsModelList");
const ttsVoiceRow = $("ttsVoiceRow");
const ttsVoiceSel = $<HTMLSelectElement>("ttsVoice");
const ttsSpeedRow = $("ttsSpeedRow");
const ttsSpeedInput = $<HTMLInputElement>("ttsSpeed");
const ttsSpeedVal = $("ttsSpeedVal");
const ttsPreviewRow = $("ttsPreviewRow");
const ttsPreviewBtn = $<HTMLButtonElement>("ttsPreview");

const modelDownloadButtons = new Map<string, HTMLButtonElement>();
const modelProgressBars = new Map<string, HTMLDivElement>();

function renderModelRow(m: ModelInfo): HTMLElement {
  const row = document.createElement("div");
  row.className = "voice-model-row" + (m.present ? " active" : "");

  const top = document.createElement("div");
  top.className = "row";
  const name = document.createElement("span");
  name.className = "voice-model-name";
  name.textContent = m.label;
  const size = document.createElement("span");
  size.className = "voice-model-size";
  size.textContent = m.sizeLabel;
  name.appendChild(size);
  top.appendChild(name);

  const actions = document.createElement("div");
  actions.className = "row key-actions";
  if (m.present) {
    const del = document.createElement("button");
    del.className = "ghost danger";
    del.textContent = "Delete";
    del.addEventListener("click", async () => {
      del.disabled = true;
      try {
        await invoke("delete_voice_model", { model: m.id });
        await refreshVoiceModels();
      } catch (e) {
        console.error("delete_voice_model failed", e);
      } finally {
        del.disabled = false;
      }
    });
    actions.appendChild(del);
  } else {
    const dl = document.createElement("button");
    dl.className = "primary";
    dl.textContent = "Download";
    dl.addEventListener("click", async () => {
      dl.disabled = true;
      const bar = modelProgressBars.get(m.id);
      const wrap = bar?.parentElement;
      if (wrap) wrap.hidden = false;
      if (bar) bar.style.width = "0%";
      try {
        await invoke("download_voice_model", { model: m.id });
        await refreshVoiceModels();
      } catch (e) {
        dl.disabled = false;
        console.error("download_voice_model failed", e);
      }
    });
    modelDownloadButtons.set(m.id, dl);
    actions.appendChild(dl);
  }
  top.appendChild(actions);
  row.appendChild(top);

  const progressWrap = document.createElement("div");
  progressWrap.className = "progress voice-model-progress";
  progressWrap.hidden = true;
  const progressBar = document.createElement("div");
  progressBar.className = "progress-bar";
  progressWrap.appendChild(progressBar);
  modelProgressBars.set(m.id, progressBar);
  row.appendChild(progressWrap);

  return row;
}

async function refreshVoiceModels() {
  try {
    const models = await invoke<ModelInfo[]>("list_voice_models");
    const stt = models.filter((m) => m.kind === "stt");
    const tts = models.filter((m) => m.kind === "tts");

    sttModelListEl.replaceChildren(...stt.map(renderModelRow));
    ttsModelListEl.replaceChildren(...tts.map(renderModelRow));

    const ttsReady = tts.some((m) => m.present);
    ttsVoiceRow.hidden = !ttsReady;
    ttsSpeedRow.hidden = !ttsReady;
    ttsPreviewRow.hidden = !ttsReady;

    const totalSize = await invoke<string>("voice_models_total_size");
    voiceModelsTotalSizeEl.textContent = totalSize;
  } catch (e) {
    console.error("refreshVoiceModels failed", e);
  }
}

async function loadTtsVoices() {
  try {
    const voices = await invoke<[string, string][]>("list_tts_voices");
    const opts = voices.map(([id, label]) => {
      const opt = document.createElement("option");
      opt.value = id;
      opt.textContent = label;
      if (id === settings.ttsVoice) opt.selected = true;
      return opt;
    });
    ttsVoiceSel.replaceChildren(...opts);
  } catch (e) {
    console.error("list_tts_voices failed", e);
  }
}

ttsVoiceSel.addEventListener("change", () => save({ ttsVoice: ttsVoiceSel.value }));

ttsSpeedInput.addEventListener("input", () => {
  ttsSpeedVal.textContent = Number(ttsSpeedInput.value).toFixed(1) + "×";
});
ttsSpeedInput.addEventListener("change", () =>
  save({ ttsSpeed: Number(ttsSpeedInput.value) }),
);

ttsPreviewBtn.addEventListener("click", async () => {
  ttsPreviewBtn.disabled = true;
  try {
    await invoke("test_tts_voice", { voice: ttsVoiceSel.value || settings.ttsVoice });
  } catch (e) {
    console.error("test_tts_voice failed", e);
  } finally {
    ttsPreviewBtn.disabled = false;
  }
});

modelSel.addEventListener("change", () => save({ model: modelSel.value }));

listen<DownloadProgress>("model-progress", (e) => {
  const p = e.payload;
  const bar = modelProgressBars.get(p.model);
  const btn = modelDownloadButtons.get(p.model);
  if (p.error) {
    if (bar) bar.classList.add("error");
    if (btn) btn.disabled = false;
    console.error(`${p.model} download failed:`, p.error);
    return;
  }
  const pct = p.total > 0 ? Math.round((p.received / p.total) * 100) : 0;
  if (bar) bar.style.width = pct + "%";
  if (p.done) {
    refreshVoiceModels();
  }
}).catch(() => {});

// ---- Field bindings ----
engineSel.addEventListener("change", () =>
  save({ engine: engineSel.value as Settings["engine"] }),
);
groqModelSel.addEventListener("change", () => save({ groqModel: groqModelSel.value }));
micSel.addEventListener("change", () => save({ micDevice: micSel.value }));
loopbackSel.addEventListener("change", () => save({ loopbackDevice: loopbackSel.value }));
langInput.addEventListener("change", () => save({ language: langInput.value.trim() || "en" }));
insertionSel.addEventListener("change", () => save({ insertion: insertionSel.value as Settings["insertion"] }));
polishChk.addEventListener("change", () => save({ polish: polishChk.checked }));
// Saved by the backend command itself (not save()), so the frontend's cached
// settings object can't later overwrite it with a stale value.
hideTrayChk.addEventListener("change", async () => {
  await invoke("set_tray_visible", { visible: !hideTrayChk.checked });
  settings.hideTray = hideTrayChk.checked;
});

autostartChk.addEventListener("change", async () => {
  await save({ autostart: autostartChk.checked });
  await invoke("set_autostart", { enabled: autostartChk.checked });
});

// ---- Hotkey capture (shared by both hotkey fields) ----
interface HotkeyBinding {
  input: HTMLInputElement;
  editBtn: HTMLButtonElement;
  command: "set_hotkey" | "set_secondary_hotkey" | "set_speak_hotkey" | "set_wheel_hotkey" | "set_web_hotkey";
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
bindHotkey({
  input: hotkeyWheelInput,
  editBtn: hotkeyWheelEdit,
  command: "set_wheel_hotkey",
  get: () => settings.wheelHotkey,
  set: (v) => (settings.wheelHotkey = v),
});
bindHotkey({
  input: hotkeyWebInput,
  editBtn: hotkeyWebEdit,
  command: "set_web_hotkey",
  get: () => settings.webHotkey,
  set: (v) => (settings.webHotkey = v),
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

  await loadSettings();
  await loadMics();
  await loadLoopbackDevices();
  await refreshVoiceModels();
  await loadTtsVoices();
  await refreshKey();
  await refreshVisionKey();
  await refreshUsage();
  applyStatus("idle");
  invoke("ui_ready").catch(() => {});

  // Refresh usage while the window is open.
  usageTimer = window.setInterval(refreshUsage, 2000);
  window.addEventListener("beforeunload", () => {
    if (usageTimer) clearInterval(usageTimer);
  });
}

// Linux: explain how hotkeys work for the current display server. X11
// supports app-registered global hotkeys like Windows does; Wayland doesn't,
// so the app's command-line actions are bound as desktop shortcuts instead.
async function showPlatformNotes() {
  try {
    const info = await invoke<{ os: string; session: string }>("platform_info");
    if (info.os !== "linux") return;
    const note = document.getElementById("linuxHotkeyNote") as HTMLElement;
    const label = document.getElementById("linuxSessionLabel") as HTMLElement;
    const text = document.getElementById("linuxSessionText") as HTMLElement;
    const cli = document.getElementById("linuxCliList") as HTMLElement;
    note.hidden = false;
    cli.hidden = false;
    if (info.session === "wayland") {
      label.textContent = "Linux · Wayland";
      text.textContent =
        "Wayland doesn't let apps register global hotkeys, so the keys below only work while an X11 app is focused. Bind VoiceWriter's commands as desktop shortcuts instead:";
    } else {
      label.textContent = "Linux · X11";
      text.textContent =
        "The hotkeys below work everywhere in an X11 session. You can also bind VoiceWriter's commands as desktop shortcuts:";
    }
  } catch {
    // not fatal — the note just stays hidden
  }
}
showPlatformNotes();

// Guide section links open in the system's real default browser, not
// VoiceWriter's own in-app Web browser — these point at actual account
// sign-up/API-key pages, which is a different purpose than the Web wedge.
document.addEventListener("click", (ev) => {
  const link = (ev.target as HTMLElement).closest<HTMLElement>(".ext-link");
  if (!link) return;
  ev.preventDefault();
  const url = link.dataset.url;
  if (url) openUrl(url).catch(() => {});
});

boot().catch((e) => console.error("boot failed", e));
