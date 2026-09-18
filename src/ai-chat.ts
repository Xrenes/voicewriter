import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

interface CaptureEntry {
  path: string;
  fileName: string;
  modifiedMs: number;
}

interface SessionSummary {
  id: string;
  title: string;
  updatedMs: number;
}

interface ChatTurn {
  role: "user" | "assistant";
  text: string;
  imagePath?: string | null;
}

interface Session {
  id: string;
  title: string;
  createdMs: number;
  updatedMs: number;
  turns: ChatTurn[];
}

const sidebar = document.getElementById("sidebar") as HTMLElement;
const sidebarToggleBtn = document.getElementById("sidebarToggleBtn") as HTMLButtonElement;
const modelSelect = document.getElementById("modelSelect") as HTMLSelectElement;
const newChatBtn = document.getElementById("newChatBtn") as HTMLButtonElement;
const sessionList = document.getElementById("sessionList") as HTMLDivElement;
const thread = document.getElementById("thread") as HTMLDivElement;
const emptyHint = document.getElementById("emptyHint") as HTMLParagraphElement;
const gallery = document.getElementById("gallery") as HTMLDivElement;
const galleryGrid = document.getElementById("galleryGrid") as HTMLDivElement;
const attachBtn = document.getElementById("attachBtn") as HTMLButtonElement;
const attachFileBtn = document.getElementById("attachFileBtn") as HTMLButtonElement;
const attachLinkBtn = document.getElementById("attachLinkBtn") as HTMLButtonElement;
const pendingThumb = document.getElementById("pendingThumb") as HTMLImageElement;
const textInput = document.getElementById("textInput") as HTMLInputElement;
const sendBtn = document.getElementById("sendBtn") as HTMLButtonElement;

let currentSession: Session | null = null;
let sessions: SessionSummary[] = [];
let pendingImagePath: string | null = null;
let pendingImageDataUrl: string | null = null;
let sending = false;
const imageCache = new Map<string, string>();

/** Guess an image's MIME type from its magic bytes — a file attached via
 * "attach a file" can be any common format, not just the PNG every in-app
 * capture actually is, so the extension/assumption can't be trusted. */
function sniffMime(bytes: number[]): string {
  if (bytes[0] === 0x89 && bytes[1] === 0x50) return "image/png"; // \x89PNG
  if (bytes[0] === 0xff && bytes[1] === 0xd8) return "image/jpeg"; // JFIF
  if (bytes[0] === 0x47 && bytes[1] === 0x49 && bytes[2] === 0x46) return "image/gif"; // GIF8
  if (bytes[0] === 0x42 && bytes[1] === 0x4d) return "image/bmp"; // BM
  if (bytes[8] === 0x57 && bytes[9] === 0x45 && bytes[10] === 0x42 && bytes[11] === 0x50) return "image/webp"; // RIFF....WEBP
  return "image/png";
}

function bytesToDataUrl(bytes: number[]): string {
  const bin = String.fromCharCode(...bytes);
  return `data:${sniffMime(bytes)};base64,${btoa(bin)}`;
}

async function imageDataUrlFor(path: string): Promise<string> {
  const cached = imageCache.get(path);
  if (cached) return cached;
  const bytes = await invoke<number[]>("read_capture_bytes", { path });
  const url = bytesToDataUrl(bytes);
  imageCache.set(path, url);
  return url;
}

/** Inline **bold** within a line of text, appended as DOM nodes (never
 * innerHTML — the text is model output, so this must not be parsed as HTML). */
function appendInline(parent: HTMLElement, line: string) {
  const re = /\*\*(.+?)\*\*/g;
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(line))) {
    if (m.index > last) parent.appendChild(document.createTextNode(line.slice(last, m.index)));
    const strong = document.createElement("strong");
    strong.textContent = m[1];
    parent.appendChild(strong);
    last = re.lastIndex;
  }
  if (last < line.length) parent.appendChild(document.createTextNode(line.slice(last)));
}

/** Minimal markdown renderer covering what Groq's replies actually use:
 * paragraphs, **bold**, and numbered/bulleted lists. Deliberately not a full
 * markdown parser — just enough to turn "1. Foo" / "- Foo" / "**Foo**" into
 * real structure instead of showing the literal markdown syntax. */
