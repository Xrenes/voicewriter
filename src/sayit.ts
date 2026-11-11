import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface Route {
  toCall: boolean;
  device: string;
  mic: string;
  error?: string | null;
}

interface Status {
  savedSecs?: number | null;
  takeSecs?: number | null;
  recording: boolean;
  playing: boolean;
  route: Route;
  playHotkey: string;
}

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const stateText = $("stateText");
const recordBtn = $<HTMLButtonElement>("recordBtn");
const listenBtn = $<HTMLButtonElement>("listenBtn");
const saveBtn = $<HTMLButtonElement>("saveBtn");
const discardBtn = $<HTMLButtonElement>("discardBtn");
const deleteBtn = $<HTMLButtonElement>("deleteBtn");
const playBtn = $<HTMLButtonElement>("playBtn");
const routeText = $("routeText");
const hintText = $("hintText");
const errorText = $("errorText");
const player = $<HTMLAudioElement>("player");

let status: Status | null = null;
let recordStarted = 0;
let timer: number | undefined;
let blobUrl: string | null = null;
let deleteArmed = false;

const clock = (secs: number) => {
  const t = Math.max(0, Math.round(secs));
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
};

function showError(e: unknown) {
  errorText.textContent = e ? String(e) : "";
}

function render(s: Status) {
  status = s;
  const hasTake = s.takeSecs != null;
  const hasSaved = s.savedSecs != null;

  if (s.recording) {
    stateText.textContent = `● Recording ${clock((Date.now() - recordStarted) / 1000)} — say your message`;
  } else if (hasTake) {
    stateText.textContent = `New recording · ${clock(s.takeSecs!)} · not saved yet`;
  } else if (hasSaved) {
    stateText.textContent = `Saved message · ${clock(s.savedSecs!)}`;
  } else {
    stateText.textContent = "No message saved yet — press Record";
  }
  stateText.classList.toggle("live", s.recording);

  recordBtn.textContent = s.recording ? "■ Stop" : hasSaved || hasTake ? "● Re-record" : "● Record";
  listenBtn.hidden = s.recording || (!hasTake && !hasSaved);
  saveBtn.hidden = s.recording || !hasTake;
  discardBtn.hidden = s.recording || !hasTake;
  deleteBtn.hidden = s.recording || hasTake || !hasSaved;
  playBtn.hidden = s.recording || hasTake || !hasSaved;
  playBtn.textContent = s.playing ? "■ Stop playing" : "▶ Play into the call now";

  const r = s.route;
  if (r.toCall) {
    routeText.className = "route ok";
    routeText.textContent = `✓ Calls hear SayIt through “${r.device}”` + (r.mic ? ` · your voice via ${r.mic}` : "");
  } else {
    routeText.className = "route warn";
    routeText.textContent =
      "⚠ Calls can't hear SayIt yet: install VB-Audio Virtual Cable, then set your call app's microphone to “CABLE Output” and press Reconnect in Settings → SayIt.";
  }
  hintText.textContent = hasSaved
    ? `During a call press ${s.playHotkey} to play it, and ${s.playHotkey} again to stop.`
    : "";
  if (r.error) showError(r.error);
}

async function refresh() {
  try {
    render(await invoke<Status>("sayit_status"));
  } catch (e) {
    showError(e);
  }
}

recordBtn.addEventListener("click", async () => {
  showError("");
  stopListening();
  try {
    const wasRecording = status?.recording ?? false;
    if (!wasRecording) recordStarted = Date.now();
    const s = await invoke<Status>("sayit_record_toggle");
    render(s);
    if (s.recording) {
      timer = window.setInterval(() => status && render(status), 500);
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

function stopListening() {
  player.pause();
  listenBtn.textContent = "▶ Listen";
}

listenBtn.addEventListener("click", async () => {
  if (!player.paused) {
    stopListening();
    return;
  }
  try {
    const bytes = await invoke<ArrayBuffer>("sayit_audio");
    if (blobUrl) URL.revokeObjectURL(blobUrl);
    blobUrl = URL.createObjectURL(new Blob([bytes], { type: "audio/wav" }));
    player.src = blobUrl;
    await player.play();
    listenBtn.textContent = "■ Stop";
  } catch (e) {
    showError(e);
  }
});
player.addEventListener("ended", stopListening);

saveBtn.addEventListener("click", async () => {
  stopListening();
  try {
    render(await invoke<Status>("sayit_save"));
  } catch (e) {
    showError(e);
  }
});

discardBtn.addEventListener("click", async () => {
  stopListening();
  render(await invoke<Status>("sayit_discard"));
});

deleteBtn.addEventListener("click", async () => {
  if (!deleteArmed) {
    deleteArmed = true;
    deleteBtn.textContent = "Sure? Delete";
    setTimeout(() => {
      deleteArmed = false;
      deleteBtn.textContent = "Delete";
    }, 3000);
    return;
  }
  deleteArmed = false;
  deleteBtn.textContent = "Delete";
  stopListening();
  try {
    render(await invoke<Status>("sayit_delete"));
  } catch (e) {
    showError(e);
  }
});

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
