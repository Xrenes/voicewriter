// VoiceWriter popup: SayIt first, then Read aloud and the board,
// then the settings (same sections as the desktop app's settings window).
// Every change saves immediately.
const VW = globalThis.VoiceWriterShared;
const S = VW.SAYIT;
const $ = (id) => document.getElementById(id);

let settings = { ...VW.DEFAULTS };
let sayit = { ...S.DEFAULTS }; // SayIt's own settings
let status = { mode: "idle" };
let liveState = { active: false };
let inTab = false;
let capturing = null;
let sayPending = false; // a Read aloud started from this window is in progress

async function save(patch) {
  Object.assign(settings, patch);
  await chrome.storage.local.set(patch);
}

async function saveSayit(patch) {
  sayit = { ...sayit, ...patch };
  await chrome.storage.local.set({ sayit });
  renderSayitSettings();
  renderSayit();
  renderHotkeys();
}

async function send(msg) {
  try {
    return (await chrome.runtime.sendMessage(msg)) || { ok: false, error: "No response from VoiceWriter." };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

function setHint(el, text, cls = "") {
  el.textContent = text;
  el.className = `hint ${cls}`.trim();
}

function showMsg(el, text) {
  el.textContent = text || "";
  el.hidden = !text;
}

// ---------------------------------------------------------------- status

const DOT = {
  listening: "recording",
  transcribing: "transcribing",
  working: "transcribing",
  loading: "transcribing",
  speaking: "speaking",
  error: "error",
};

function currentMode() {
  const mode = status.mode || "idle";
  // An old error shouldn't stick around forever.
  if (mode === "error" && Date.now() - (status.at || 0) > 60000) return "idle";
  return mode;
}

function renderStatus() {
  const mode = currentMode();
  const hasKey = !!settings.apiKey;
  let dot = mode === "idle" ? (hasKey ? "ready" : "") : DOT[mode] || "";
  let text = mode === "idle" ? (hasKey ? "Ready" : "No API key") : status.text || mode;
  if (mode === "idle" && liveState.active) {
    dot = "ready";
    text = "Board open";
  }
  $("statusDot").className = `dot ${dot}`;
  $("statusText").textContent = text;
  $("statusText").title = text;
  renderSayButton(mode);
}

// ---------------------------------------------------------------- SayIt
// Several saved messages (one per project, say), recorded here or uploaded;
// the selected one is what the hotkey plays.

let lib = { messages: [], activeId: "" };
let take = null; // { wav: Uint8Array, secs }: recorded, not saved yet
let recorder = null;
let recStartedAt = 0;
let recTick = 0;
let listening = null; // what the player plays: a message id, or "take"
let takeUrl = null;
let renaming = null; // the message whose name is being edited
let armedDelete = null; // the message whose delete button asks "Delete?"
let armTimer = 0;
let uploading = false;
let rebuilding = false; // the list is being rebuilt (that blurs a rename box)
let callPlaying = false;
let platform = "win";

const MAX_NAME = 60;
const clock = (s) => {
  const t = Math.max(0, Math.round(s));
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
};
const errText = (e) => (e && e.message ? e.message : String(e));
const sayitError = (e) => showMsg($("sayitError"), e ? errText(e) : "");
const sayitNote = (t) => showMsg($("sayitNote"), t || "");
const activeEntry = () => lib.messages.find((m) => m.id === lib.activeId);

async function reloadLibrary() {
  lib = await S.library();
  renderSayit();
}

// ---- Names ----
const cleanName = (raw) =>
  String(raw || "")
    .replace(/[\u0000-\u001f\u007f]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, MAX_NAME)
    .trim();
const nameTaken = (list, name, exceptId) => list.some((m) => m.id !== exceptId && m.name.toLowerCase() === name.toLowerCase());
function defaultName(list) {
  for (let n = list.length + 1; ; n++) if (!nameTaken(list, `Message ${n}`)) return `Message ${n}`;
}
function uniqueName(list, raw, exceptId) {
  const base = cleanName(raw);
  if (!base) return defaultName(list);
  if (!nameTaken(list, base, exceptId)) return base;
  for (let i = 2; ; i++) if (!nameTaken(list, `${base} (${i})`, exceptId)) return `${base} (${i})`;
}

// ---- Changes to the list, one at a time ----
let queue = Promise.resolve();
function change(fn) {
  const run = queue.then(async () => {
    await S.migrate();
    return fn(await S.library());
  });
  queue = run.catch(() => {});
  return run;
}
const newId = (list) => {
  for (;;) {
    const id = `m${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
    if (!list.some((m) => m.id === id)) return id;
  }
};
// A new message plays only if none was picked.
const addMessage = (name, t) =>
  change(async (l) => {
    const id = newId(l.messages);
    const entry = { id, name: uniqueName(l.messages, name), secs: t.secs, added: Date.now() };
    await chrome.storage.local.set({
      [S.audioKey(id)]: { data: bytesToBase64(t.wav), type: "audio/wav" },
      sayitMessages: [...l.messages, entry],
      sayitActiveId: l.activeId || id,
    });
    return entry;
  });
const selectMessage = (id) =>
  change(async (l) => {
    if (!l.messages.some((m) => m.id === id)) throw new Error("that message no longer exists");
    await chrome.storage.local.set({ sayitActiveId: id });
  });
const renameMessage = (id, raw) =>
  change(async (l) => {
    if (!cleanName(raw)) throw new Error("give the message a name");
    const name = uniqueName(l.messages, raw, id);
    await chrome.storage.local.set({ sayitMessages: l.messages.map((m) => (m.id === id ? { ...m, name } : m)) });
  });
// Deleting the one that plays leaves none picked (so a call never gets a
// message you didn't choose), unless a single message is left.
const deleteMessage = (id) =>
  change(async (l) => {
    const list = l.messages.filter((m) => m.id !== id);
    if (list.length === l.messages.length) return;
    let active = l.activeId === id ? "" : l.activeId;
    if (!active && list.length === 1) active = list[0].id;
    await chrome.storage.local.set({ sayitMessages: list, sayitActiveId: active });
    await chrome.storage.local.remove(S.audioKey(id));
  });

// ---- The list ----
function el(tag, className, text) {
  const node = document.createElement(tag);
  node.className = className;
  if (text != null) node.textContent = text;
  return node;
}
function iconButton(text, title, onClick) {
  const b = el("button", "icon", text);
  b.title = title;
  b.setAttribute("aria-label", title);
  b.addEventListener("click", (e) => {
    e.stopPropagation();
    onClick();
  });
  return b;
}

function messageRow(m, active) {
  const li = el("li", active ? "msg on" : "msg");
  li.title = active ? "Calls get this message" : "Click to use this message for calls";
  li.addEventListener("click", () => pick(m.id));
  const dot = el("button", "pick");
  dot.setAttribute("aria-pressed", String(active));
  dot.setAttribute("aria-label", `Use “${m.name}” for calls`);
  dot.addEventListener("click", (e) => {
    e.stopPropagation();
    pick(m.id);
  });
  let label;
  if (renaming === m.id) {
    label = el("input", "rename");
    label.type = "text";
    label.value = m.name;
    label.maxLength = MAX_NAME;
    label.spellcheck = false;
    label.dataset.id = m.id;
    label.addEventListener("click", (e) => e.stopPropagation());
    label.addEventListener("keydown", (e) => {
      if (e.key === "Enter") commitRename(m.id, label.value);
      else if (e.key === "Escape") {
        renaming = null;
        renderSayit();
      }
    });
    label.addEventListener("blur", () => {
      if (!rebuilding) commitRename(m.id, label.value);
    });
  } else {
    label = el("span", "name", m.name);
  }
  const on = listening === m.id;
  const armed = armedDelete === m.id;
  const del = iconButton(armed ? "Delete?" : "✕", armed ? "Click again to delete" : "Delete", () => removeMessage(m.id));
  if (armed) del.classList.add("armed");
  li.append(
    dot,
    label,
    el("span", "dur", clock(m.secs || 0)),
    iconButton(on ? "■" : "▶", on ? "Stop" : "Listen", () => toggleListen(m.id)),
    iconButton("✎", "Rename", () => {
      renaming = m.id;
      armedDelete = null;
      renderSayit();
    }),
    del,
  );
  return li;
}

function renderList() {
  const list = $("sayitList");
  const open = list.querySelector("input.rename");
  const typed = open && open.dataset.id === renaming ? { v: open.value, s: open.selectionStart, e: open.selectionEnd } : null;
  if (renaming && !lib.messages.some((m) => m.id === renaming)) renaming = null;
  rebuilding = true;
  try {
    list.replaceChildren(...lib.messages.map((m) => messageRow(m, m.id === lib.activeId)));
  } finally {
    rebuilding = false;
  }
  const input = list.querySelector("input.rename");
  if (input) {
    if (typed) {
      input.value = typed.v;
      input.setSelectionRange(typed.s, typed.e);
    } else input.select();
    input.focus();
  }
}

function renderClock() {
  $("sayitState").textContent = `● Recording ${clock((Date.now() - recStartedAt) / 1000)}: say your message`;
}

function renderSayit() {
  renderList();
  const recording = !!recorder;
  const active = activeEntry();
  $("sayitEmpty").hidden = lib.messages.length > 0 || recording || !!take;
  $("sayitTake").hidden = !recording && !take;
  $("sayitTakeRow").hidden = recording;
  if (recording) renderClock();
  else if (take) $("sayitState").textContent = `New recording · ${clock(take.secs)} · name it and save`;
  $("sayitState").classList.toggle("live", recording);
  $("sayitListen").textContent = listening === "take" ? "■" : "▶";

  $("sayitRecord").textContent = recording ? "■ Stop" : take ? "● Re-record" : "● Record";
  $("sayitRecord").className = recording || !lib.messages.length ? "primary" : "ghost";
  $("sayitRecord").disabled = uploading;
  $("sayitUpload").disabled = uploading || recording;
  $("sayitUpload").textContent = uploading ? "Adding…" : "Upload audio…";

  $("sayitPlayCall").hidden = recording || (!active && !callPlaying);
  $("sayitPlayCall").textContent = callPlaying ? "■ Stop playing" : `▶ Play “${active ? active.name : ""}” into the call now`;
  const key = S.combo(sayit.hotkey);
  $("sayitHint").textContent = !lib.messages.length
    ? ""
    : !active
      ? "Click the message calls should get."
      : key
        ? `During a call press ${key} on any page to play it. Press again to stop. Click another message to switch.`
        : "Set a SayIt hotkey in Settings → Hotkeys.";
}

async function pick(id) {
  if (id === lib.activeId) return;
  sayitError("");
  try {
    await selectMessage(id);
  } catch (e) {
    sayitError(e);
  }
  await reloadLibrary();
}

async function commitRename(id, value) {
  if (renaming !== id) return; // Enter, then the blur that follows
  renaming = null;
  const current = lib.messages.find((m) => m.id === id);
  if (!current || !value.trim() || value.trim() === current.name) return renderSayit();
  try {
    await renameMessage(id, value);
  } catch (e) {
    sayitError(e);
  }
  await reloadLibrary();
}

async function removeMessage(id) {
  if (armedDelete !== id) {
    armedDelete = id;
    clearTimeout(armTimer);
    armTimer = setTimeout(() => {
      armedDelete = null;
      renderSayit();
    }, 3000);
    return renderSayit();
  }
  armedDelete = null;
  clearTimeout(armTimer);
  if (listening === id) stopListening();
  try {
    await deleteMessage(id);
  } catch (e) {
    sayitError(e);
  }
  await reloadLibrary();
}

function stopListening() {
  $("sayitPlayer").pause();
  const was = listening;
  listening = null;
  if (was) renderSayit();
}

async function toggleListen(id) {
  const key = id || "take";
  if (listening === key) return stopListening();
  stopListening();
  const player = $("sayitPlayer");
  try {
    if (id) {
      const audio = await S.audioOf(id);
      if (!audio) throw new Error("that message's audio is missing. Record or upload it again.");
      player.src = `data:${audio.type || "audio/wav"};base64,${audio.data}`;
    } else {
      if (takeUrl) URL.revokeObjectURL(takeUrl);
      takeUrl = URL.createObjectURL(new Blob([take.wav], { type: "audio/wav" }));
      player.src = takeUrl;
    }
    await player.play();
    listening = key;
    renderSayit();
  } catch (e) {
    sayitError(e);
  }
}

// An uploaded voicemail (MP3, WAV, WebM, OGG, M4A, FLAC: whatever Chrome
// plays). It's already produced, so it's only trimmed, faded and leveled,
// not cleaned up like a recording. 48 kHz mono WAV.
async function processFile(file) {
  const RATE = 48000;
  if (file.size > 50 * 1024 * 1024) throw new Error("it's over 50 MB");
  let decoded;
  try {
    decoded = await new OfflineAudioContext(1, 1, RATE).decodeAudioData(await file.arrayBuffer());
  } catch {
    throw new Error("it isn't an audio file SayIt can read (try MP3, WAV, WebM, OGG or M4A)");
  }
  if (decoded.duration > 600) throw new Error(`it's ${Math.ceil(decoded.duration / 60)} minutes long; a message can be up to 10`);
  const mono = new Float32Array(decoded.length);
  for (let c = 0; c < decoded.numberOfChannels; c++) {
    const d = decoded.getChannelData(c);
    for (let i = 0; i < mono.length; i++) mono[i] += d[i] / decoded.numberOfChannels;
  }
  const out = trimAndLevel(mono, RATE);
  let peak = 0;
  for (const v of out) peak = Math.max(peak, Math.abs(v));
  if (out.length < RATE * 0.3 || peak < 1e-3) throw new Error("there's no sound in it");
  return { wav: encodeWav(out, RATE), secs: out.length / RATE };
}

async function uploadFiles(files) {
  if (!files.length) return;
  stopListening();
  sayitError("");
  uploading = true;
  renderSayit();
  const added = [];
  const failed = [];
  for (let i = 0; i < files.length; i++) {
    const f = files[i];
    sayitNote(files.length > 1 ? `Adding ${i + 1} of ${files.length}: ${f.name}…` : `Adding ${f.name}…`);
    try {
      const entry = await addMessage(f.name.replace(/\.[^.\\/]+$/, ""), await processFile(f));
      added.push(`“${entry.name}”`);
    } catch (e) {
      failed.push(`${f.name} (${errText(e)})`);
    }
  }
  uploading = false;
  sayitNote(added.length ? `Added ${added.join(", ")}.` : "");
  sayitError(failed.length ? `Couldn't add ${failed.join("; ")}.` : "");
  await reloadLibrary();
}

function bindSayit() {
  chrome.runtime.getPlatformInfo().then((p) => (platform = p.os), () => {});

  $("sayitRecord").onclick = async () => {
    sayitError("");
    sayitNote("");
    stopListening();
    if (recorder) {
      recorder.stop();
      return;
    }
    let stream;
    try {
      // The raw microphone: Chrome's live noise removal and echo cancelling
      // can make a recording sound metallic. It's cleaned up afterwards
      // instead (cleanTake), where it can be done gently.
      stream = await navigator.mediaDevices.getUserMedia({
        audio: {
          ...(settings.micDevice ? { deviceId: { ideal: settings.micDevice } } : {}),
          echoCancellation: false,
          noiseSuppression: false,
          autoGainControl: false,
          channelCount: 1,
        },
      });
    } catch {
      $("sayitAllowMic").hidden = false;
      sayitError("Chrome hasn't let VoiceWriter use the microphone yet.");
      return;
    }
    $("sayitAllowMic").hidden = true;
    const chunks = [];
    recorder = new MediaRecorder(stream, { audioBitsPerSecond: 192000 }); // high quality: no codec artifacts
    recorder.ondataavailable = (e) => e.data.size && chunks.push(e.data);
    recorder.onstop = async () => {
      stream.getTracks().forEach((t) => t.stop());
      const type = recorder.mimeType;
      recorder = null;
      clearInterval(recTick);
      $("sayitState").textContent = "Cleaning up the recording…";
      $("sayitState").classList.remove("live");
      try {
        take = await processTake(new Blob(chunks, { type }));
      } catch (e) {
        take = null;
        sayitError(`Couldn't use that recording: ${errText(e)}`);
      }
      renderSayit();
      if (take) {
        $("sayitTakeName").value = defaultName(lib.messages);
        $("sayitTakeName").focus();
        $("sayitTakeName").select();
      }
    };
    take = null;
    recStartedAt = Date.now();
    recorder.start();
    recTick = setInterval(renderClock, 500);
    renderSayit();
  };

  $("sayitListen").onclick = () => toggleListen(null);
  $("sayitPlayer").addEventListener("ended", stopListening);

  const saveTake = async () => {
    if (!take) return;
    stopListening();
    try {
      const entry = await addMessage($("sayitTakeName").value, take);
      take = null;
      sayitNote(`Saved “${entry.name}”.`);
    } catch (e) {
      sayitError(e);
    }
    await reloadLibrary();
  };
  $("sayitSave").onclick = saveTake;
  $("sayitTakeName").addEventListener("keydown", (e) => {
    if (e.key === "Enter") saveTake();
  });
  $("sayitDiscard").onclick = () => {
    stopListening();
    take = null;
    renderSayit();
  };

  // On Windows the file picker opens over the popup. Elsewhere Chrome closes
  // the popup when a picker opens, so the same window opens in a tab first.
  $("sayitUpload").onclick = () => {
    sayitError("");
    sayitNote("");
    if (inTab || platform === "win") $("sayitFile").click();
    else chrome.tabs.create({ url: chrome.runtime.getURL("popup.html#sayit-upload") });
  };
  $("sayitFile").onchange = () => {
    const files = Array.from($("sayitFile").files || []);
    $("sayitFile").value = ""; // picking the same file again still counts
    uploadFiles(files);
  };

  $("sayitPlayCall").onclick = () => chrome.runtime.sendMessage({ sayit: "toggle-calls" }).catch(() => {});
  $("sayitAllowMic").onclick = () => chrome.tabs.create({ url: chrome.runtime.getURL("welcome.html#microphone") });

  // Changes from another VoiceWriter window.
  chrome.storage.onChanged.addListener((changes, area) => {
    if (area === "local" && (changes.sayitMessages || changes.sayitActiveId || changes.sayitMessage)) reloadLibrary();
  });
}

// Mono, cleaned up (cleanTake), trimmed of the silence around it (so it
// starts the instant the hotkey is pressed), faded in and out (no thump),
// leveled to about -1 dBFS, stored as 48 kHz WAV (full quality).
async function processTake(blob) {
  const RATE = 48000;
  const decoded = await new OfflineAudioContext(1, 1, RATE).decodeAudioData(await blob.arrayBuffer());
  const n = decoded.length;
  const mono = new Float32Array(n);
  for (let c = 0; c < decoded.numberOfChannels; c++) {
    const d = decoded.getChannelData(c);
    for (let i = 0; i < n; i++) mono[i] += d[i] / decoded.numberOfChannels;
  }
  const out = trimAndLevel(await cleanTake(mono, RATE), RATE);
  if (out.length < RATE * 0.3) throw new Error("it was too short. Record again.");
  return { wav: encodeWav(out, RATE), secs: out.length / RATE };
}

// A clearer voice and a quiet background, without the metallic sound:
// 1. tone: rumble and the "boom" of a close mic out (a steep cut below
//    100 Hz, 4 dB less around 200 Hz), a little clarity at 3 kHz;
// 2. the room is turned down only BETWEEN words (up to 18 dB), smoothly.
//    Your voice itself is never filtered for noise: removing noise from
//    inside the voice (as Chrome's live noise suppression does, on top of
//    the headset's own) is what makes recordings sound metallic.
async function cleanTake(x, rate) {
  const ctx = new OfflineAudioContext(1, x.length, rate);
  const buf = ctx.createBuffer(1, x.length, rate);
  buf.copyToChannel(x, 0);
  const src = ctx.createBufferSource();
  src.buffer = buf;
  const filter = (type, f, q, g) => {
    const b = ctx.createBiquadFilter();
    b.type = type;
    b.frequency.value = f;
    b.Q.value = q;
    b.gain.value = g;
    return b;
  };
  src
    .connect(filter("highpass", 100, -5.33, 0)) // two stages: 4th-order Butterworth (Q in dB)
    .connect(filter("highpass", 100, 2.32, 0))
    .connect(filter("peaking", 200, 0.8, -4))
    .connect(filter("peaking", 3000, 0.9, 2))
    .connect(ctx.destination);
  src.start();
  return quietPauses((await ctx.startRendering()).getChannelData(0), rate);
}

function quietPauses(x, rate) {
  const frame = Math.round(rate * 0.01); // 10 ms
  const n = Math.floor(x.length / frame);
  if (n < 50) return x;
  const db = new Float64Array(n);
  for (let f = 0; f < n; f++) {
    let a = 0;
    for (let i = f * frame; i < (f + 1) * frame; i++) a += x[i] * x[i];
    db[f] = 10 * Math.log10(a / frame + 1e-12);
  }
  const sorted = Float64Array.from(db).sort();
  const room = sorted[Math.floor(n * 0.1)];
  const voice = sorted[Math.floor(n * 0.9)];
  if (voice - room < 12) return x; // the room is as loud as the voice: leave it alone
  const open = Math.min(room + 10, voice - 20);
  const close = Math.min(room + 4, open - 3);
  const FLOOR = -18;
  // Target gain per frame, looking 30 ms ahead so words start fully open.
  const target = new Float64Array(n);
  for (let f = 0; f < n; f++) {
    let m = -200;
    for (let k = f; k < Math.min(n, f + 4); k++) m = Math.max(m, db[k]);
    const t = Math.min(1, Math.max(0, (m - close) / (open - close)));
    target[f] = FLOOR * (1 - t * t * (3 - 2 * t));
  }
  // Opens fast; holds 150 ms; then closes slowly (no chopped word endings).
  const gain = new Float64Array(n);
  let g = FLOOR;
  let hold = 0;
  for (let f = 0; f < n; f++) {
    if (target[f] >= g) {
      g = g + (target[f] - g) * 0.6;
      if (target[f] > -1) hold = 15;
    } else if (hold > 0) hold--;
    else g = Math.max(target[f], g - 1.2); // -1.2 dB per 10 ms
    gain[f] = g;
  }
  const out = new Float32Array(x.length);
  for (let i = 0; i < x.length; i++) {
    const p = i / frame - 0.5;
    const f0 = Math.max(0, Math.min(n - 1, Math.floor(p)));
    const f1 = Math.min(n - 1, f0 + 1);
    const w = Math.min(1, Math.max(0, p - f0));
    out[i] = x[i] * Math.pow(10, (gain[f0] * (1 - w) + gain[f1] * w) / 20);
  }
  return out;
}

function trimAndLevel(s, rate) {
  const frame = Math.max(1, Math.round(rate / 50)); // 20 ms
  const rms = [];
  for (let i = 0; i < s.length; i += frame) {
    let acc = 0;
    const end = Math.min(s.length, i + frame);
    for (let j = i; j < end; j++) acc += s[j] * s[j];
    rms.push(Math.sqrt(acc / (end - i)));
  }
  let loudest = 0;
  for (const r of rms) loudest = Math.max(loudest, r);
  if (loudest <= 0) return s;
  const voiced = (r) => r >= loudest * 0.06;
  const first = rms.findIndex(voiced);
  let last = rms.length - 1;
  while (last > 0 && !voiced(rms[last])) last--;
  const pad = Math.ceil((rate * 0.02) / frame);
  const start = Math.max(0, first - pad) * frame;
  const end = Math.min(s.length, (last + 1 + pad) * frame);
  const out = s.slice(start, end);
  // 15 ms fades: no click or thump when it starts and stops on the call.
  const fade = Math.min(Math.round(rate * 0.015), Math.floor(out.length / 2));
  for (let i = 0; i < fade; i++) {
    const g = i / fade;
    out[i] *= g;
    out[out.length - 1 - i] *= g;
  }
  let peak = 0;
  for (const v of out) peak = Math.max(peak, Math.abs(v));
  if (peak > 1e-4) {
    const gain = Math.min(0.89 / peak, 8);
    for (let i = 0; i < out.length; i++) out[i] *= gain;
  }
  return out;
}

function encodeWav(samples, rate) {
  const buf = new ArrayBuffer(44 + samples.length * 2);
  const v = new DataView(buf);
  const str = (o, t) => [...t].forEach((c, i) => v.setUint8(o + i, c.charCodeAt(0)));
  str(0, "RIFF");
  v.setUint32(4, 36 + samples.length * 2, true);
  str(8, "WAVE");
  str(12, "fmt ");
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true);
  v.setUint16(22, 1, true);
  v.setUint32(24, rate, true);
  v.setUint32(28, rate * 2, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  str(36, "data");
  v.setUint32(40, samples.length * 2, true);
  for (let i = 0; i < samples.length; i++) v.setInt16(44 + i * 2, Math.max(-1, Math.min(1, samples[i])) * 0x7fff, true);
  return new Uint8Array(buf);
}

function bytesToBase64(bytes) {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

// "calls.aloware.com inside app-na2.hubspot.com"
const placeOf = (where) => (where && where.length > 1 ? `${where[0]} inside ${where[where.length - 1]}` : (where && where[0]) || "");

async function renderCall() {
  let list = [];
  try {
    list = (await chrome.runtime.sendMessage({ sayit: "calls" })) || [];
  } catch {
    /* the background is starting */
  }
  const key = S.combo(sayit.hotkey) || "the SayIt key";
  const playing = list.find((f) => f.playing);
  const live = list.find((f) => f.live > 0);
  let text = "";
  let ok = false;
  const active = activeEntry();
  const what = active ? `“${active.name}”` : "your message";
  if (playing) [text, ok] = [`Playing ${what} · ${placeOf(playing.where)}`, true];
  else if (live) [text, ok] = [`Live call found: ${key} plays it · ${placeOf(live.where)}`, true];
  else if (list.length) text = `Microphone found, no connected call yet · ${placeOf(list[0].where)}`;
  else if (lib.messages.length) text = "No call found yet. Start a call, then look here.";
  const line = $("sayitCall");
  line.textContent = text;
  line.className = `callstate${ok ? " ok" : ""}`;
  line.hidden = !text;
  if (!!playing !== callPlaying) {
    callPlaying = !!playing;
    renderSayit();
  }
}

// ---- SayIt settings ----
let tabOrigin = null; // the active tab's web origin, if any

function renderSayitSettings() {
  $("sayitMute").checked = sayit.muteMic;
  $("sayitHear").checked = sayit.hearIt;
  $("sayitOwnNc").checked = !!sayit.ownNoiseCancel;
  $("sayitEverywhere").checked = sayit.everywhere;
  $("sayitSites").replaceChildren(
    ...sayit.sites.map((origin) => {
      const li = document.createElement("li");
      const name = document.createElement("span");
      name.textContent = S.host(origin);
      const remove = document.createElement("button");
      remove.className = "ghost";
      remove.textContent = "Remove";
      remove.onclick = () => saveSayit({ sites: sayit.sites.filter((o) => o !== origin) });
      li.append(name, remove);
      return li;
    }),
  );
  const add = $("sayitAddSite");
  add.hidden = !tabOrigin || sayit.everywhere || S.builtIn(tabOrigin) || sayit.sites.includes(tabOrigin);
  if (tabOrigin) add.textContent = `+ Add ${S.host(tabOrigin)}`;
}

function bindSayitSettings() {
  $("sayitMute").onchange = (e) => saveSayit({ muteMic: e.target.checked });
  $("sayitHear").onchange = (e) => saveSayit({ hearIt: e.target.checked });
  $("sayitOwnNc").onchange = (e) => saveSayit({ ownNoiseCancel: e.target.checked });
  $("sayitEverywhere").onchange = (e) => saveSayit({ everywhere: e.target.checked });
  $("sayitAddSite").onclick = async () => {
    await saveSayit({ sites: [...sayit.sites, tabOrigin] });
    $("sayitSiteHint").textContent = "Added. SayIt takes the microphone there from the next call on.";
  };
  $("sayitTest").onclick = () => chrome.tabs.create({ url: chrome.runtime.getURL("sayit-test.html") });
}

async function checkTab() {
  if (inTab) return;
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  try {
    const u = new URL(tab?.url || "");
    if (u.protocol === "https:" || u.protocol === "http:") tabOrigin = u.origin;
  } catch {
    /* not a web page */
  }
  renderSayitSettings();
}

// ---------------------------------------------------------------- Read aloud

const PLAY_ICON = '<path d="M2 1l9 5-9 5z" fill="currentColor"/>';
const STOP_ICON = '<rect x="2" y="2" width="8" height="8" rx="1.2" fill="currentColor"/>';

function renderSayButton(mode) {
  const busy = mode === "loading" || mode === "speaking";
  $("sayLabel").textContent = busy ? "Stop" : "Read aloud";
  $("sayIcon").innerHTML = busy ? STOP_ICON : PLAY_ICON;
  const progress = $("sayProgress");
  progress.hidden = !busy;
  progress.classList.toggle("indeterminate", mode === "loading");
  if (mode !== "speaking") $("sayProgressBar").style.width = mode === "loading" ? "" : "0%";
}

function sayHint(text, cls = "") {
  const el = $("sayHint");
  el.textContent = text;
  el.style.color = cls === "err" ? "var(--rec)" : "";
}

async function onSay() {
  const mode = currentMode();
  if (mode === "loading" || mode === "speaking") {
    await send({ type: "stopSpeaking" });
    return;
  }
  const text = $("sayText").value.trim();
  if (!text) {
    sayHint("Type or paste some text first.", "err");
    $("sayText").focus();
    return;
  }
  sayHint("");
  sayPending = true;
  const r = await send({ type: "speak", text });
  if (!r.ok) {
    sayPending = false;
    sayHint(r.error, "err");
  }
}

// Errors from Groq arrive after the request starts, through the status.
function onStatusForSay() {
  if (!sayPending) return;
  if (status.mode === "error") {
    sayHint(status.text || "Something went wrong.", "err");
    sayPending = false;
  } else if (status.mode === "idle") {
    sayPending = false;
  }
}

// Selected text on the current page, "" when nothing is selected, or null
// when VoiceWriter can't reach the page (Chrome's own pages). `flexible`
// also takes a selected spreadsheet cell or the focused text box.
async function pageSelection(flexible = false) {
  if (inTab) return "";
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab?.id) return null;
  try {
    const r = await chrome.tabs.sendMessage(tab.id, { type: "vw-get-selection", flexible });
    return r?.text || "";
  } catch (e) {
    return /Receiving end does not exist/i.test(e.message) ? null : "";
  }
}

function bindReadAloud() {
  const voice = $("voice");
  for (const v of VW.VOICES) voice.append(new Option(v[0].toUpperCase() + v.slice(1), v));
  voice.value = settings.voice;
  voice.onchange = () => save({ voice: voice.value });

  const box = $("sayText");
  box.addEventListener("input", () => {
    sayHint("");
    chrome.storage.session.set({ sayDraft: box.value });
  });
  box.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      onSay();
    }
  });
  $("sayBtn").onclick = onSay;

  $("useSelection").onclick = async () => {
    const text = await pageSelection(true);
    if (text) {
      box.value = text;
      chrome.storage.session.set({ sayDraft: text });
      sayHint("");
    } else if (text === null) {
      sayHint("VoiceWriter can't read this page. Paste the text instead.", "err");
    } else {
      sayHint("Nothing is selected on the page. Select some text or a cell first.", "err");
    }
  };
  if (inTab) $("useSelection").hidden = true;

  chrome.runtime.onMessage.addListener((msg) => {
    if (msg?.type === "vw-progress" && msg.duration) {
      const pct = ((msg.index + msg.current / msg.duration) / msg.total) * 100;
      $("sayProgress").classList.remove("indeterminate");
      $("sayProgressBar").style.width = `${Math.min(100, pct)}%`;
    }
    return false;
  });
}

