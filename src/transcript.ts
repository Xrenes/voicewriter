import { listen } from "@tauri-apps/api/event";

const thread = document.getElementById("thread") as HTMLDivElement;
const filePathEl = document.getElementById("filePath") as HTMLSpanElement;

interface TranscriptPayload {
  text: string;
  filePath: string;
}

/** Split into sentences on ., !, or ? followed by whitespace (or end of
 * string). Purely cosmetic — this is one mic source, not real speaker
 * separation, so the exact split points don't need to be linguistically
 * precise. */
function splitSentences(text: string): string[] {
  const trimmed = text.trim();
  if (!trimmed) return [];
  const matches = trimmed.match(/[^.!?]+[.!?]*(\s+|$)/g);
  if (!matches) return [trimmed];
  return matches.map((s) => s.trim()).filter(Boolean);
}

function render(payload: TranscriptPayload) {
  filePathEl.textContent = payload.filePath;
  filePathEl.title = payload.filePath;
  thread.innerHTML = "";

  const sentences = splitSentences(payload.text);
  if (sentences.length === 0) {
    const empty = document.createElement("p");
    empty.className = "empty";
    empty.textContent = "Nothing was transcribed.";
    thread.appendChild(empty);
    return;
  }

  sentences.forEach((sentence, i) => {
    const bubble = document.createElement("div");
    bubble.className = "bubble " + (i % 2 === 0 ? "left" : "right");
    bubble.textContent = sentence;
    thread.appendChild(bubble);
  });

  thread.scrollTop = thread.scrollHeight;
}

listen<TranscriptPayload>("transcript-ready", (e) => render(e.payload)).catch(() => {});
