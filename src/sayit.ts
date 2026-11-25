import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface Route {
  toCall: boolean;
  device: string;
  mic: string;
  error?: string | null;
}

interface Message {
  id: string;
  name: string;
  secs: number;
  added: number;
}

interface Status {
  messages: Message[];
  /** The message the hotkey plays. */
  active?: string | null;
  takeSecs?: number | null;
  takeName: string;
  recording: boolean;
  playing: boolean;
  route: Route;
  playHotkey: string;
}

/** Uploads are decoded at this rate: plenty for a call, and small. */
const UPLOAD_RATE = 24000;
const MAX_UPLOAD_MB = 50;
const MAX_UPLOAD_SECS = 600;

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const listEl = $<HTMLUListElement>("lib");
const countText = $("countText");
const emptyText = $("emptyText");
const recordBtn = $<HTMLButtonElement>("recordBtn");
const uploadBtn = $<HTMLButtonElement>("uploadBtn");
const fileInput = $<HTMLInputElement>("fileInput");
const takeBox = $("takeBox");
const stateText = $("stateText");
const takeRow = $("takeRow");
const takeName = $<HTMLInputElement>("takeName");
const takeListen = $<HTMLButtonElement>("takeListen");
const saveBtn = $<HTMLButtonElement>("saveBtn");
const discardBtn = $<HTMLButtonElement>("discardBtn");
const playBtn = $<HTMLButtonElement>("playBtn");
const routeText = $("routeText");
const hintText = $("hintText");
const noteText = $("noteText");
const errorText = $("errorText");
const player = $<HTMLAudioElement>("player");

let status: Status | null = null;
let recordStarted = 0;
let timer: number | undefined;
let blobUrl: string | null = null;
/** What the player is playing: a message id, or "take" for the new recording. */
let listening: string | null = null;
/** The message whose name is being edited. */
let renaming: string | null = null;
/** The message whose delete button asks "Delete?" (a second click deletes). */
let armedDelete: string | null = null;
let armTimer: number | undefined;
/** Files are being added. */
let uploading = false;
let hadTake = false;
/** The list is being rebuilt (removing a focused rename box blurs it). */
let rebuilding = false;

const clock = (secs: number) => {
  const t = Math.max(0, Math.round(secs));
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
};
const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

function showError(e: unknown) {
  errorText.textContent = e ? errText(e) : "";
}
function showNote(text: string) {
  noteText.textContent = text;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, className: string, text?: string) {
  const node = document.createElement(tag);
  node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

function iconButton(text: string, title: string, onClick: () => void) {
  const b = el("button", "icon", text);
  b.title = title;
  b.setAttribute("aria-label", title);
  b.addEventListener("click", (e) => {
    e.stopPropagation();
    onClick();
  });
  return b;
}

// ---- The list ----
function messageRow(m: Message, active: boolean): HTMLLIElement {
  const li = el("li", active ? "msg on" : "msg");
  li.title = active ? "Calls get this message" : "Click to use this message for calls";
  li.addEventListener("click", () => select(m.id));

  const pick = el("button", "pick");
  pick.setAttribute("aria-pressed", String(active));
  pick.setAttribute("aria-label", `Use “${m.name}” for calls`);
  pick.addEventListener("click", (e) => {
    e.stopPropagation();
    select(m.id);
  });

  let label: HTMLElement;
  if (renaming === m.id) {
    const input = el("input", "rename");
    input.value = m.name;
    input.maxLength = 60;
    input.spellcheck = false;
    input.dataset.id = m.id;
    input.setAttribute("aria-label", "New name");
    input.addEventListener("click", (e) => e.stopPropagation());
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") commitRename(m.id, input.value);
      else if (e.key === "Escape") cancelRename();
    });
    input.addEventListener("blur", () => {
      if (!rebuilding) commitRename(m.id, input.value);
    });
    label = input;
  } else {
    label = el("span", "name", m.name);
  }

  const listenBtn = iconButton(listening === m.id ? "■" : "▶", listening === m.id ? "Stop" : "Listen", () =>
    toggleListen(m.id),
  );
  const renameBtn = iconButton("✎", "Rename", () => startRename(m.id));
  const armed = armedDelete === m.id;
  const deleteBtn = iconButton(armed ? "Delete?" : "✕", armed ? "Click again to delete" : "Delete", () =>
    deleteMessage(m.id),
  );
  if (armed) deleteBtn.classList.add("armed");

  li.append(pick, label, el("span", "dur", clock(m.secs)), listenBtn, renameBtn, deleteBtn);
  return li;
}

