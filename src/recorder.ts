import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const stopBtn = document.getElementById("stopBtn") as HTMLButtonElement;
const mark = document.getElementById("mark") as unknown as SVGSVGElement;

function resetBlink() {
  stopBtn.disabled = false;
  // Restart the CSS blink animation from its first frame for each new
  // recording (re-showing the window doesn't reload the page, so a running
  // animation would otherwise just keep going from wherever it left off).
  mark.style.animation = "none";
  mark.getBoundingClientRect(); // force reflow before re-enabling the animation
  mark.style.animation = "";
}

listen("recorder-started", resetBlink).catch(() => {});

stopBtn.addEventListener("click", async () => {
  stopBtn.disabled = true;
  try {
    await invoke("stop_recording");
  } catch (e) {
    console.error("stop_recording failed", e);
  }
});