function renderMarkdown(container: HTMLElement, text: string) {
  const lines = text.split("\n");
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.trim() === "") {
      i++;
      continue;
    }
    const numbered = /^\s*\d+\.\s+/.exec(line);
    const bulleted = /^\s*[-*]\s+/.exec(line);
    if (numbered || bulleted) {
      const list = document.createElement(numbered ? "ol" : "ul");
      while (i < lines.length) {
        const m = numbered ? /^\s*\d+\.\s+(.*)$/.exec(lines[i]) : /^\s*[-*]\s+(.*)$/.exec(lines[i]);
        if (!m) break;
        const li = document.createElement("li");
        appendInline(li, m[1]);
        list.appendChild(li);
        i++;
      }
      container.appendChild(list);
      continue;
    }
    const p = document.createElement("p");
    appendInline(p, line);
    container.appendChild(p);
    i++;
  }
}

const SPARK_ICON =
  '<path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z" fill="currentColor"/>';

function renderThread() {
  thread.replaceChildren();
  const turns = currentSession?.turns ?? [];
  emptyHint.hidden = turns.length > 0;
  for (const turn of turns) {
    if (turn.role === "user") {
      const row = document.createElement("div");
      row.className = "msg-row user";
      const bubble = document.createElement("div");
      bubble.className = "pill-bubble";
      if (turn.imagePath) {
        const img = document.createElement("img");
        imageDataUrlFor(turn.imagePath).then((url) => (img.src = url));
        bubble.appendChild(img);
      }
      const p = document.createElement("div");
      p.textContent = turn.text;
      bubble.appendChild(p);
      row.appendChild(bubble);
      thread.appendChild(row);
    } else {
      const row = document.createElement("div");
      row.className = "msg-row assistant";
      const icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
      icon.setAttribute("class", "spark");
      icon.setAttribute("viewBox", "0 0 24 24");
      icon.innerHTML = SPARK_ICON;
      row.appendChild(icon);
      const body = document.createElement("div");
      body.className = "assistant-body";
      renderMarkdown(body, turn.text);
      row.appendChild(body);
      thread.appendChild(row);
    }
  }
  thread.scrollTop = thread.scrollHeight;
}

function renderSidebar() {
  sessionList.replaceChildren();
  if (sessions.length === 0) {
    const p = document.createElement("p");
    p.className = "session-empty";
    p.textContent = "No chats yet";
    sessionList.appendChild(p);
    return;
  }
  for (const s of sessions) {
    const item = document.createElement("div");
    item.className = "session-item" + (s.id === currentSession?.id ? " active" : "");
    const label = document.createElement("span");
    label.textContent = s.title;
    item.appendChild(label);

    const del = document.createElement("button");
    del.className = "del";
    del.innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>';
    del.title = "Delete chat";
    del.addEventListener("click", async (ev) => {
      ev.stopPropagation();
      await invoke("ai_chat_delete_session", { id: s.id }).catch(() => {});
      await refreshSidebar();
      if (currentSession?.id === s.id) {
        await openOrCreateSession();
      }
    });
    item.appendChild(del);

    item.addEventListener("click", () => openSession(s.id));
    sessionList.appendChild(item);
  }
}

async function refreshSidebar() {
  sessions = await invoke<SessionSummary[]>("ai_chat_list_sessions").catch(() => []);
  renderSidebar();
}

async function openSession(id: string) {
  currentSession = await invoke<Session>("ai_chat_open_session", { id }).catch(() => null);
  renderThread();
  renderSidebar();
}

async function openOrCreateSession() {
  if (sessions.length > 0) {
    await openSession(sessions[0].id);
  } else {
    currentSession = await invoke<Session>("ai_chat_new_session").catch(() => null);
    renderThread();
    await refreshSidebar();
  }
}

newChatBtn.addEventListener("click", async () => {
  currentSession = await invoke<Session>("ai_chat_new_session").catch(() => null);
  renderThread();
  await refreshSidebar();
});

async function loadGallery() {
  galleryGrid.replaceChildren();
  try {
    const entries = await invoke<CaptureEntry[]>("list_capture_gallery");
    if (entries.length === 0) {
      const p = document.createElement("p");
      p.className = "gallery-empty";
      p.textContent = "Nothing here yet — use Find on the wheel to add something first.";
      galleryGrid.appendChild(p);
      return;
    }
    for (const entry of entries) {
      const img = document.createElement("img");
      img.loading = "lazy";
      img.title = entry.fileName;
      imageDataUrlFor(entry.path).then((url) => (img.src = url));
      img.addEventListener("click", async () => {
        pendingImagePath = entry.path;
        pendingImageDataUrl = await imageDataUrlFor(entry.path);
        pendingThumb.src = pendingImageDataUrl;
        pendingThumb.hidden = false;
        gallery.hidden = true;
      });
      galleryGrid.appendChild(img);
    }
  } catch (e) {
    const p = document.createElement("p");
    p.className = "gallery-empty";
    p.textContent = String(e);
    galleryGrid.appendChild(p);
  }
}

