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

// A magnifying glass — "Find" (capture a screenshot/photo).
const ICON_FIND =
  '<circle cx="10.5" cy="10.5" r="6.5" stroke="currentColor" stroke-width="1.7"/>' +
  '<path d="M15.3 15.3 20 20" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/>';

// A camera body with a lens circle.
const ICON_CAMERA =
  '<path d="M4 8a2 2 0 0 1 2-2h1.2l.8-1.4A1 1 0 0 1 8.86 4h6.28a1 1 0 0 1 .86.6L16.8 6H18a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2z" ' +
  'fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"/>' +
  '<circle cx="12" cy="13" r="3.4" fill="none" stroke="currentColor" stroke-width="1.6"/>';

// A monitor/rectangle with a small stand — full screenshot.
const ICON_SCREENSHOT =
  '<rect x="3.5" y="5" width="17" height="12" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.6"/>' +
  '<path d="M8 20h8M12 17v3" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>';

// Four corner brackets around a filled rectangle — select an area.
const ICON_SELECT_AREA =
  '<path d="M9 4H6a2 2 0 0 0-2 2v3M15 4h3a2 2 0 0 1 2 2v3M9 20H6a2 2 0 0 1-2-2v-3M15 20h3a2 2 0 0 0 2-2v-3" ' +
  'fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>' +
  '<rect x="8" y="8" width="8" height="8" rx="1" fill="currentColor" opacity="0.35"/>';

// A chat bubble with a spark — "AI" chat.
const ICON_AI =
  '<path d="M3 6.5A2.5 2.5 0 0 1 5.5 4h13A2.5 2.5 0 0 1 21 6.5v8A2.5 2.5 0 0 1 18.5 17H9l-4 3.5V17H5.5A2.5 2.5 0 0 1 3 14.5z" ' +
  'stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/>' +
  '<path d="M12 7.5l.9 2.1 2.1.9-2.1.9-.9 2.1-.9-2.1-2.1-.9 2.1-.9z" fill="currentColor"/>';

// A globe-with-browser-window look — "Web" (in-app browser).
const ICON_WEB =
  '<rect x="3" y="4.5" width="18" height="15" rx="2" fill="none" stroke="currentColor" stroke-width="1.6"/>' +
  '<path d="M3 8.5h18" stroke="currentColor" stroke-width="1.6"/>' +
  '<circle cx="6" cy="6.5" r="0.6" fill="currentColor"/>' +
  '<circle cx="8" cy="6.5" r="0.6" fill="currentColor"/>';

// Fixed 6-slot ring (like a full loadout wheel), starting at 12 o'clock,
// going clockwise.
const TOTAL_SLOTS = 6;

const MAIN_WEDGES: WedgeDef[] = [
  { action: "refine", label: "Refine", icon: ICON_REFINE },
  { action: "bangla", label: "Bangla", icon: ICON_BANGLA },
  { action: "__translate", label: "Translate", icon: ICON_GLOBE },
  { action: "__record", label: "Record", icon: ICON_RECORD },
  { action: "__web", label: "Web", icon: ICON_WEB },
  { action: "__ai", label: "AI", icon: ICON_AI },
];

// Second-level menu shown after clicking "Translate": pick a target language.
const LANGUAGE_WEDGES: WedgeDef[] = [
  { action: "translate:en", label: "English", icon: ICON_GLOBE },
  { action: "translate:bn", label: "Bangla", icon: ICON_GLOBE },
  { action: "translate:es", label: "Spanish", icon: ICON_GLOBE },
  { action: "translate:it", label: "Italian", icon: ICON_GLOBE },
];

