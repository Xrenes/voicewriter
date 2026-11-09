import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface Line {
  speaker: "You" | "Device";
  start: number;
  end: number;
  text: string;
}

interface View {
  state: "transcribing" | "done" | "error";
  title: string;
  audioPath: string;
  textPath?: string | null;
  lines: Line[];
  message?: string | null;
  canRetry: boolean;
}

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const titleEl = $("title");
const filePathEl = $("filePath");
const copyBtn = $<HTMLButtonElement>("copyBtn");
const revealBtn = $<HTMLButtonElement>("revealBtn");
const statusEl = $("status");
const spinner = $("spinner");
const statusText = $("statusText");
const retryBtn = $<HTMLButtonElement>("retryBtn");
const legend = $("legend");
const thread = $("thread");

function clock(secs: number): string {
  const t = Math.max(0, Math.floor(secs));
  const h = Math.floor(t / 3600);
  const m = String(Math.floor((t % 3600) / 60)).padStart(2, "0");
  const s = String(t % 60).padStart(2, "0");
  return h > 0 ? `${h}:${m}:${s}` : `${m}:${s}`;
}

function lineElement(line: Line): HTMLElement {
  const row = document.createElement("div");
  row.className = "msg " + (line.speaker === "You" ? "you" : "device");
  const meta = document.createElement("div");
  meta.className = "meta";
  meta.textContent = `${line.speaker} · ${clock(line.start)}`;
  const bubble = document.createElement("div");
  bubble.className = "bubble";
  bubble.textContent = line.text;
  row.append(meta, bubble);
  return row;
}

function render(v: View) {
  titleEl.textContent = v.title || "Transcript";
  const path = v.textPath || v.audioPath;
  filePathEl.textContent = path;
  filePathEl.title = path;
  revealBtn.disabled = !v.audioPath;
  copyBtn.disabled = v.state !== "done" || v.lines.length === 0;

  const busy = v.state === "transcribing";
  statusEl.hidden = v.state === "done" && !v.message;
  statusEl.dataset.state = v.state;
  spinner.hidden = !busy;
  statusText.textContent = v.message || (busy ? "Transcribing…" : "");
  retryBtn.hidden = !v.canRetry;
  retryBtn.disabled = false;

  legend.hidden = v.lines.length === 0;
  thread.replaceChildren(...v.lines.map(lineElement));
  thread.scrollTop = 0;
}

retryBtn.addEventListener("click", () => {
  retryBtn.disabled = true;
  invoke("retry_transcription").catch((e) => {
    statusText.textContent = String(e);
    retryBtn.disabled = false;
  });
});

copyBtn.addEventListener("click", async () => {
  try {
    await invoke("transcript_copy");
    copyBtn.textContent = "Copied";
    setTimeout(() => (copyBtn.textContent = "Copy"), 1500);
  } catch (e) {
    statusText.textContent = "Copy failed: " + String(e);
  }
});

revealBtn.addEventListener("click", () => {
  invoke("transcript_reveal").catch(() => {});
});

// Pull the current state on load (the window can be reloaded in dev builds
// right before it's shown), then follow updates.
invoke<View | null>("transcript_current")
  .then((v) => {
    if (v) render(v);
  })
  .catch(() => {});
listen<View>("transcript-update", (e) => render(e.payload)).catch(() => {});
