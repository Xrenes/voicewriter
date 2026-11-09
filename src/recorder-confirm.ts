import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const filenameInput = document.getElementById("filename") as HTMLInputElement;
const folderInput = document.getElementById("folder") as HTMLInputElement;
const chooseFolderBtn = document.getElementById("chooseFolder") as HTMLButtonElement;
const player = document.getElementById("player") as HTMLAudioElement;
const captureInfo = document.getElementById("captureInfo") as HTMLSpanElement;
const deviceNote = document.getElementById("deviceNote") as HTMLParagraphElement;
const hint = document.getElementById("hint") as HTMLParagraphElement;
const cancelBtn = document.getElementById("cancelBtn") as HTMLButtonElement;
const confirmBtn = document.getElementById("confirmBtn") as HTMLButtonElement;

interface ConfirmInfo {
  /** Identifies the recording, so a duplicate notification doesn't reset typing. */
  token: string;
  defaultName: string;
  defaultFolder: string;
  durationSecs: number;
  deviceCaptured: boolean;
  deviceNote?: string | null;
}

let currentToken: string | null = null;
let currentBlobUrl: string | null = null;

function formatDuration(secs: number): string {
  const t = Math.max(0, Math.round(secs));
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const s = String(t % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

// The preview is mic + device audio already mixed into one track — exactly
// what gets saved.
async function loadAudioPreview() {
  hint.textContent = "Loading preview…";
  try {
    const bytes = await invoke<ArrayBuffer>("recording_audio_data");
    if (currentBlobUrl) URL.revokeObjectURL(currentBlobUrl);
    currentBlobUrl = URL.createObjectURL(new Blob([bytes], { type: "audio/wav" }));
    player.src = currentBlobUrl;
    hint.textContent = "";
  } catch (e) {
    hint.textContent = "Preview unavailable: " + String(e);
  }
}

// Every new recording replaces the form; the window is reused between
// recordings rather than reloaded.
function apply(info: ConfirmInfo) {
  if (info.token === currentToken) return;
  currentToken = info.token;
  filenameInput.value = info.defaultName;
  folderInput.value = info.defaultFolder;
  const source = info.deviceCaptured ? "your mic + device audio" : "your mic only";
  captureInfo.textContent = `${formatDuration(info.durationSecs)} · ${source}`;
  deviceNote.textContent = info.deviceNote ?? "";
  deviceNote.hidden = !info.deviceNote;
  confirmBtn.disabled = false;
  cancelBtn.disabled = false;
  filenameInput.focus();
  filenameInput.select();
  loadAudioPreview();
}

// Pull on load as well as listening: in dev builds the window is reloaded
// right before it's shown, which can drop an event emitted in between.
invoke<ConfirmInfo | null>("recorder_confirm_defaults")
  .then((info) => {
    if (info) apply(info);
  })
  .catch((e) => {
    hint.textContent = "Failed to load recording: " + String(e);
  });

listen<ConfirmInfo>("recorder-confirm-ready", (e) => apply(e.payload)).catch(() => {});

chooseFolderBtn.addEventListener("click", async () => {
  const folder = await invoke<string | null>("choose_recording_folder");
  if (folder) folderInput.value = folder;
});

cancelBtn.addEventListener("click", async () => {
  player.pause();
  currentToken = null;
  await invoke("cancel_recording").catch(() => {});
});

confirmBtn.addEventListener("click", async () => {
  confirmBtn.disabled = true;
  cancelBtn.disabled = true;
  player.pause();
  hint.textContent = "Saving…";
  try {
    await invoke("confirm_recording", {
      name: filenameInput.value.trim(),
      folder: folderInput.value.trim(),
    });
    hint.textContent = "";
    currentToken = null;
  } catch (e) {
    hint.textContent = "Couldn't save: " + String(e);
    confirmBtn.disabled = false;
    cancelBtn.disabled = false;
  }
});

filenameInput.addEventListener("keydown", (ev) => {
  if (ev.key === "Enter" && !confirmBtn.disabled) confirmBtn.click();
});