async function prefillSayText() {
  const { sayDraft = "" } = await chrome.storage.session.get("sayDraft");
  const box = $("sayText");
  box.value = sayDraft;
  const selected = await pageSelection();
  if (selected) {
    box.value = selected;
    chrome.storage.session.set({ sayDraft: selected });
  }
}

// ---------------------------------------------------------------- the board

function renderLiveCard() {
  const btn = $("liveBtn");
  btn.textContent = liveState.active ? "Close" : "Open";
  btn.className = liveState.active ? "ghost" : "primary";
  $("liveKey").textContent = settings.hotkeys.live || "Alt+A";
}

function bindLive() {
  $("liveBtn").onclick = async () => {
    showMsg($("liveError"), "");
    const r = await send({ type: liveState.active ? "liveStop" : "liveStart" });
    if (!r.ok) showMsg($("liveError"), r.error);
  };
  const range = $("liveTransparency");
  range.value = settings.liveTransparency;
  $("liveTransparencyVal").textContent = `${settings.liveTransparency}%`;
  range.oninput = () => {
    $("liveTransparencyVal").textContent = `${range.value}%`;
    save({ liveTransparency: Number(range.value) });
  };
}

// ---------------------------------------------------------------- hotkeys

// Chrome handles these itself before a page ever sees them.
const RESERVED = new Set([
  "Control+T",
  "Control+N",
  "Control+W",
  "Control+L",
  "Control+Tab",
  "Control+Shift+N",
  "Control+Shift+T",
  "Control+Shift+W",
  "Control+Shift+Tab",
  "Alt+D",
  "Alt+E",
  "Alt+F",
  "Alt+F4",
  "Alt+Home",
  "Alt+ArrowLeft",
  "Alt+ArrowRight",
]);
const HOTKEY_HELP = "Click Change, then press the new keys. Esc cancels, Backspace turns the shortcut off.";