function renderList(s: Status) {
  // Keep an open rename box (and what's typed in it) across refreshes.
  const open = listEl.querySelector<HTMLInputElement>("input.rename");
  const typed =
    open && open.dataset.id === renaming
      ? { value: open.value, start: open.selectionStart, end: open.selectionEnd }
      : null;
  if (renaming && !s.messages.some((m) => m.id === renaming)) renaming = null;

  rebuilding = true;
  try {
    listEl.replaceChildren(...s.messages.map((m) => messageRow(m, m.id === s.active)));
  } finally {
    rebuilding = false;
  }

  const input = listEl.querySelector<HTMLInputElement>("input.rename");
  if (input) {
    if (typed) {
      input.value = typed.value;
      input.setSelectionRange(typed.start, typed.end);
    } else {
      input.select();
    }
    input.focus();
  }
}

function render(s: Status) {
  status = s;
  renderList(s);
  const hasTake = s.takeSecs != null;
  const active = s.messages.find((m) => m.id === s.active);

  countText.textContent = s.messages.length ? String(s.messages.length) : "";
  emptyText.hidden = s.messages.length > 0 || s.recording || hasTake;

  // The new recording: recording now, or waiting for a name.
  takeBox.hidden = !s.recording && !hasTake;
  takeRow.hidden = s.recording;
  if (s.recording) renderRecordingClock();
  else if (hasTake) stateText.textContent = `New recording · ${clock(s.takeSecs!)} · name it and save`;
  stateText.classList.toggle("live", s.recording);
  if (hasTake && !hadTake) {
    takeName.value = s.takeName;
    takeName.focus();
    takeName.select();
  }
  hadTake = hasTake;
  takeListen.textContent = listening === "take" ? "■" : "▶";

  recordBtn.textContent = s.recording ? "■ Stop" : hasTake ? "● Re-record" : "● Record";
  recordBtn.disabled = uploading;
  uploadBtn.disabled = uploading || s.recording;
  uploadBtn.textContent = uploading ? "Adding…" : "Upload audio…";

  playBtn.hidden = s.recording || (!active && !s.playing);
  playBtn.textContent = s.playing ? "■ Stop playing" : `▶ Play “${active?.name ?? ""}” into the call now`;

  const r = s.route;
  if (r.toCall) {
    routeText.className = "route ok";
    routeText.textContent = `✓ Calls hear SayIt through “${r.device}”` + (r.mic ? ` · your voice via ${r.mic}` : "");
  } else {
    routeText.className = "route warn";
    routeText.textContent =
      "⚠ Calls can't hear SayIt yet: install VB-Audio Virtual Cable, then set your call app's microphone to “CABLE Output” and press Reconnect in Settings → SayIt.";
  }

  if (!s.messages.length) hintText.textContent = "";
  else if (!active) hintText.textContent = "Click the message calls should get.";
  else hintText.textContent = `During a call press ${s.playHotkey} to play it, and again to stop. Click another message to switch.`;
  if (r.error) showError(r.error);
}

function renderRecordingClock() {
  stateText.textContent = `● Recording ${clock((Date.now() - recordStarted) / 1000)} — say your message`;
}

async function refresh() {
  try {
    render(await invoke<Status>("sayit_status"));
  } catch (e) {
    showError(e);
  }
}

// ---- Picking, renaming, deleting ----
async function select(id: string) {
  if (!status || status.active === id) return;
  showError("");
  try {
    render(await invoke<Status>("sayit_select", { id }));
  } catch (e) {
    showError(e);
  }
}

function startRename(id: string) {
  renaming = id;
  armedDelete = null;
  if (status) render(status);
}

function cancelRename() {
  renaming = null;
  if (status) render(status);
}

async function commitRename(id: string, value: string) {
  if (renaming !== id) return; // Enter, then the blur that follows
  renaming = null;
  const current = status?.messages.find((m) => m.id === id);
  if (!current || value.trim() === "" || value.trim() === current.name) {
    if (status) render(status);
    return;
  }
  try {
    render(await invoke<Status>("sayit_rename", { id, name: value }));
  } catch (e) {
    showError(e);
    if (status) render(status);
  }
}

async function deleteMessage(id: string) {
  if (armedDelete !== id) {
    armedDelete = id;
    clearTimeout(armTimer);
    armTimer = window.setTimeout(() => {
      armedDelete = null;
      if (status) render(status);
    }, 3000);
    if (status) render(status);
    return;
  }
  armedDelete = null;
  clearTimeout(armTimer);
  if (listening === id) stopListening();
  try {
    render(await invoke<Status>("sayit_delete", { id }));
  } catch (e) {
    showError(e);
  }
}

// ---- Listening (on this computer only) ----
function stopListening() {
  player.pause();
  const was = listening;
  listening = null;
  if (was && status) render(status);
}

