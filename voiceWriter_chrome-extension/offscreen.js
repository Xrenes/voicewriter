// VoiceWriter offscreen document: records the microphone for dictation, plays
// Read aloud audio and writes to the clipboard on behalf of the background worker.

let recorder = null;
let recStream = null;
let recChunks = [];
let recStartedAt = 0;
let keepAlive = null;

let player = null;
let finishPlayback = null;

function codedError(message, code) {
  const e = new Error(message);
  e.code = code;
  return e;
}

function blobToBase64(blob) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result).split(",")[1] || "");
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(blob);
  });
}

function base64ToBlob(b64, type) {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new Blob([bytes], { type });
}

async function openMic(deviceId) {
  const base = { echoCancellation: true, noiseSuppression: true, autoGainControl: true };
  try {
    return await navigator.mediaDevices.getUserMedia({ audio: deviceId ? { ...base, deviceId: { exact: deviceId } } : base });
  } catch (e) {
    // The chosen microphone was unplugged: fall back to the default one.
    if (deviceId && (e.name === "OverconstrainedError" || e.name === "NotFoundError")) {
      return navigator.mediaDevices.getUserMedia({ audio: base });
    }
    throw e;
  }
}

function releaseMic() {
  clearInterval(keepAlive);
  keepAlive = null;
  if (recStream) recStream.getTracks().forEach((t) => t.stop());
  recStream = null;
}

function stopPlayer(stopped) {
  if (player) {
    player.pause();
    player.removeAttribute("src");
  }
  player = null;
  if (finishPlayback) finishPlayback(stopped);
  finishPlayback = null;
}

const handlers = {
  async "rec-start"({ deviceId }) {
    if (recorder) return {};
    let stream;
    try {
      stream = await openMic(deviceId);
    } catch (e) {
      if (e.name === "NotAllowedError" || e.name === "SecurityError") {
        throw codedError("Microphone access hasn't been allowed yet.", "mic-permission");
      }
      if (e.name === "NotFoundError") throw codedError("No microphone was found.", "no-mic");
      throw e;
    }
    const mimeType = MediaRecorder.isTypeSupported("audio/webm;codecs=opus") ? "audio/webm;codecs=opus" : "audio/webm";
    recStream = stream;
    recChunks = [];
    recorder = new MediaRecorder(stream, { mimeType, audioBitsPerSecond: 64000 });
    recorder.ondataavailable = (e) => {
      if (e.data.size) recChunks.push(e.data);
    };
    recorder.start(1000);
    recStartedAt = performance.now();
    // Messages keep the background worker awake during long dictations.
    keepAlive = setInterval(() => chrome.runtime.sendMessage({ type: "vw-keepalive" }).catch(() => {}), 20000);
    return {};
  },

  async "rec-stop"() {
    if (!recorder) throw codedError("Not recording.", "not-recording");
    const r = recorder;
    recorder = null;
    const durationMs = performance.now() - recStartedAt;
    await new Promise((resolve) => {
      r.onstop = resolve;
      r.stop();
    });
    releaseMic();
    const blob = new Blob(recChunks, { type: "audio/webm" });
    recChunks = [];
    return { audio: blob.size ? await blobToBase64(blob) : "", mime: "audio/webm", durationMs };
  },

  async "rec-cancel"() {
    if (recorder) {
      recorder.ondataavailable = null;
      recorder.stop();
    }
    recorder = null;
    recChunks = [];
    releaseMic();
    return {};
  },

  play({ audio, index = 0, total = 1 }) {
    stopPlayer(true);
    return new Promise((resolve, reject) => {
      const url = URL.createObjectURL(base64ToBlob(audio, "audio/wav"));
      const el = new Audio(url);
      player = el;
      finishPlayback = (stopped) => {
        URL.revokeObjectURL(url);
        resolve({ stopped: !!stopped });
      };
      el.ontimeupdate = () => {
        chrome.runtime
          .sendMessage({ type: "vw-progress", index, total, current: el.currentTime, duration: el.duration || 0 })
          .catch(() => {});
      };
      el.onended = () => {
        if (player === el) {
          player = null;
          const done = finishPlayback;
          finishPlayback = null;
          done?.(false);
        }
      };
      el.onerror = () => {
        if (player === el) {
          player = null;
          finishPlayback = null;
          URL.revokeObjectURL(url);
          reject(new Error("The audio couldn't be played."));
        }
      };
      el.play().catch((e) => {
        if (player === el) {
          player = null;
          finishPlayback = null;
          URL.revokeObjectURL(url);
          reject(e);
        }
      });
    });
  },

  async "stop-audio"() {
    stopPlayer(true);
    return {};
  },

  async copy({ text }) {
    const area = document.getElementById("clip");
    area.value = text;
    area.select();
    const ok = document.execCommand("copy");
    area.value = "";
    if (!ok) throw new Error("Couldn't copy to the clipboard.");
    return {};
  },
};

chrome.runtime.onMessage.addListener((msg, _sender, reply) => {
  if (msg?.target !== "offscreen") return false;
  const handler = handlers[msg.type];
  if (!handler) return false;
  Promise.resolve()
    .then(() => handler(msg))
    .then(
      (result) => reply({ ok: true, ...result }),
      (e) => reply({ ok: false, error: e?.message || String(e), code: e?.code }),
    );
  return true;
});