// Second-level menu shown after clicking "Find": pick a capture source.
const FIND_WEDGES: WedgeDef[] = [
  { action: "__find:camera", label: "Camera", icon: ICON_CAMERA },
  { action: "__find:screenshot", label: "Screenshot", icon: ICON_SCREENSHOT },
  { action: "__find:area", label: "Select area", icon: ICON_SELECT_AREA },
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
const permission = document.getElementById("permission") as HTMLDivElement;
const permissionAllow = document.getElementById("permissionAllow") as HTMLButtonElement;
const permissionDeny = document.getElementById("permissionDeny") as HTMLButtonElement;

const win = getCurrentWindow();

// Set right before showing the permission panel, so Allow knows which
// capture source to actually run once consent is granted.
let pendingCaptureSource: string | null = null;

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
  if (action === "__find") {
    // Same submenu pattern as Translate: swap the ring to the capture-source
    // wedges rather than opening a separate window.
    buildWheel(FIND_WEDGES);
    return;
  }
  if (action.startsWith("__find:")) {
    const source = action.slice("__find:".length);
    runCapture(source);
    return;
  }
  if (action === "__ai") {
    invoke("open_ai_chat_window").catch((e) => showError(String(e)));
    invoke("wheel_cancel").catch(() => {});
    return;
  }
  if (action === "__web") {
    invoke("open_web_window").catch((e) => showError(String(e)));
    invoke("wheel_cancel").catch(() => {});
    return;
  }
  runAction(action);
}

/** Backend command for each Find submenu wedge. */
const CAPTURE_COMMANDS: Record<string, string> = {
  camera: "capture_via_camera",
  screenshot: "capture_via_screenshot",
  area: "start_region_select",
};

async function runCapture(source: string) {
  if (!CAPTURE_COMMANDS[source]) return;
  const granted = await invoke<boolean>("capture_permission_status").catch(() => false);
  if (!granted) {
    pendingCaptureSource = source;
    showPermissionPrompt();
    return;
  }
  fireCapture(source);
}

function fireCapture(source: string) {
  const command = CAPTURE_COMMANDS[source];
  if (!command) return;
  // "area" hands off to the region-select overlay window, which does its own
  // capture+save once the user finishes dragging — everything else captures
  // immediately. Either way the wheel just needs to get out of the way.
  invoke(command).catch((e) => showError(String(e)));
  invoke("wheel_cancel").catch(() => {});
}

function showPermissionPrompt() {
  wheel.hidden = true;
  permission.hidden = false;
}

permissionAllow.addEventListener("click", async () => {
  await invoke("grant_capture_permission").catch(() => {});
  permission.hidden = true;
  const source = pendingCaptureSource;
  pendingCaptureSource = null;
  if (source) fireCapture(source);
  else invoke("wheel_cancel").catch(() => {});
});

permissionDeny.addEventListener("click", () => {
  pendingCaptureSource = null;
  invoke("wheel_cancel").catch(() => {});
});

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

// Escape steps back to the main menu from a submenu, or cancels the whole
// wheel if already on the main menu.
window.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") {
    if (currentWedges === LANGUAGE_WEDGES || currentWedges === FIND_WEDGES) {
      buildWheel(MAIN_WEDGES);
    } else {
      invoke("wheel_cancel").catch(() => {});
    }
  }
});

// Clicking anywhere that isn't a wedge dismisses — including the preview
// panel itself, since it has no buttons of its own (click it to dismiss
// once you've read/copied the result). The permission panel DOES have its
// own buttons, so clicks inside it are excluded — otherwise this handler
// would race Allow/Not now's own click handler and cancel the wheel first.
document.body.addEventListener("click", (ev) => {
  const target = ev.target as HTMLElement;
  if (target.closest(".wedge-path") || target.closest(".permission")) return;
  invoke("wheel_cancel").catch(() => {});
});

// The backend emits this right before showing the ring again, so a window
// left on the preview panel, the language picker, or mid-loading from a
// previous run starts clean — the window is only ever hidden between uses,
// never reloaded.
listen("wheel-reset", () => {
  setLoading(false);
  preview.hidden = true;
  permission.hidden = true;
  pendingCaptureSource = null;
  wheel.hidden = false;
  buildWheel(MAIN_WEDGES);
});

win.onCloseRequested(() => {
  setLoading(false);
});