attachBtn.addEventListener("click", () => {
  const willShow = gallery.hidden;
  gallery.hidden = !willShow;
  if (willShow) loadGallery();
});
pendingThumb.addEventListener("click", () => {
  pendingImagePath = null;
  pendingImageDataUrl = null;
  pendingThumb.hidden = true;
  pendingThumb.src = "";
});

function appendErrorBubble(message: string) {
  const row = document.createElement("div");
  row.className = "msg-row assistant";
  const body = document.createElement("div");
  body.className = "assistant-body error-text";
  body.textContent = message;
  row.appendChild(body);
  emptyHint.hidden = true;
  thread.appendChild(row);
  thread.scrollTop = thread.scrollHeight;
}

async function send() {
  const text = textInput.value.trim();
  if (!text || sending) return;
  if (!currentSession) {
    // Should be unreachable now that the session loads synchronously on
    // page load (see loadCurrentSession above), but failing loudly here
    // instead of silently no-op'ing is what would have made the earlier
    // "Send does nothing" bug immediately obvious instead of looking broken.
    appendErrorBubble("Chat isn't ready yet — try again in a moment.");
    return;
  }
  sending = true;
  sendBtn.disabled = true;

  const imagePath = pendingImagePath;
  pendingImagePath = null;
  pendingImageDataUrl = null;
  pendingThumb.hidden = true;
  pendingThumb.src = "";
  textInput.value = "";

  // Optimistic render of the user's turn while the request is in flight.
  currentSession.turns.push({ role: "user", text, imagePath });
  renderThread();

  try {
    currentSession = await invoke<Session>("ai_chat_ask", {
      sessionId: currentSession.id,
      text,
      imagePath: imagePath ?? null,
    });
    renderThread();
    await refreshSidebar();
  } catch (e) {
    // The backend already rolled back the user turn it appended on failure —
    // reload from disk so this window's view matches what's actually saved.
    if (currentSession) {
      currentSession = await invoke<Session>("ai_chat_open_session", { id: currentSession.id }).catch(() => currentSession);
    }
    renderThread();
    appendErrorBubble(String(e));
  } finally {
    sending = false;
    sendBtn.disabled = false;
  }
}

sendBtn.addEventListener("click", send);
textInput.addEventListener("keydown", (ev) => {
  if (ev.key === "Enter") send();
});

// Attach a file from disk (native picker, restricted to image formats —
// the backend enforces this, see pick_image_file in lib.rs).
attachFileBtn.addEventListener("click", async () => {
  const path = await invoke<string | null>("pick_image_file").catch(() => null);
  if (!path) return;
  pendingImagePath = path;
  pendingImageDataUrl = await imageDataUrlFor(path);
  pendingThumb.src = pendingImageDataUrl;
  pendingThumb.hidden = false;
});

// Attach a link: no fetch/preview support, so this just drops the URL into
// the message text for the model to reason about as plain text.
attachLinkBtn.addEventListener("click", () => {
  const url = window.prompt("Paste a link to include with your message:");
  if (!url || !url.trim()) return;
  const sep = textInput.value.trim() ? " " : "";
  textInput.value = textInput.value + sep + url.trim();
  textInput.focus();
});

// ---- Sidebar: hidden by default, toggled on demand rather than always shown ----
sidebarToggleBtn.addEventListener("click", () => {
  sidebar.hidden = !sidebar.hidden;
  sidebarToggleBtn.classList.toggle("active", !sidebar.hidden);
});

// ---- Model picker, right here in the chat window instead of only in
// Settings — ground truth from this key's own model list, each one actually
// tested with a real request rather than just listed (see
// list_vision_models_tested's doc comment in lib.rs: Groq has deprecated/
// renamed vision models out from under a hardcoded constant more than once,
// and a model can be listed for an account without actually being usable). ----
interface ModelTestResult {
  model: string;
  ok: boolean;
  error: string | null;
}

function buildModelOption(m: string, current: string, mark?: string): HTMLOptionElement {
  const opt = document.createElement("option");
  opt.value = m;
  opt.textContent = mark ? `${mark} ${m}` : m;
  if (m === current) opt.selected = true;
  return opt;
}

