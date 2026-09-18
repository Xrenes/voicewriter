import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

const backBtn = document.getElementById("backBtn") as HTMLButtonElement;
const forwardBtn = document.getElementById("forwardBtn") as HTMLButtonElement;
const reloadBtn = document.getElementById("reloadBtn") as HTMLButtonElement;
const shareBtn = document.getElementById("shareBtn") as HTMLButtonElement;
const closeBtn = document.getElementById("closeBtn") as HTMLButtonElement;
const addressInput = document.getElementById("addressInput") as HTMLInputElement;
const addressIcon = document.getElementById("addressIcon") as unknown as SVGSVGElement;

backBtn.addEventListener("click", () => invoke("web_back").catch(() => {}));
forwardBtn.addEventListener("click", () => invoke("web_forward").catch(() => {}));
reloadBtn.addEventListener("click", () => invoke("web_reload").catch(() => {}));
closeBtn.addEventListener("click", () => invoke("web_close").catch(() => {}));

const shareIconMarkup = shareBtn.innerHTML;
const checkIconMarkup =
  '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12.5l4.5 4.5L19 7.5" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/></svg>';
shareBtn.addEventListener("click", async () => {
  try {
    await invoke("web_copy_url");
    shareBtn.innerHTML = checkIconMarkup;
    shareBtn.title = "Copied!";
    setTimeout(() => {
      shareBtn.innerHTML = shareIconMarkup;
      shareBtn.title = "Copy URL";
    }, 1200);
  } catch {
    // no page open yet — nothing to copy, ignore
  }
});

let lastRealUrl = "";

/** Chrome-style: show just the domain when not focused/editing, the full
 * URL once the user clicks in to actually edit it. */
function displayUrl(url: string): string {
  try {
    const u = new URL(url);
    return u.hostname + (u.pathname !== "/" ? u.pathname : "");
  } catch {
    return url;
  }
}

addressInput.addEventListener("focus", () => {
  addressInput.value = lastRealUrl;
  addressInput.select();
});
addressInput.addEventListener("blur", () => {
  if (lastRealUrl) addressInput.value = displayUrl(lastRealUrl);
});

addressInput.addEventListener("keydown", (ev) => {
  if (ev.key !== "Enter") return;
  const value = addressInput.value.trim();
  if (!value) return;
  invoke("web_navigate", { url: value }).catch(() => {});
  addressInput.blur();
});

// The address bar reflects wherever web-content has actually navigated to —
// polling is the simplest reliable way to notice that, since there's no
// dedicated "did-navigate" event surfaced across the two sibling webviews.
async function pollUrl() {
  try {
    const url = await invoke<string | null>("web_current_url");
    if (url && url !== lastRealUrl) {
      lastRealUrl = url;
      addressIcon.style.opacity = url.startsWith("https://") ? "1" : "0.4";
      if (document.activeElement !== addressInput) {
        addressInput.value = displayUrl(url);
      }
    }
  } catch {
    // window not open yet — ignore
  }
}
setInterval(pollUrl, 1000);
pollUrl();

// Custom resize edges: the "web" window is fully borderless (no native
// titlebar/frame at all, per an explicit request for custom-only chrome),
// so there's no OS resize border either. Only top-half edges/corners are
// placed in this toolbar strip — see the comment in web-toolbar.html for
// why (this is the only trusted, our-own-HTML surface in the window).
const win = getCurrentWindow();
type ResizeDirection = "North" | "West" | "East" | "NorthWest" | "NorthEast";
document.querySelectorAll<HTMLElement>(".resize-edge").forEach((el) => {
  el.addEventListener("mousedown", (ev) => {
    if (ev.button !== 0) return;
    const dir = el.dataset.dir as ResizeDirection;
    win.startResizeDragging(dir).catch(() => {});
  });
});

window.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") invoke("web_close").catch(() => {});
});