// Every shortcut in use, by action, including SayIt's.
const allCombos = () => ({ ...settings.hotkeys, sayit: S.combo(sayit.hotkey) });

function renderHotkeys() {
  document.querySelectorAll("[data-hotkey]").forEach((input) => {
    const action = input.dataset.hotkey;
    if (capturing === action) return;
    input.value = settings.hotkeys[action] || "Off";
    input.classList.remove("capturing");
  });
  if (capturing !== "sayit") {
    $("sayitHotkey").value = S.combo(sayit.hotkey) || "Off";
    $("sayitHotkey").classList.remove("capturing");
  }
  document.querySelectorAll("[data-change]").forEach((b) => {
    b.textContent = capturing === b.dataset.change ? "Cancel" : "Change";
  });
  document.querySelectorAll("kbd[data-key]").forEach((k) => {
    k.textContent = settings.hotkeys[k.dataset.key] || "(off)";
  });
  const sayitKey = S.combo(sayit.hotkey) || "(off)";
  $("sayitKey").textContent = sayitKey;
  $("guideSayitKey").textContent = sayitKey;
  $("sayKeyName").textContent = settings.hotkeys.speak || "the Read aloud hotkey (currently off)";
  renderLiveCard();
}

function captureInput(action) {
  return action === "sayit" ? $("sayitHotkey") : document.querySelector(`[data-hotkey="${action}"]`);
}

