const $ = (id) => document.getElementById(id);
const showError = (e) => ($("error").textContent = e ? String(e) : "");

if (location.hash === "#mic") $("micSection").hidden = false;
$("allowMic").addEventListener("click", async () => {
  try {
    const s = await navigator.mediaDevices.getUserMedia({ audio: true });
    s.getTracks().forEach((t) => t.stop());
    $("micOk").hidden = false;
    showError("");
  } catch (e) {
    showError(`The microphone is still blocked: ${e}. Allow it from the icon in the address bar.`);
  }
});

sayitSettings().then((s) => ($("hotkey").textContent = sayitHotkeyLabel(s.hotkey)));

// ---- Upload voicemails (also where the popup sends you on Mac and Linux,
// where Chrome closes the popup when a file picker opens) ----
$("file").accept = SAYIT_ACCEPT;
if (location.hash === "#upload") {
  $("uploadSection").classList.add("flash");
  $("uploadSection").scrollIntoView();
}

async function renderLibrary() {
  const lib = await sayitLibrary();
  const active = lib.messages.find((m) => m.id === lib.activeId);
  const names = lib.messages.map((m) => (m.id === lib.activeId ? `“${m.name}” (calls get this one)` : `“${m.name}”`));
  $("libLine").textContent = !lib.messages.length
    ? "No messages yet."
    : `Your messages: ${names.join(", ")}.` + (active ? "" : " Pick the one calls get in the SayIt popup.");
}

let uploading = false;
async function upload(files) {
  if (uploading || !files.length) return;
  uploading = true;
  $("uploadBtn").disabled = true;
  $("uploadError").textContent = "";
  const report = sayitAddFilesReport(await sayitAddFiles(files, (line) => ($("uploadNote").textContent = line)));
  $("uploadNote").textContent = report.note;
  $("uploadError").textContent = report.error;
  $("uploadBtn").disabled = false;
  uploading = false;
}

$("uploadBtn").addEventListener("click", () => $("file").click());
$("file").addEventListener("change", () => {
  const files = Array.from($("file").files || []);
  $("file").value = "";
  upload(files);
});
const drop = $("drop");
drop.addEventListener("dragover", (e) => {
  e.preventDefault();
  drop.classList.add("over");
});
drop.addEventListener("dragleave", () => drop.classList.remove("over"));
drop.addEventListener("drop", (e) => {
  e.preventDefault();
  drop.classList.remove("over");
  upload(Array.from(e.dataTransfer.files || []));
});
chrome.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && (changes.messages || changes.activeId || changes.message)) renderLibrary();
});
sayitMigrate()
  .catch(() => {})
  .then(renderLibrary);

// ---- Test it ----
let testRecorder = null;
let testStream = null;
$("testStart").addEventListener("click", async () => {
  showError("");
  const lib = await sayitLibrary();
  if (!lib.activeId) {
    return showError(
      lib.messages.length
        ? "Pick the message calls get in the SayIt popup first."
        : "Record or upload a message first (SayIt button in the toolbar, or above).",
    );
  }
  try {
    testStream = await navigator.mediaDevices.getUserMedia({ audio: true }); // the "call" microphone
  } catch (e) {
    return showError(`Couldn't use the microphone: ${e}`);
  }
  const chunks = [];
  testRecorder = new MediaRecorder(testStream);
  testRecorder.ondataavailable = (e) => e.data.size && chunks.push(e.data);
  testRecorder.onstop = () => {
    const player = $("testPlayer");
    player.src = URL.createObjectURL(new Blob(chunks, { type: testRecorder.mimeType }));
    player.hidden = false;
    testStream.getTracks().forEach((t) => t.stop());
    testRecorder = null;
    $("testState").textContent = "Here's what the call heard:";
  };
  testRecorder.start();
  $("testStart").hidden = true;
  $("testStop").hidden = false;
  $("testState").textContent = "Test running — talk, then press your hotkey.";
});

$("testStop").addEventListener("click", () => {
  if (testRecorder) testRecorder.stop();
  $("testStart").hidden = false;
  $("testStop").hidden = true;
});

window.addEventListener("message", (e) => {
  if (e.source !== window || !e.data || e.data.__sayit !== "state") return;
  const notes = {
    playing: "Playing your message…",
    done: "Message finished.",
    stopped: "Message stopped.",
    "no-message": "No message to play — record, upload or pick one in the SayIt popup.",
    "no-call": "Press Start test first.",
  };
  if (notes[e.data.state] && testRecorder !== undefined) $("testState").textContent = notes[e.data.state];
});
