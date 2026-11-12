// Live transcript overlay: a see-through black window that shows the call as
// it's being transcribed. White is you (your mic), yellow is the other side
// (sound played on this PC). Dimmed text is still being spoken and updates
// in place until it's final. Consecutive lines from the same speaker flow
// together as one paragraph, which re-wraps as the window is resized.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

interface LiveLine {
  id: number;
  who: "you" | "them";
  start: number;
  end: number;
  text: string;
  isFinal: boolean;
}

interface Snapshot {
  version: number;
  lines: LiveLine[];
  active: boolean;
  notice: string | null;
}

/** Same-speaker lines closer than this (seconds) share a paragraph. */
const JOIN_GAP = 3;

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const feed = $("feed");
const empty = $("empty");
const stateEl = $("state");
const stateText = $("stateText");
const notice = $("notice");
const copyBtn = $<HTMLButtonElement>("copy");

// DOM nodes are kept per line and per paragraph, so an update only touches
// what changed (and a text selection survives new lines arriving).
const spans = new Map<number, HTMLSpanElement>();
const paras = new Map<number, HTMLParagraphElement>();
let shownVersion = -1;

function nearBottom() {
  return feed.scrollHeight - feed.scrollTop - feed.clientHeight < 48;
}

function render(snap: Snapshot) {
  if (snap.version <= shownVersion) return; // an older copy arrived late
  shownVersion = snap.version;
  const stick = nearBottom();

  const groups: LiveLine[][] = [];
  for (const line of snap.lines) {
    const group = groups[groups.length - 1];
    const last = group?.[group.length - 1];
    if (group && last.who === line.who && line.start - last.end < JOIN_GAP) group.push(line);
    else groups.push([line]);
  }

  const liveSpans = new Set<number>();
  const liveParas = new Set<number>();
  let prevPara: Element = empty;
  for (const group of groups) {
    const key = group[0].id;
    liveParas.add(key);
    let p = paras.get(key);
    if (!p) {
      p = document.createElement("p");
      paras.set(key, p);
    }
    if (p.className !== group[0].who) p.className = group[0].who;
    if (prevPara.nextElementSibling !== p) prevPara.after(p);
    prevPara = p;

    let prevSpan: Element | null = null;
    for (const line of group) {
      liveSpans.add(line.id);
      let s = spans.get(line.id);
      if (!s) {
        s = document.createElement("span");
        spans.set(line.id, s);
      }
      const cls = line.isFinal ? "" : "partial";
      if (s.className !== cls) s.className = cls;
      const text = line.text + " ";
      if (s.textContent !== text) s.textContent = text;
      const inPlace = prevSpan ? prevSpan.nextElementSibling === s : p.firstElementChild === s;
      if (!inPlace) {
        if (prevSpan) prevSpan.after(s);
        else p.prepend(s);
      }
      prevSpan = s;
    }
    // Spans that now belong to a later paragraph get moved there below.
    while (prevSpan?.nextElementSibling) prevSpan.nextElementSibling.remove();
  }
  for (const [id, s] of spans) {
    if (!liveSpans.has(id)) {
      s.remove();
      spans.delete(id);
    }
  }
  for (const [id, p] of paras) {
    if (!liveParas.has(id)) {
      p.remove();
      paras.delete(id);
    }
  }

  empty.hidden = snap.lines.length > 0;
  empty.textContent = snap.active ? "Listening… start talking." : "Nothing was said.";
  stateEl.classList.toggle("ended", !snap.active);
  stateText.textContent = snap.active ? "Live" : "Ended";
  notice.hidden = !snap.notice;
  notice.textContent = snap.notice ?? "";
  if (stick) feed.scrollTop = feed.scrollHeight;
}

// Versions only ever grow (a new recording continues the count), so the
// highest one seen is always the current state.
listen<Snapshot>("live-update", (e) => render(e.payload));
invoke<Snapshot>("live_snapshot").then(render).catch(() => {});

// Text size, remembered between calls.
let scale = 1;
try {
  scale = Number(localStorage.getItem("liveScale")) || 1;
} catch {}
function applyScale() {
  document.documentElement.style.setProperty("--scale", String(scale));
  try {
    localStorage.setItem("liveScale", String(scale));
  } catch {}
}
applyScale();
$("smaller").addEventListener("click", () => {
  scale = Math.max(0.6, +(scale - 0.1).toFixed(2));
  applyScale();
});
$("bigger").addEventListener("click", () => {
  scale = Math.min(2.4, +(scale + 0.1).toFixed(2));
  applyScale();
});

// The window never takes focus (so the call app keeps it), which rules out
// the browser clipboard; the app copies instead.
copyBtn.addEventListener("click", async () => {
  try {
    await invoke("live_copy");
    copyBtn.textContent = "Copied";
  } catch {
    copyBtn.textContent = "Failed";
  }
  setTimeout(() => (copyBtn.textContent = "Copy"), 1200);
});

$("close").addEventListener("click", () => invoke("live_close").catch(() => {}));

// Resize from any edge (the window has no OS border). Don't preventDefault:
// Windows needs the native mousedown to start its resize loop.
const win = getCurrentWindow();
type Dir = "East" | "North" | "NorthEast" | "NorthWest" | "South" | "SouthEast" | "SouthWest" | "West";
document.querySelectorAll<HTMLElement>(".resize-edge").forEach((el) => {
  el.addEventListener("mousedown", (ev) => {
    if (ev.button !== 0) return;
    win.startResizeDragging(el.dataset.dir as Dir).catch(() => {});
  });
});