function startCapture(action) {
  capturing = action;
  const input = captureInput(action);
  input.value = "Press keys…";
  input.classList.add("capturing");
  setHint($("hotkeyMsg"), `Press the new shortcut for ${VW.LABELS[action]}.`);
  renderHotkeys();
}

function stopCapture(message = HOTKEY_HELP, cls = "") {
  capturing = null;
  setHint($("hotkeyMsg"), message, cls);
  renderHotkeys();
}

async function setHotkey(action, combo, e) {
  if (action === "sayit") {
    const hotkey = combo
      ? { code: e.code, key: VW.keyName(e.code), alt: e.altKey, ctrl: e.ctrlKey, shift: e.shiftKey, meta: e.metaKey }
      : { code: "", key: "", alt: false, ctrl: false, shift: false, meta: false };
    capturing = null;
    await saveSayit({ hotkey });
  } else {
    await save({ hotkeys: { ...settings.hotkeys, [action]: combo } });
  }
  stopCapture(combo ? `${VW.LABELS[action]} is now ${combo}.` : `${VW.LABELS[action]} shortcut is off.`, "good");
}

function onCaptureKey(e) {
  if (!capturing) return;
  e.preventDefault();
  e.stopPropagation();
  if (e.key === "Escape") {
    stopCapture();
    return;
  }
  if ((e.key === "Backspace" || e.key === "Delete") && !e.ctrlKey && !e.altKey && !e.metaKey) {
    setHotkey(capturing, "", e);
    return;
  }
  const combo = VW.comboFromEvent(e);
  if (!combo) return; // only modifiers so far
  const main = combo.split("+").pop();
  if (!e.ctrlKey && !e.altKey && !e.metaKey && !/^F\d+$/.test(main)) {
    setHint($("hotkeyMsg"), "Use Control, Alt or Meta together with a key, so normal typing isn't affected.", "warn");
    return;
  }
  if (RESERVED.has(combo)) {
    setHint($("hotkeyMsg"), `${combo} belongs to Chrome itself. Pick another.`, "warn");
    return;
  }
  const combos = allCombos();
  const clash = Object.keys(combos).find((a) => a !== capturing && combos[a] === combo);
  if (clash) {
    setHint($("hotkeyMsg"), `${combo} is already used for ${VW.LABELS[clash]}.`, "warn");
    return;
  }
  setHotkey(capturing, combo, e);
}