async function toggleListen(id: string | null) {
  const key = id ?? "take";
  if (listening === key) return stopListening();
  stopListening();
  try {
    const bytes = await invoke<ArrayBuffer>("sayit_audio", { id });
    if (blobUrl) URL.revokeObjectURL(blobUrl);
    blobUrl = URL.createObjectURL(new Blob([bytes], { type: "audio/wav" }));
    player.src = blobUrl;
    await player.play();
    listening = key;
    if (status) render(status);
  } catch (e) {
    showError(e);
  }
}
player.addEventListener("ended", stopListening);
takeListen.addEventListener("click", () => toggleListen(null));

// ---- Recording ----
recordBtn.addEventListener("click", async () => {
  showError("");
  showNote("");
  stopListening();
  try {
    const wasRecording = status?.recording ?? false;
    if (!wasRecording) recordStarted = Date.now();
    const s = await invoke<Status>("sayit_record_toggle");
    render(s);
    if (s.recording) {
      timer = window.setInterval(renderRecordingClock, 500);
    } else if (timer) {
      clearInterval(timer);
      timer = undefined;
    }
  } catch (e) {
    if (timer) clearInterval(timer);
    timer = undefined;
    showError(e);
    refresh();
  }
});

async function saveTake() {
  stopListening();
  try {
    render(await invoke<Status>("sayit_save", { name: takeName.value }));
    showNote("Saved.");
  } catch (e) {
    showError(e);
  }
}
saveBtn.addEventListener("click", saveTake);
takeName.addEventListener("keydown", (e) => {
  if (e.key === "Enter") saveTake();
});

discardBtn.addEventListener("click", async () => {
  stopListening();
  render(await invoke<Status>("sayit_discard"));
});

// ---- Uploading voicemail files ----
// The WebView decodes the file (MP3, WAV, WebM, OGG, M4A, FLAC…) to mono at
// UPLOAD_RATE; VoiceWriter trims the silence around it, levels it and saves it.
async function decodeFile(file: File): Promise<Float32Array> {
  if (file.size > MAX_UPLOAD_MB * 1024 * 1024) throw new Error(`it's over ${MAX_UPLOAD_MB} MB`);
  let audio: AudioBuffer;
  try {
    audio = await new OfflineAudioContext(1, 1, UPLOAD_RATE).decodeAudioData(await file.arrayBuffer());
  } catch {
    throw new Error("it isn't an audio file SayIt can read (try MP3, WAV, WebM, OGG or M4A)");
  }
  if (audio.duration > MAX_UPLOAD_SECS) {
    throw new Error(`it's ${Math.ceil(audio.duration / 60)} minutes long; a message can be up to ${MAX_UPLOAD_SECS / 60}`);
  }
  const mono = new Float32Array(audio.length);
  for (let c = 0; c < audio.numberOfChannels; c++) {
    const data = audio.getChannelData(c);
    for (let i = 0; i < mono.length; i++) mono[i] += data[i] / audio.numberOfChannels;
  }
  return mono;
}

const baseName = (fileName: string) => fileName.replace(/\.[^.\\/]+$/, "");

uploadBtn.addEventListener("click", () => {
  showError("");
  fileInput.click();
});

fileInput.addEventListener("change", async () => {
  const files = Array.from(fileInput.files ?? []);
  fileInput.value = ""; // so picking the same file again still counts
  if (!files.length) return;
  showError("");
  stopListening();
  uploading = true;
  if (status) render(status);
  const added: string[] = [];
  const failed: string[] = [];
  for (let i = 0; i < files.length; i++) {
    const file = files[i];
    showNote(files.length > 1 ? `Adding ${i + 1} of ${files.length}: ${file.name}…` : `Adding ${file.name}…`);
    try {
      const samples = await decodeFile(file);
      const before = new Set(status?.messages.map((m) => m.id));
      const s = await invoke<Status>("sayit_import", new Uint8Array(samples.buffer), {
        headers: { "x-sayit-name": encodeURIComponent(baseName(file.name)), "x-sayit-rate": String(UPLOAD_RATE) },
      });
      added.push(...s.messages.filter((m) => !before.has(m.id)).map((m) => `“${m.name}”`));
      render(s);
    } catch (e) {
      failed.push(`${file.name} (${errText(e)})`);
    }
  }
  uploading = false;
  if (status) render(status);
  showNote(added.length ? `Added ${added.join(", ")}.` : "");
  showError(failed.length ? `Couldn't add ${failed.join("; ")}.` : "");
});

// ---- Playing into the call ----
playBtn.addEventListener("click", () => {
  stopListening();
  invoke("sayit_play_toggle").catch(showError);
});

$("closeBtn").addEventListener("click", () => {
  stopListening();
  invoke("sayit_hide").catch(() => {});
});

listen<Status>("sayit-status", (e) => render(e.payload)).catch(() => {});
refresh();
