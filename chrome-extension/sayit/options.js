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

let testRecorder = null;
let testStream = null;
$("testStart").addEventListener("click", async () => {
  showError("");
  const { message } = await chrome.storage.local.get("message");
  if (!message) return showError("Record and save a message in the SayIt popup first.");
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
    "no-message": "No message saved yet.",
    "no-call": "Press Start test first.",
  };
  if (notes[e.data.state] && testRecorder !== undefined) $("testState").textContent = notes[e.data.state];
});