// The SayIt and board keys work with this window open too.
function onPopupHotkey(e) {
  if (capturing || e.defaultPrevented) return;
  const combo = VW.comboFromEvent(e);
  if (!combo) return;
  if (combo === S.combo(sayit.hotkey)) {
    e.preventDefault();
    if (!e.repeat) chrome.runtime.sendMessage({ sayit: "toggle-calls" }).catch(() => {});
  } else if (combo === settings.hotkeys.live) {
    e.preventDefault();
    if (!e.repeat) $("liveBtn").click();
  }
}

async function renderCommands() {
  try {
    const cmds = await chrome.commands.getAll();
    const key = (name) => cmds.find((c) => c.name === name)?.shortcut || "not set";
    $("cmdLive").textContent = key("live-toggle");
    $("cmdSayit").textContent = key("sayit-toggle");
  } catch {
    /* older Chrome */
  }
}

function bindHotkeys() {
  document.querySelectorAll("[data-change]").forEach((b) => {
    b.onclick = () => (capturing === b.dataset.change ? stopCapture() : startCapture(b.dataset.change));
  });
  document.addEventListener("keydown", onCaptureKey, true);
  document.addEventListener("keydown", onPopupHotkey);
  $("hotkeysReset").onclick = async () => {
    await save({ hotkeys: { ...VW.DEFAULTS.hotkeys } });
    await saveSayit({ hotkey: { ...S.DEFAULTS.hotkey } });
    stopCapture("Shortcuts reset to the defaults.", "good");
  };
  $("openShortcuts").onclick = () => chrome.tabs.create({ url: "chrome://extensions/shortcuts" });
}

