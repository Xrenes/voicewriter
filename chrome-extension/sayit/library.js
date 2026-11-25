// Adding, naming, picking and deleting SayIt messages, and turning recordings
// and uploaded files into them. Used by the popup and the extension's page
// (with common.js loaded first).

const SAYIT_RATE = 24000; // messages are stored at this rate: plenty for a call
const SAYIT_MAX_UPLOAD_MB = 50;
const SAYIT_MAX_SECS = 600;
const SAYIT_MAX_NAME = 60;
const SAYIT_ACCEPT = "audio/*,.mp3,.wav,.webm,.ogg,.oga,.opus,.m4a,.aac,.flac";

// ---- Names ----
function sayitCleanName(raw) {
  return String(raw || "")
    .replace(/[\u0000-\u001f\u007f]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, SAYIT_MAX_NAME)
    .trim();
}

const sayitNameTaken = (list, name, exceptId) =>
  list.some((m) => m.id !== exceptId && m.name.toLowerCase() === name.toLowerCase());

// "Message 1", "Message 2"…: the suggested name for a new recording.
function sayitDefaultName(list) {
  for (let n = list.length + 1; ; n++) if (!sayitNameTaken(list, `Message ${n}`)) return `Message ${n}`;
}

// The name, cleaned, or "Name (2)", "Name (3)"… when another message has it.
function sayitUniqueName(list, raw, exceptId) {
  const base = sayitCleanName(raw);
  if (!base) return sayitDefaultName(list);
  if (!sayitNameTaken(list, base, exceptId)) return base;
  for (let i = 2; ; i++) if (!sayitNameTaken(list, `${base} (${i})`, exceptId)) return `${base} (${i})`;
}

const sayitBaseName = (fileName) => String(fileName).replace(/\.[^.\\/]+$/, "");

// ---- Changes to the list (one at a time, so two quick ones can't undo each other) ----
let sayitQueue = Promise.resolve();
function sayitChange(fn) {
  const run = sayitQueue.then(async () => {
    await sayitMigrate();
    return fn(await sayitLibrary());
  });
  sayitQueue = run.catch(() => {});
  return run;
}

