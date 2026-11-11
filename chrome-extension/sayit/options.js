const $ = (id) => document.getElementById(id);
const state = $("state");
const recordBtn = $("record");
const listenBtn = $("listen");
const saveBtn = $("save");
const discardBtn = $("discard");
const removeBtn = $("remove");
const player = $("player");
const errorEl = $("error");

let recorder = null;
let take = null; // Blob of a new, unsaved recording
let saved = null; // { data (base64), type, secs }
let startedAt = 0;
let timer = 0;

const clock = (s) => `${Math.floor(s / 60)}:${String(Math.round(s % 60)).padStart(2, "0")}`;
const showError = (e) => (errorEl.textContent = e ? String(e) : "");

function render() {
  const recording = !!recorder;
  if (recording) state.textContent = `● Recording ${clock((Date.now() - startedAt) / 1000)} — say your message`;
  else if (take) state.textContent = "New recording · not saved yet";
  else if (saved) state.textContent = `Saved message · ${clock(saved.secs || 0)}`;
  else state.textContent = "No message saved yet — press Record";
  recordBtn.textContent = recording ? "■ Stop" : saved || take ? "● Re-record" : "● Record";
  listenBtn.hidden = recording || (!take && !saved);
  saveBtn.hidden = discardBtn.hidden = recording || !take;
  removeBtn.hidden = recording || !!take || !saved;
}

async function load() {
  ({ message: saved = null } = await chrome.storage.local.get("message"));
  render();
  pushClipToPage();
  const cmds = await chrome.commands.getAll();
  const cmd = cmds.find((c) => c.name === "toggle-message");
  $("hotkey").textContent = (cmd && cmd.shortcut) || "not set";
}

recordBtn.addEventListener("click", async () => {
  showError("");
  player.pause();
  if (recorder) {
    recorder.stop();
    return;
  }
  try {
    const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    const chunks = [];
    recorder = new MediaRecorder(stream);
    recorder.ondataavailable = (e) => e.data.size && chunks.push(e.data);
    recorder.onstop = () => {
      stream.getTracks().forEach((t) => t.stop());
      take = new Blob(chunks, { type: recorder.mimeType });
      recorder = null;
      clearInterval(timer);
      render();
    };
    take = null;
    startedAt = Date.now();
    recorder.start();
    timer = setInterval(render, 500);
    render();
  } catch (e) {
    showError(`Couldn't use the microphone: ${e}`);
  }
});

function blobToBase64(blob) {
  return new Promise((resolve) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).split(",")[1]);
    r.readAsDataURL(blob);
  });
}

listenBtn.addEventListener("click", () => {
  if (!player.paused) {
    player.pause();
    listenBtn.textContent = "▶ Listen";
    return;
  }
  player.src = take ? URL.createObjectURL(take) : `data:${saved.type};base64,${saved.data}`;
  player.play();
  listenBtn.textContent = "■ Stop";
});
player.addEventListener("ended", () => (listenBtn.textContent = "▶ Listen"));

saveBtn.addEventListener("click", async () => {
  const data = await blobToBase64(take);
  const secs = await durationOf(take);
  saved = { data, type: take.type, secs };
  await chrome.storage.local.set({ message: saved });
  take = null;
  render();
  pushClipToPage();
});

discardBtn.addEventListener("click", () => {
  take = null;
  render();
});

let removeArmed = false;
removeBtn.addEventListener("click", async () => {
  if (!removeArmed) {
    removeArmed = true;
    removeBtn.textContent = "Sure? Delete";
    setTimeout(() => {
      removeArmed = false;
      removeBtn.textContent = "Delete";
    }, 3000);
    return;
  }
  removeArmed = false;
  removeBtn.textContent = "Delete";
  await chrome.storage.local.remove("message");
  saved = null;
  render();
  pushClipToPage();
});

async function durationOf(blob) {
  try {
    const buf = await new AudioContext().decodeAudioData(await blob.arrayBuffer());
    return buf.duration;
  } catch {
    return 0;
  }
}

// ---- Test mode: this page gets the same mic hand-off as a dialer page ----
function pushClipToPage() {
  if (!saved) return window.postMessage({ __sayit: "to-page", type: "clear" }, "*");
  const bin = atob(saved.data);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  window.postMessage({ __sayit: "to-page", type: "clip", bytes: bytes.buffer }, "*", [bytes.buffer]);
}

let testRecorder = null;
let testStream = null;
$("testStart").addEventListener("click", async () => {
  showError("");
  if (!saved) return showError("Save a message first.");
  try {
    testStream = await navigator.mediaDevices.getUserMedia({ audio: true }); // the "call" mic
    const chunks = [];
    testRecorder = new MediaRecorder(testStream);
    testRecorder.ondataavailable = (e) => e.data.size && chunks.push(e.data);
    testRecorder.onstop = () => {
      const tp = $("testPlayer");
      tp.src = URL.createObjectURL(new Blob(chunks, { type: testRecorder.mimeType }));
      tp.hidden = false;
      testStream.getTracks().forEach((t) => t.stop());
      testRecorder = null;
    };
    testRecorder.start();
    $("testStart").hidden = true;
    $("testPlay").hidden = $("testStop").hidden = false;
  } catch (e) {
    showError(`Couldn't start the test: ${e}`);
  }
});
$("testPlay").addEventListener("click", () => window.postMessage({ __sayit: "to-page", type: "toggle" }, "*"));
$("testStop").addEventListener("click", () => {
  if (testRecorder) testRecorder.stop();
  $("testStart").hidden = false;
  $("testPlay").hidden = $("testStop").hidden = true;
});

chrome.runtime.onMessage.addListener((m) => {
  if (m && m.sayit === "toggle" && testRecorder) window.postMessage({ __sayit: "to-page", type: "toggle" }, "*");
});
window.addEventListener("message", (e) => {
  if (e.source !== window || !e.data || e.data.__sayit !== "from-page") return;
  const notes = { "no-message": "No message saved.", "no-call": "Start the test first." };
  if (notes[e.data.state]) showError(notes[e.data.state]);
});

load();