// ---------------------------------------------------------------- API keys

const mask = (k) => (k.length > 12 ? `${k.slice(0, 4)}…${k.slice(-4)}` : "••••");

function bindKeyCard(prefix, field) {
  const el = (suffix) => $(prefix + suffix);
  const hint = el("Hint");
  const defaultHint = hint.innerHTML;
  const resetHint = () => {
    hint.innerHTML = defaultHint;
    hint.className = "hint";
  };

  const render = () => {
    const key = settings[field];
    const st = el("Status");
    st.textContent = key ? mask(key) : field === "speakKey" ? "Uses main key" : "Not set";
    st.classList.toggle("ok", !!key);
    el("Clear").hidden = !key;
    el("Replace").textContent = key ? "Replace" : "Add key";
    el("Test").disabled = !(key || (field === "speakKey" && settings.apiKey));
  };

  const test = async () => {
    const key = settings[field] || (field === "speakKey" ? settings.apiKey : "");
    setHint(hint, "Testing…");
    const r = await send({ type: "testKey", key });
    setHint(hint, r.ok ? `✓ ${r.result}` : r.error, r.ok ? "good" : "err");
  };

  const doSave = async () => {
    const value = el("Input").value.trim();
    if (!value) {
      el("Input").focus();
      return;
    }
    await save({ [field]: value });
    el("Input").value = "";
    el("EditRow").hidden = true;
    renderAll();
    await test();
    if (!value.startsWith("gsk_") && hint.classList.contains("good")) {
      setHint(hint, "Saved. Groq keys usually start with gsk_, so double-check it if requests fail.", "warn");
    }
  };

  el("Replace").onclick = () => {
    const row = el("EditRow");
    row.hidden = !row.hidden;
    if (!row.hidden) el("Input").focus();
  };
  el("Save").onclick = doSave;
  el("Input").addEventListener("keydown", (e) => {
    if (e.key === "Enter") doSave();
  });
  el("Test").onclick = test;
  el("Clear").onclick = async () => {
    await save({ [field]: "" });
    resetHint();
    renderAll();
  };
  return render;
}

