import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";

interface WedgeDef {
  action: string;
  label: string;
  /** Inline SVG markup (no outer <svg> tag), stroke="currentColor". */
  icon: string;
}

// 24x24 line icons (Feather/Lucide-style grid), one per action. currentColor
// picks up the wedge-icon's white color automatically.

// A magic wand with sparkles — represents "refine" as a polish/cleanup wand.
const ICON_REFINE =
  '<path d="M4.5 19.5 15 9" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>' +
  '<path d="M13.2 6.8 15 5l1.8 1.8L18.6 5l1.8 1.8-1.8 1.8L20.4 10.4 18.6 12.2l-1.8-1.8-1.8 1.8-1.8-1.8 1.8-1.8z" ' +
  'fill="currentColor"/>' +
  '<path d="M6 4v3M4.5 5.5h3" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>' +
  '<path d="M19 16v2.5M17.75 17.25h2.5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>';

// A briefcase with a rounded handle and a center latch line.
const ICON_PROFESSIONAL =
  '<rect x="3" y="8" width="18" height="12" rx="2" stroke="currentColor" stroke-width="1.6"/>' +
  '<path d="M8.5 8V6.5A2.5 2.5 0 0 1 11 4h2a2.5 2.5 0 0 1 2.5 2.5V8" ' +
  'stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>' +
  '<path d="M3 13h18" stroke="currentColor" stroke-width="1.6"/>' +
  '<rect x="10.5" y="12" width="3" height="2.4" rx="0.6" fill="currentColor"/>';

// A globe with meridian lines — represents translation in general.
const ICON_GLOBE =
  '<circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="1.6"/>' +
  '<ellipse cx="12" cy="12" rx="4" ry="9" stroke="currentColor" stroke-width="1.6"/>' +
  '<path d="M3 12h18" stroke="currentColor" stroke-width="1.6"/>' +
  '<path d="M4.5 7h15M4.5 17h15" stroke="currentColor" stroke-width="1.6"/>';

// Two overlapping speech bubbles — the dedicated one-click Bangla shortcut.
const ICON_BANGLA =
  '<path d="M3 6.5A2.5 2.5 0 0 1 5.5 4h8A2.5 2.5 0 0 1 16 6.5v4A2.5 2.5 0 0 1 13.5 13H9l-3.5 3v-3H5.5A2.5 2.5 0 0 1 3 10.5z" ' +
  'stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/>' +
  '<path d="M15 9.5h1.5A2.5 2.5 0 0 1 19 12v3.5a2.5 2.5 0 0 1-2.5 2.5H16v2.5l-3-2.5" ' +
  'stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/>';

// A solid red-tinted dot inside a ring — a record button.
const ICON_RECORD =
  '<circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="1.6"/>' +
  '<circle cx="12" cy="12" r="5" fill="currentColor"/>';

// Fixed 6-slot ring (like a full loadout wheel), starting at 12 o'clock,
// going clockwise.
const TOTAL_SLOTS = 6;

const MAIN_WEDGES: WedgeDef[] = [
  { action: "refine", label: "Refine", icon: ICON_REFINE },
  { action: "professional", label: "Professional", icon: ICON_PROFESSIONAL },
  { action: "bangla", label: "Bangla", icon: ICON_BANGLA },
  { action: "__translate", label: "Translate", icon: ICON_GLOBE },
  { action: "__record", label: "Record", icon: ICON_RECORD },
];

// Second-level menu shown after clicking "Translate": pick a target language.
const LANGUAGE_WEDGES: WedgeDef[] = [
  { action: "translate:en", label: "English", icon: ICON_GLOBE },
  { action: "translate:bn", label: "Bangla", icon: ICON_GLOBE },
  { action: "translate:es", label: "Spanish", icon: ICON_GLOBE },
  { action: "translate:it", label: "Italian", icon: ICON_GLOBE },
];

let currentWedges = MAIN_WEDGES;

const CENTER = 120;
const OUTER_R = 116;
const INNER_R = 50;
const GAP_DEG = 3; // small visual gap between wedges

const wheel = document.getElementById("wheel") as HTMLDivElement;
const svg = document.getElementById("wheelSvg") as unknown as SVGSVGElement;
const hub = document.getElementById("hub") as HTMLDivElement;
const hubLabel = document.getElementById("hubLabel") as HTMLSpanElement;
const labelsLayer = document.getElementById("labels") as HTMLDivElement;
const preview = document.getElementById("preview") as HTMLDivElement;
const previewText = document.getElementById("previewText") as HTMLDivElement;

const win = getCurrentWindow();

function polar(cx: number, cy: number, r: number, angleDeg: number) {
  const rad = ((angleDeg - 90) * Math.PI) / 180;
  return { x: cx + r * Math.cos(rad), y: cy + r * Math.sin(rad) };
}

