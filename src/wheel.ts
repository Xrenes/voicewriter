import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface WedgeDef {
  action: string;
  label: string;
  /** Inline SVG markup (no outer <svg> tag), stroke="currentColor". */
  icon: string;
}

/** What's running, from the backend, so those wedges read "Stop". */
interface WheelState {
  call: "live" | "record" | null;
  sayitPlaying: boolean;
}

// 24x24 line icons (Feather/Lucide-style grid), one per action. currentColor
// picks up the wedge-icon's white color automatically.

// A caption box with lines of text — "Live" transcript.
const ICON_LIVE =
  '<rect x="3" y="5" width="18" height="14" rx="2.5" fill="none" stroke="currentColor" stroke-width="1.6"/>' +
  '<path d="M7 10.5h5M14.5 10.5H17M7 14.5h2.5M12 14.5h5" fill="none" stroke="currentColor" stroke-width="1.6" ' +
  'stroke-linecap="round"/>';

// A solid red-tinted dot inside a ring — a record button.
const ICON_RECORD =
  '<circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="1.6"/>' +
  '<circle cx="12" cy="12" r="5" fill="currentColor"/>';

// The voicemail symbol (two reels joined) — "SayIt", the voicemail player.
const ICON_SAYIT =
  '<circle cx="6.5" cy="12" r="3.5" fill="none" stroke="currentColor" stroke-width="1.6"/>' +
  '<circle cx="17.5" cy="12" r="3.5" fill="none" stroke="currentColor" stroke-width="1.6"/>' +
  '<path d="M6.5 15.5h11" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>';

const ICON_PLAY =
  '<path d="M8 5.5v13l10.5-6.5z" fill="currentColor" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>';

const ICON_STOP = '<rect x="6.5" y="6.5" width="11" height="11" rx="2" fill="currentColor"/>';

// A microphone — record or change the SayIt message.
const ICON_MESSAGE =
  '<rect x="9" y="3.5" width="6" height="10.5" rx="3" fill="none" stroke="currentColor" stroke-width="1.6"/>' +
  '<path d="M6 11a6 6 0 0 0 12 0M12 17v3.5M9 20.5h6" fill="none" stroke="currentColor" stroke-width="1.6" ' +
  'stroke-linecap="round"/>';

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

// Fixed 5-slot ring, starting at 12 o'clock, going clockwise.
const TOTAL_SLOTS = 5;

let state: WheelState = { call: null, sayitPlaying: false };

// Main ring. Whatever is running reads "Stop" (clicking it stops it).
function mainWedges(): WedgeDef[] {
  const live = state.call === "live";
  const recording = state.call === "record";
  return [
    { action: "__live", label: live ? "Stop live" : "Live", icon: live ? ICON_STOP : ICON_LIVE },
    { action: "__record", label: recording ? "Stop" : "Record", icon: recording ? ICON_STOP : ICON_RECORD },
    { action: "__sayit", label: "SayIt", icon: ICON_SAYIT },
    { action: "__web", label: "Web", icon: ICON_WEB },
    { action: "__ai", label: "AI", icon: ICON_AI },
  ];
}

// Second-level menu shown after clicking "SayIt".
function sayitWedges(): WedgeDef[] {
  const playing = state.sayitPlaying;
  return [
    { action: "__sayit:play", label: playing ? "Stop" : "Play", icon: playing ? ICON_STOP : ICON_PLAY },
    { action: "__sayit:message", label: "Message", icon: ICON_MESSAGE },
  ];
}

// Second-level menu shown after clicking "Find": pick a capture source.
const FIND_WEDGES: WedgeDef[] = [
  { action: "__find:camera", label: "Camera", icon: ICON_CAMERA },
  { action: "__find:screenshot", label: "Screenshot", icon: ICON_SCREENSHOT },
  { action: "__find:area", label: "Select area", icon: ICON_SELECT_AREA },
];

let onMainRing = true;