function sayitNewId(list) {
  for (;;) {
    const id = `m${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
    if (!list.some((m) => m.id === id)) return id;
  }
}

// Save { wav: Uint8Array, secs } as a new message; returns its entry. It's
// the one that plays only if none was picked.
function sayitAdd(name, take) {
  return sayitChange(async (lib) => {
    const id = sayitNewId(lib.messages);
    const entry = { id, name: sayitUniqueName(lib.messages, name), secs: take.secs, added: Date.now() };
    await chrome.storage.local.set({
      [sayitAudioKey(id)]: { data: sayitBytesToBase64(take.wav), type: "audio/wav" },
      messages: [...lib.messages, entry],
      activeId: lib.activeId || id,
    });
    return entry;
  });
}

function sayitSelect(id) {
  return sayitChange(async (lib) => {
    if (!lib.messages.some((m) => m.id === id)) throw new Error("that message no longer exists");
    await chrome.storage.local.set({ activeId: id });
  });
}

function sayitRename(id, raw) {
  return sayitChange(async (lib) => {
    if (!sayitCleanName(raw)) throw new Error("give the message a name");
    if (!lib.messages.some((m) => m.id === id)) throw new Error("that message no longer exists");
    const name = sayitUniqueName(lib.messages, raw, id);
    await chrome.storage.local.set({ messages: lib.messages.map((m) => (m.id === id ? { ...m, name } : m)) });
    return name;
  });
}

// If it was the one that plays, none does afterwards (so a call never gets a
// message you didn't pick), unless a single message is left.
function sayitDelete(id) {
  return sayitChange(async (lib) => {
    const list = lib.messages.filter((m) => m.id !== id);
    if (list.length === lib.messages.length) return;
    let active = lib.activeId === id ? "" : lib.activeId;
    if (!active && list.length === 1) active = list[0].id;
    await chrome.storage.local.set({ messages: list, activeId: active });
    await chrome.storage.local.remove(sayitAudioKey(id));
  });
}

// ---- Audio: recordings and uploaded files become 24 kHz mono WAV, trimmed of
// the silence around them (so they start the instant the hotkey is pressed)
// and leveled to about -1 dBFS ----
async function sayitDecodeMono(arrayBuffer) {
  // Decoding resamples to the context's rate; channels are mixed down here.
  const audio = await new OfflineAudioContext(1, 1, SAYIT_RATE).decodeAudioData(arrayBuffer);
  const mono = new Float32Array(audio.length);
  for (let c = 0; c < audio.numberOfChannels; c++) {
    const d = audio.getChannelData(c);
    for (let i = 0; i < mono.length; i++) mono[i] += d[i] / audio.numberOfChannels;
  }
  return { mono, secs: audio.duration };
}

function sayitTrimAndLevel(s, rate) {
  const frame = Math.max(1, Math.round(rate / 50)); // 20 ms
  const rms = [];
  let loudest = 0;
  for (let i = 0; i < s.length; i += frame) {
    let acc = 0;
    const end = Math.min(s.length, i + frame);
    for (let j = i; j < end; j++) acc += s[j] * s[j];
    const r = Math.sqrt(acc / (end - i));
    rms.push(r);
    if (r > loudest) loudest = r;
  }
  if (loudest <= 0) return s;
  const voiced = (r) => r >= loudest * 0.06;
  const first = rms.findIndex(voiced);
  let last = rms.length - 1;
  while (last > 0 && !voiced(rms[last])) last--;
  const pad = Math.ceil((rate * 0.02) / frame);
  const start = Math.max(0, first - pad) * frame;
  const end = Math.min(s.length, (last + 1 + pad) * frame);
  const out = s.slice(start, end);
  let peak = 0;
  for (const v of out) peak = Math.max(peak, Math.abs(v));
  if (peak > 1e-4) {
    const gain = Math.min(0.89 / peak, 8);
    for (let i = 0; i < out.length; i++) out[i] *= gain;
  }
  return out;
}

function sayitEncodeWav(samples, rate) {
  const buf = new ArrayBuffer(44 + samples.length * 2);
  const v = new DataView(buf);
  const str = (o, t) => [...t].forEach((c, i) => v.setUint8(o + i, c.charCodeAt(0)));
  str(0, "RIFF");
  v.setUint32(4, 36 + samples.length * 2, true);
  str(8, "WAVE");
  str(12, "fmt ");
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true); // PCM
  v.setUint16(22, 1, true); // mono
  v.setUint32(24, rate, true);
  v.setUint32(28, rate * 2, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  str(36, "data");
  v.setUint32(40, samples.length * 2, true);
  for (let i = 0; i < samples.length; i++) {
    v.setInt16(44 + i * 2, Math.max(-1, Math.min(1, samples[i])) * 0x7fff, true);
  }
  return new Uint8Array(buf);
}

function sayitBytesToBase64(bytes) {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  }
  return btoa(bin);
}

function sayitToTake(samples) {
  return { wav: sayitEncodeWav(samples, SAYIT_RATE), secs: samples.length / SAYIT_RATE };
}

// A recording from the popup.
async function sayitFromRecording(blob) {
  const { mono } = await sayitDecodeMono(await blob.arrayBuffer());
  const out = sayitTrimAndLevel(mono, SAYIT_RATE);
  if (out.length < SAYIT_RATE * 0.3) throw new Error("it was too short — record again");
  return sayitToTake(out);
}

// An uploaded voicemail: MP3, WAV, WebM, OGG, M4A, FLAC — whatever Chrome plays.
async function sayitFromFile(file) {
  if (file.size > SAYIT_MAX_UPLOAD_MB * 1024 * 1024) throw new Error(`it's over ${SAYIT_MAX_UPLOAD_MB} MB`);
  let decoded;
  try {
    decoded = await sayitDecodeMono(await file.arrayBuffer());
  } catch {
    throw new Error("it isn't an audio file SayIt can read (try MP3, WAV, WebM, OGG or M4A)");
  }
  if (decoded.secs > SAYIT_MAX_SECS) {
    throw new Error(`it's ${Math.ceil(decoded.secs / 60)} minutes long; a message can be up to ${SAYIT_MAX_SECS / 60}`);
  }
  const out = sayitTrimAndLevel(decoded.mono, SAYIT_RATE);
  let peak = 0;
  for (const v of out) peak = Math.max(peak, Math.abs(v));
  if (out.length < SAYIT_RATE * 0.3 || peak < 1e-3) throw new Error("there's no sound in it");
  return sayitToTake(out);
}

// Add several files, each named after itself. `progress` gets a line per file.
async function sayitAddFiles(files, progress) {
  const added = [];
  const failed = [];
  for (let i = 0; i < files.length; i++) {
    const f = files[i];
    if (progress) progress(files.length > 1 ? `Adding ${i + 1} of ${files.length}: ${f.name}…` : `Adding ${f.name}…`);
    try {
      const entry = await sayitAdd(sayitBaseName(f.name), await sayitFromFile(f));
      added.push(entry.name);
    } catch (e) {
      failed.push(`${f.name} (${e && e.message ? e.message : e})`);
    }
  }
  return { added, failed };
}

// "Added “A”, “B”." / "Couldn't add x.mp4 (…)."
function sayitAddFilesReport({ added, failed }) {
  return {
    note: added.length ? `Added ${added.map((n) => `“${n}”`).join(", ")}.` : "",
    error: failed.length ? `Couldn't add ${failed.join("; ")}.` : "",
  };
}