/** SVG path for a donut wedge spanning [startDeg, endDeg). */
function donutWedgePath(startDeg: number, endDeg: number): string {
  const large = endDeg - startDeg > 180 ? 1 : 0;
  const oStart = polar(CENTER, CENTER, OUTER_R, startDeg);
  const oEnd = polar(CENTER, CENTER, OUTER_R, endDeg);
  const iStart = polar(CENTER, CENTER, INNER_R, startDeg);
  const iEnd = polar(CENTER, CENTER, INNER_R, endDeg);
  return [
    `M ${iStart.x} ${iStart.y}`,
    `L ${oStart.x} ${oStart.y}`,
    `A ${OUTER_R} ${OUTER_R} 0 ${large} 1 ${oEnd.x} ${oEnd.y}`,
    `L ${iEnd.x} ${iEnd.y}`,
    `A ${INNER_R} ${INNER_R} 0 ${large} 0 ${iStart.x} ${iStart.y}`,
    "Z",
  ].join(" ");
}

function buildWheel(wedges: WedgeDef[]) {
  currentWedges = wedges;
  svg.innerHTML = "";
  labelsLayer.innerHTML = "";
  hubLabel.textContent = "";

  const step = 360 / TOTAL_SLOTS;
  const mid = (INNER_R + OUTER_R) / 2;

  for (let i = 0; i < TOTAL_SLOTS; i++) {
    const start = i * step + GAP_DEG / 2;
    const end = (i + 1) * step - GAP_DEG / 2;
    const wedge = wedges[i];

    const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
    path.setAttribute("d", donutWedgePath(start, end));
    svg.appendChild(path);

    if (!wedge) {
      // Reserved, unfilled slot: rendered but inert and nearly invisible.
      path.setAttribute("class", "wedge-path empty");
      continue;
    }

    path.setAttribute("class", "wedge-path");
    path.dataset.action = wedge.action;

    const labelAngle = (start + end) / 2;
    const pos = polar(CENTER, CENTER, mid, labelAngle);
    const label = document.createElement("div");
    label.className = "wedge-label";
    label.style.left = `${(pos.x / 240) * 100}%`;
    label.style.top = `${(pos.y / 240) * 100}%`;
    label.dataset.action = wedge.action;

    const iconSvg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    iconSvg.setAttribute("class", "wedge-icon");
    iconSvg.setAttribute("viewBox", "0 0 24 24");
    iconSvg.innerHTML = wedge.icon; // trusted, hardcoded per-action constant

    const text = document.createElement("span");
    text.className = "wedge-text";
    text.textContent = wedge.label;

    label.appendChild(iconSvg);
    label.appendChild(text);
    labelsLayer.appendChild(label);

    const setHover = (on: boolean) => {
      path.classList.toggle("hover", on);
      label.classList.toggle("hover", on);
      hub.classList.toggle("hover", on);
      hubLabel.textContent = on ? wedge.label : "";
    };
    path.addEventListener("mouseenter", () => setHover(true));
    path.addEventListener("mouseleave", () => setHover(false));
    path.addEventListener("click", () => onWedgeClick(wedge.action));
  }
}

function onWedgeClick(action: string) {
  if (action === "__translate") {
    buildWheel(LANGUAGE_WEDGES);
    return;
  }
  if (action === "__record") {
    // Fire-and-forget: closes the whole wheel immediately (recording has its
    // own floating widget, not the wheel's preview panel). Toggles: starts
    // if idle, stops (and transcribes/saves) if already recording.
    invoke("wheel_record_toggle").catch((e) => showError(String(e)));
    invoke("wheel_cancel").catch(() => {});
    return;
  }
  runAction(action);
}

function setLoading(on: boolean) {
  wheel.classList.toggle("loading", on);
}

function showError(message: string) {
  const toast = document.createElement("div");
  toast.className = "error-toast";
  toast.textContent = message;
  wheel.appendChild(toast);
  setTimeout(() => toast.remove(), 2500);
}

function showPreview(text: string) {
  wheel.hidden = true;
  previewText.textContent = text;
  preview.hidden = false;
}

async function runAction(action: string) {
  setLoading(true);
  try {
    const result = await invoke<string>("wheel_run", { action });
    setLoading(false);
    showPreview(result);
  } catch (e) {
    showError(String(e));
    setLoading(false);
  }
}

buildWheel(MAIN_WEDGES);

// Escape steps back to the main menu from the language picker, or cancels
// the whole wheel if already on the main menu.
window.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") {
    if (currentWedges === LANGUAGE_WEDGES) {
      buildWheel(MAIN_WEDGES);
    } else {
      invoke("wheel_cancel").catch(() => {});
    }
  }
});

// Clicking anywhere that isn't a wedge dismisses — including the preview
// panel itself, since it has no buttons of its own (click it to dismiss
// once you've read/copied the result).
document.body.addEventListener("click", (ev) => {
  const target = ev.target as HTMLElement;
  if (target.closest(".wedge-path")) return;
  invoke("wheel_cancel").catch(() => {});
});

// The backend emits this right before showing the ring again, so a window
// left on the preview panel, the language picker, or mid-loading from a
// previous run starts clean — the window is only ever hidden between uses,
// never reloaded.
listen("wheel-reset", () => {
  setLoading(false);
  preview.hidden = true;
  wheel.hidden = false;
  buildWheel(MAIN_WEDGES);
});

win.onCloseRequested(() => {
  setLoading(false);
});