let renderMainKey = () => {};
let renderSpeakKey = () => {};

function renderNotice() {
  $("setupNotice").hidden = !!settings.apiKey;
}

// ---------------------------------------------------------------- usage

async function renderUsage() {
  const { usage = {} } = await chrome.storage.local.get("usage");
  const today = VW.today();
  const now = Date.now();
  const show = (kind, prefix) => {
    const u = usage[kind]?.day === today ? usage[kind] : null;
    const limit = VW.LIMITS[kind];
    const count = u?.count || 0;
    const rpm = (u?.recent || []).filter((t) => now - t < 60000).length;
    const frac = Math.min(1, Math.max(count / limit.requests, limit.audioSec ? (u?.audioSec || 0) / limit.audioSec : 0));
    $(`${prefix}Today`).textContent = count;
    $(`${prefix}Rpm`).textContent = rpm;
    $(`${prefix}Pct`).textContent = `${frac > 0 && frac < 0.1 ? (frac * 100).toFixed(1) : Math.round(frac * 100)}%`;
    const fill = $(`${prefix}Fill`);
    fill.style.width = `${frac * 100}%`;
    fill.className = `meter-fill${frac >= 0.9 ? " crit" : frac >= 0.7 ? " warn" : ""}`;
  };
  show("dictation", "dict");
  show("speak", "speak");
}