const CENTER = 120;
const OUTER_R = 116;
const INNER_R = 50;
const GAP_DEG = 3; // small visual gap between wedges

const wheel = document.getElementById("wheel") as HTMLDivElement;
const svg = document.getElementById("wheelSvg") as unknown as SVGSVGElement;
const hub = document.getElementById("hub") as HTMLDivElement;
const hubLabel = document.getElementById("hubLabel") as HTMLSpanElement;
const labelsLayer = document.getElementById("labels") as HTMLDivElement;
const permission = document.getElementById("permission") as HTMLDivElement;
const permissionAllow = document.getElementById("permissionAllow") as HTMLButtonElement;
const permissionDeny = document.getElementById("permissionDeny") as HTMLButtonElement;


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

function buildWheel(wedges: WedgeDef[], main = false) {
  onMainRing = main;
  svg.innerHTML = "";
  labelsLayer.innerHTML = "";
  hubLabel.textContent = "";
  // The hovered wedge is removed without a mouseleave, so clear its hover.
  hub.classList.remove("hover");

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

function showMainRing() {
  buildWheel(mainWedges(), true);
}

/** Run a backend command and close the wheel. Failures are shown by the
 * backend's own toast, since the wheel is already gone by then. */
function runAndClose(command: string) {
  invoke(command).catch((e) => showError(String(e)));
  invoke("wheel_cancel").catch(() => {});
}

function onWedgeClick(action: string) {
  switch (action) {
    // Both toggle: start if idle, stop (and open the save dialog) if this
    // kind of recording is already running. Each has its own floating widget
    // or window, so the wheel just closes.
    case "__live":
      runAndClose("wheel_live_toggle");
      return;
    case "__record":
      runAndClose("wheel_record_toggle");
      return;
    case "__sayit":
      buildWheel(sayitWedges());
      return;
    case "__sayit:play":
      runAndClose("sayit_play_toggle");
      return;
    case "__sayit:message":
      runAndClose("open_sayit_window");
      return;
    case "__web":
      runAndClose("open_web_window");
      return;
    case "__ai":
      runAndClose("open_ai_chat_window");
      return;
    case "__find":
      // Swap the ring to the capture-source wedges rather than opening a
      // separate window.
      buildWheel(FIND_WEDGES);
      return;
  }
  if (action.startsWith("__find:")) {
    runCapture(action.slice("__find:".length));
  }
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
  runAndClose(command);
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

function showError(message: string) {
  const toast = document.createElement("div");
  toast.className = "error-toast";
  toast.textContent = message;
  wheel.appendChild(toast);
  setTimeout(() => toast.remove(), 2500);
}

showMainRing();
// In development the page reloads on every open, which can drop the reset
// event below, so also ask for the current state directly.
invoke<WheelState>("wheel_state")
  .then((s) => {
    state = s;
    if (onMainRing) showMainRing();
  })
  .catch(() => {});

// Escape steps back to the main menu from a submenu, or cancels the whole
// wheel if already on the main menu.
window.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") {
    if (!onMainRing) {
      showMainRing();
    } else {
      invoke("wheel_cancel").catch(() => {});
    }
  }
});

// Clicking anywhere that isn't a wedge dismisses. The permission panel has
// its own buttons, so clicks inside it are excluded — otherwise this handler
// would race Allow/Not now's own click handler and cancel the wheel first.
document.body.addEventListener("click", (ev) => {
  const target = ev.target as HTMLElement;
  if (target.closest(".wedge-path") || target.closest(".permission")) return;
  invoke("wheel_cancel").catch(() => {});
});

// The backend emits this right before showing the ring again, with what's
// running, so a window left on a submenu or the permission panel from a
// previous run starts clean — the window is only ever hidden between uses,
// never reloaded.
listen<WheelState>("wheel-reset", (e) => {
  state = e.payload;
  permission.hidden = true;
  pendingCaptureSource = null;
  wheel.hidden = false;
  showMainRing();
});