async function loadModelOptions() {
  modelSelect.disabled = true;
  const loadingOpt = document.createElement("option");
  loadingOpt.textContent = "Loading…";
  modelSelect.replaceChildren(loadingOpt);

  let current: string;
  try {
    current = await invoke<string>("get_vision_model");
  } catch {
    current = "";
  }

  // Show the untested list immediately — testing every model is a handful of
  // real API calls and takes a few seconds, so the dropdown shouldn't sit on
  // "Loading…" the whole time.
  let models: string[];
  try {
    models = await invoke<string[]>("list_vision_models");
  } catch (e) {
    const opt = document.createElement("option");
    opt.textContent = "No key set";
    modelSelect.replaceChildren(opt);
    modelSelect.title = String(e);
    return;
  }
  if (models.length === 0) {
    const opt = document.createElement("option");
    opt.textContent = "No models found";
    modelSelect.replaceChildren(opt);
    return;
  }
  modelSelect.replaceChildren(...models.map((m) => buildModelOption(m, current)));
  modelSelect.disabled = false;
  // Nothing saved yet — default to the first model in the list rather than
  // silently sending requests with the (possibly stale/404) fallback
  // constant in vision.rs until the user happens to open the dropdown.
  if (!current && models.length > 0) {
    current = models[0];
    await invoke("set_vision_model", { model: current }).catch(() => {});
  }

  // Now test every model for real and mark ✓/✗ once results land. This is a
  // connectivity/existence check only (plain text request, no image) — it
  // catches a deprecated/wrong model id, the actual failure this app has hit
  // twice, but doesn't confirm real vision/image support on its own.
  try {
    const results = await invoke<ModelTestResult[]>("list_vision_models_tested");
    const selectedValue = modelSelect.value || current;
    modelSelect.replaceChildren(
      ...results.map((r) => buildModelOption(r.model, selectedValue, r.ok ? "✓" : "✗")),
    );
  } catch {
    // Testing failed outright (e.g. key removed mid-flight) — leave the
    // untested list in place rather than erroring out the whole dropdown.
  }
}

modelSelect.addEventListener("change", () => {
  invoke("set_vision_model", { model: modelSelect.value }).catch(() => {});
});

loadModelOptions();

// ---- Custom resize edges (decorations:false removes the OS resize border) ----
// startResizeDragging hands the drag off to Windows' native non-client
// resize loop (the same mechanism a real titlebar border uses) — it needs
// the actual native mousedown to reach the webview intact, so this must NOT
// call preventDefault()/stopPropagation() or the OS has nothing to hand off
// and the call silently does nothing.
const win = getCurrentWindow();
type ResizeDirection = "East" | "North" | "NorthEast" | "NorthWest" | "South" | "SouthEast" | "SouthWest" | "West";
document.querySelectorAll<HTMLElement>(".resize-edge").forEach((el) => {
  el.addEventListener("mousedown", (ev) => {
    if (ev.button !== 0) return;
    const dir = el.dataset.dir as ResizeDirection;
    win.startResizeDragging(dir).catch(() => {});
  });
});

// ---- Window lifecycle: resume the active session on (re)open, dismiss on
// Escape or a click outside the window's content ----

// Primary path: pull the active session directly once this page has loaded
// and its own listeners are registered — see ai_chat_current_session's doc
// comment in lib.rs for why the emitted event alone isn't reliable enough
// (it can race a dev-mode window reload and get silently dropped, leaving
// currentSession stuck at null forever — the concrete bug this was fixed
// for: pressing Send appeared to do nothing).
async function loadCurrentSession() {
  currentSession = await invoke<Session>("ai_chat_current_session").catch(() => null);
  renderThread();
  await refreshSidebar();
}
loadCurrentSession();

// Secondary path: still listen for the event, for the case where the window
// is already open and reused for a different/updated session without a
// reload in between (no race there, since the listener has been live the
// whole time).
listen<Session>("ai-chat-session", (e) => {
  currentSession = e.payload;
  renderThread();
  refreshSidebar();
}).catch(() => {});

window.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") win.hide().catch(() => {});
});

// "Click away" for a desktop window means clicking outside its bounds
// entirely (it loses OS focus), not clicking empty space within it.
win.onFocusChanged(({ payload: focused }) => {
  if (!focused) {
    win.hide().catch(() => {});
  }
}).catch(() => {});