// ---------------------------------------------------------------- microphone

async function renderMic() {
  let state = "prompt";
  try {
    state = (await navigator.permissions.query({ name: "microphone" })).state;
  } catch {
    /* older Chrome */
  }
  const st = $("micStatus");
  st.textContent = state === "granted" ? "Allowed" : state === "denied" ? "Blocked" : "Not allowed yet";
  st.classList.toggle("ok", state === "granted");
  $("micAllowRow").hidden = state === "granted";

  const select = $("micDevice");
  if (state === "granted") {
    try {
      const devices = (await navigator.mediaDevices.enumerateDevices()).filter(
        (d) => d.kind === "audioinput" && d.deviceId !== "default" && d.deviceId !== "communications",
      );
      select.replaceChildren(new Option("System default", ""), ...devices.map((d, i) => new Option(d.label || `Microphone ${i + 1}`, d.deviceId)));
    } catch {
      /* keep the default option */
    }
  }
  select.disabled = state !== "granted";
  select.value = settings.micDevice;
  if (select.value !== settings.micDevice) select.value = "";
}

// ---------------------------------------------------------------- the rest

function bindFields() {
  const model = $("sttModel");
  model.value = settings.sttModel;
  model.onchange = () => save({ sttModel: model.value });

  const lang = $("language");
  lang.value = settings.language === "auto" ? "" : settings.language;
  lang.onchange = () => save({ language: lang.value.trim().toLowerCase() || "auto" });

  $("micDevice").onchange = () => save({ micDevice: $("micDevice").value });
  $("micAllow").onclick = () => chrome.tabs.create({ url: chrome.runtime.getURL("welcome.html#microphone") });

  const insertion = $("insertion");
  insertion.value = settings.insertion;
  insertion.onchange = () => save({ insertion: insertion.value });

  const polish = $("polish");
  polish.checked = settings.polish;
  polish.onchange = () => save({ polish: polish.checked });

  $("openWelcome").onclick = () => chrome.tabs.create({ url: chrome.runtime.getURL("welcome.html") });

  $("setupAddKey").onclick = () => {
    $("secKeys").open = true;
    $("apiKeyEditRow").hidden = false;
    $("apiKeyInput").focus();
    $("mainKeyCard").scrollIntoView({ behavior: "smooth", block: "center" });
  };

  // Remember which sections were open (a per-browser convenience only).
  document.querySelectorAll("details.section").forEach((d) => {
    try {
      if (localStorage.getItem(`open:${d.id}`) === "1") d.open = true;
    } catch {
      /* storage unavailable */
    }
    d.addEventListener("toggle", () => {
      try {
        localStorage.setItem(`open:${d.id}`, d.open ? "1" : "0");
      } catch {
        /* storage unavailable */
      }
    });
  });
}

function renderAll() {
  renderStatus();
  renderNotice();
  renderHotkeys();
  renderMainKey();
  renderSpeakKey();
  renderSayit();
}

async function init() {
  inTab = !!(await chrome.tabs.getCurrent());
  document.documentElement.classList.toggle("in-tab", inTab);
  settings = await VW.loadSettings();
  sayit = await S.settings();
  await S.migrate().catch(() => {});
  lib = await S.library();
  status = (await chrome.storage.session.get("vwStatus")).vwStatus || { mode: "idle" };
  liveState = (await chrome.storage.session.get("vwLive")).vwLive || { active: false };
  $("version").textContent = `v${chrome.runtime.getManifest().version}`;

  bindSayit();
  bindSayitSettings();
  bindReadAloud();
  bindLive();
  bindHotkeys();
  renderMainKey = bindKeyCard("apiKey", "apiKey");
  renderSpeakKey = bindKeyCard("speakKey", "speakKey");
  bindFields();
  renderAll();
  renderSayitSettings();
  renderUsage();
  renderMic();
  renderCommands();
  renderCall();
  checkTab();
  prefillSayText();

  chrome.storage.onChanged.addListener(async (changes, area) => {
    if (area === "session") {
      if (changes.vwStatus) {
        status = changes.vwStatus.newValue || { mode: "idle" };
        renderStatus();
        onStatusForSay();
      }
      if (changes.vwLive) {
        liveState = changes.vwLive.newValue || { active: false };
        renderLiveCard();
        renderStatus();
      }
      return;
    }
    if (area !== "local") return;
    if (changes.usage) renderUsage();
    if (changes.sayit) {
      sayit = await S.settings();
      renderSayitSettings();
      renderSayit();
      renderHotkeys();
    }
    if (Object.keys(changes).some((k) => k in VW.DEFAULTS)) {
      settings = await VW.loadSettings();
      renderAll();
    }
  });
  setInterval(() => {
    renderUsage();
    renderStatus();
  }, 5000);
  setInterval(renderCall, 1000);
}

init();
