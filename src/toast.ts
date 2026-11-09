import { listen } from "@tauri-apps/api/event";

interface ToastPayload {
  kind: "busy" | "done" | "error";
  text: string;
}

const toast = document.getElementById("toast") as HTMLDivElement;
const text = document.getElementById("text") as HTMLSpanElement;

listen<ToastPayload>("toast", (e) => {
  toast.dataset.kind = e.payload.kind;
  text.textContent = e.payload.text;
  toast.title = e.payload.text;
}).catch(() => {});
