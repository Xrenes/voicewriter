import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const filenameInput = document.getElementById("filename") as HTMLInputElement;
const folderInput = document.getElementById("folder") as HTMLInputElement;
const chooseFolderBtn = document.getElementById("chooseFolder") as HTMLButtonElement;
const player = document.getElementById("player") as HTMLAudioElement;
const hint = document.getElementById("hint") as HTMLParagraphElement;
const cancelBtn = document.getElementById("cancelBtn") as HTMLButtonElement;
const confirmBtn = document.getElementById("confirmBtn") as HTMLButtonElement;

interface ConfirmReadyPayload {
  defaultName: string;
  defaultFolder: string;
}

let currentBlobUrl: string | null = null;

async function loadAudioPreview() {
  hint.textContent = "Loading preview…";
  try {
    const bytes = await invoke<number[]>("recording_audio_data");
    if (currentBlobUrl) URL.revokeObjectURL(currentBlobUrl);
    const blob = new Blob([new Uint8Array(bytes)], { type: "audio/wav" });
    currentBlobUrl = URL.createObjectURL(blob);
    player.src = currentBlobUrl;
    hint.textContent = "";
  } catch (e) {
    hint.textContent = "Preview unavailable: " + String(e);
  }
}

listen<ConfirmReadyPayload>("recorder-confirm-ready", (e) => {
  filenameInput.value = e.payload.defaultName;
  folderInput.value = e.payload.defaultFolder;
  confirmBtn.disabled = false;
  loadAudioPreview();
}).catch(() => {});

chooseFolderBtn.addEventListener("click", async () => {
  const folder = await invoke<string | null>("choose_recording_folder");
  if (folder) folderInput.value = folder;
});

cancelBtn.addEventListener("click", async () => {
  await invoke("cancel_recording").catch(() => {});
});

confirmBtn.addEventListener("click", async () => {
  confirmBtn.disabled = true;
  hint.textContent = "Transcribing…";
  try {
    await invoke("confirm_recording", {
      name: filenameInput.value.trim(),
      folder: folderInput.value.trim(),
    });
  } catch (e) {
    hint.textContent = "Failed: " + String(e);
    confirmBtn.disabled = false;
  }
});
