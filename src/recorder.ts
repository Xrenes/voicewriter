import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

const stopBtn = document.getElementById("stopBtn") as HTMLButtonElement;
const mark = document.getElementById("mark") as unknown as SVGSVGElement;

// The widget is both a "click to stop" button and, per an explicit request,
// draggable to reposition. Calling startDragging() on every mousedown would
// break plain clicks (Windows' native drag loop takes over mouse tracking
// once started, so a zero-movement "drag" can still swallow the following
// click). Instead we only engage startDragging() once the mouse has
// actually moved past a small threshold while held down — a real click
// never crosses that threshold, so it fires normally.
const DRAG_THRESHOLD_PX = 4;
let dragCandidate: { x: number; y: number } | null = null;
let dragStarted = false;

stopBtn.addEventListener("mousedown", (ev) => {
  if (ev.button !== 0) return;
  dragCandidate = { x: ev.clientX, y: ev.clientY };
  dragStarted = false;
});

window.addEventListener("mousemove", (ev) => {
  if (!dragCandidate || dragStarted) return;
  const dx = ev.clientX - dragCandidate.x;
  const dy = ev.clientY - dragCandidate.y;
  if (Math.hypot(dx, dy) < DRAG_THRESHOLD_PX) return;
  dragStarted = true;
  dragCandidate = null;
  getCurrentWindow().startDragging().catch(() => {});
});

window.addEventListener("mouseup", () => {
  dragCandidate = null;
});

// Suppress the click that a just-finished drag would otherwise also fire
// (stopping the recording was never the intent of a drag).
stopBtn.addEventListener(
  "click",
  (ev) => {
    if (dragStarted) {
      ev.stopImmediatePropagation();
      ev.preventDefault();
      dragStarted = false;
    }
  },
  true,
);

function resetBlink() {
  stopBtn.disabled = false;
  // Restart the CSS blink animation from its first frame for each new
  // recording (re-showing the window doesn't reload the page, so a running
  // animation would otherwise just keep going from wherever it left off).
  mark.style.animation = "none";
  mark.getBoundingClientRect(); // force reflow before re-enabling the animation
  mark.style.animation = "";
}

function flashError() {
  // A fast amber flash, distinct from the slow red "recording" blink, so a
  // failure reads as different from normal recording state at a glance —
  // the widget is the only thing on screen when this fires (see stop_recording
  // in lib.rs), so this is the user's only feedback that something went wrong.
  mark.style.animation = "none";
  mark.getBoundingClientRect();
  mark.style.animation = "flash-amber 0.35s ease-in-out 3";
}

listen("recorder-started", resetBlink).catch(() => {});
listen<string>("recorder-error", (e) => {
  console.error("recording failed:", e.payload);
  flashError();
}).catch(() => {});

stopBtn.addEventListener("click", async () => {
  stopBtn.disabled = true;
  try {
    await invoke("stop_recording");
  } catch (e) {
    console.error("stop_recording failed", e);
  }
});
