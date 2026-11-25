// SayIt test page: a pretend call. The page's microphone is SayIt's (this
// page loads sayit-inject.js like a dialer would), so recording it captures
// exactly what a call would hear.
(() => {
  const VW = globalThis.VoiceWriterShared;
  const $ = (id) => document.getElementById(id);
  const showError = (e) => {
    $("error").textContent = e ? String(e) : "";
    $("error").hidden = !e;
  };

  VW.SAYIT.settings().then((s) => ($("hotkey").textContent = VW.SAYIT.combo(s.hotkey) || "your SayIt key"));

  let recorder = null;
  let stream = null;
  $("testStart").onclick = async () => {
    showError("");
    const lib = await VW.SAYIT.library();
    if (!lib.activeId) {
      showError(
        lib.messages.length
          ? "Pick the message calls get in the VoiceWriter window's SayIt card first."
          : "Record or upload a message in the VoiceWriter window's SayIt card first.",
      );
      return;
    }
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: true }); // the "call" microphone
    } catch (e) {
      showError(`Couldn't use the microphone: ${e.message || e}. Allow it on VoiceWriter's setup page first.`);
      return;
    }
    const chunks = [];
    recorder = new MediaRecorder(stream);
    recorder.ondataavailable = (e) => e.data.size && chunks.push(e.data);
    recorder.onstop = () => {
      const player = $("testPlayer");
      player.src = URL.createObjectURL(new Blob(chunks, { type: recorder.mimeType }));
      player.hidden = false;
      stream.getTracks().forEach((t) => t.stop());
      recorder = null;
      $("testState").textContent = "Here's what the call heard (step 2).";
    };
    recorder.start();
    $("testStart").hidden = true;
    $("testStop").hidden = false;
    $("testState").textContent = "Test running. Talk, then press your SayIt key.";
  };

  $("testStop").onclick = () => {
    if (recorder) recorder.stop();
    $("testStart").hidden = false;
    $("testStop").hidden = true;
  };

  const notes = {
    playing: "Playing your message…",
    done: "Message finished.",
    stopped: "Message stopped.",
    "no-message": "No message to play. Record, upload or pick one in the SayIt card.",
    "no-call": "Press Start test first.",
  };
  window.addEventListener("message", (e) => {
    if (e.source !== window || !e.data || e.data.__vwsayit !== "state") return;
    if (notes[e.data.state]) $("testState").textContent = notes[e.data.state];
  });
})();
