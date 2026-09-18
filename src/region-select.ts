import { invoke } from "@tauri-apps/api/core";

const selection = document.getElementById("selection") as HTMLDivElement;
const hint = document.getElementById("hint") as HTMLParagraphElement;

let dragging = false;
let startX = 0;
let startY = 0;

function cancel() {
  invoke("cancel_region_select").catch(() => {});
}

function rectFrom(x1: number, y1: number, x2: number, y2: number) {
  return {
    left: Math.min(x1, x2),
    top: Math.min(y1, y2),
    width: Math.abs(x2 - x1),
    height: Math.abs(y2 - y1),
  };
}

document.addEventListener("mousedown", (ev) => {
  dragging = true;
  startX = ev.clientX;
  startY = ev.clientY;
  selection.style.display = "block";
  hint.style.display = "none";
});

document.addEventListener("mousemove", (ev) => {
  if (!dragging) return;
  const r = rectFrom(startX, startY, ev.clientX, ev.clientY);
  selection.style.left = `${r.left}px`;
  selection.style.top = `${r.top}px`;
  selection.style.width = `${r.width}px`;
  selection.style.height = `${r.height}px`;
});

document.addEventListener("mouseup", (ev) => {
  if (!dragging) return;
  dragging = false;
  const r = rectFrom(startX, startY, ev.clientX, ev.clientY);
  if (r.width < 4 || r.height < 4) {
    // Too small to be an intentional selection — treat like a cancel.
    cancel();
    return;
  }
  // Coordinates are relative to this window, which the backend positions to
  // exactly cover the virtual screen at (0,0) before showing it — so these
  // are already virtual-screen pixel coordinates, no translation needed.
  invoke("finish_region_select", {
    x: Math.round(r.left),
    y: Math.round(r.top),
    w: Math.round(r.width),
    h: Math.round(r.height),
  }).catch(() => {});
});

window.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") cancel();
});
