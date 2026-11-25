const $ = (id) => document.getElementById(id);
const els = {
  lib: $("lib"),
  empty: $("empty"),
  record: $("record"),
  upload: $("upload"),
  file: $("file"),
  takeBox: $("takeBox"),
  state: $("state"),
  takeRow: $("takeRow"),
  takeName: $("takeName"),
  takeListen: $("takeListen"),
  save: $("save"),
  discard: $("discard"),
  playCall: $("playCall"),
  hint: $("hint"),
  note: $("note"),
  allowMic: $("allowMic"),
  error: $("error"),
  player: $("player"),
};
els.file.accept = SAYIT_ACCEPT;

let settings = { ...SAYIT_DEFAULTS };
let lib = { messages: [], activeId: "" };
let take = null; // { wav: Uint8Array, secs } — recorded, not saved yet
let recorder = null;
let startedAt = 0;
let tick = 0;
let listening = null; // what the player plays: a message id, or "take"
let blobUrl = null;
let renaming = null; // the message whose name is being edited
let armedDelete = null; // the message whose delete button asks "Delete?"
let armTimer = 0;
let uploading = false;
let rebuilding = false; // the list is being rebuilt (that blurs a rename box)
let callPlaying = false; // a call is playing the message right now
let os = "win";

const clock = (s) => {
  const t = Math.max(0, Math.round(s));
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
};
const errText = (e) => (e && e.message ? e.message : String(e));
const showError = (e) => (els.error.textContent = e ? errText(e) : "");
const showNote = (text) => (els.note.textContent = text || "");
const activeEntry = () => lib.messages.find((m) => m.id === lib.activeId);

async function reloadLibrary() {
  lib = await sayitLibrary();
  render();
}

// ---- The message list ----
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
  li.addEventListener("click", () => select(m.id));

  const pick = el("button", "pick");
  pick.setAttribute("aria-pressed", String(active));
  pick.setAttribute("aria-label", `Use “${m.name}” for calls`);
  pick.addEventListener("click", (e) => {
    e.stopPropagation();
    select(m.id);
  });

  let label;
  if (renaming === m.id) {
    label = el("input", "rename");
    label.value = m.name;
    label.maxLength = SAYIT_MAX_NAME;
    label.spellcheck = false;
    label.dataset.id = m.id;
    label.setAttribute("aria-label", "New name");
    label.addEventListener("click", (e) => e.stopPropagation());
    label.addEventListener("keydown", (e) => {
      if (e.key === "Enter") commitRename(m.id, label.value);
      else if (e.key === "Escape") cancelRename();
    });
    label.addEventListener("blur", () => {
      if (!rebuilding) commitRename(m.id, label.value);
    });
  } else {
    label = el("span", "name", m.name);
  }

  const playing = listening === m.id;
  const listenBtn = iconButton(playing ? "■" : "▶", playing ? "Stop" : "Listen", () => toggleListen(m.id));
  const renameBtn = iconButton("✎", "Rename", () => startRename(m.id));
  const armed = armedDelete === m.id;
  const deleteBtn = iconButton(armed ? "Delete?" : "✕", armed ? "Click again to delete" : "Delete", () =>
    deleteMessage(m.id),
  );
  if (armed) deleteBtn.classList.add("armed");

  li.append(pick, label, el("span", "dur", clock(m.secs || 0)), listenBtn, renameBtn, deleteBtn);
  return li;
}

function renderList() {
  // Keep an open rename box (and what's typed in it) across refreshes.
  const open = els.lib.querySelector("input.rename");
  const typed =
    open && open.dataset.id === renaming
      ? { value: open.value, start: open.selectionStart, end: open.selectionEnd }
      : null;
  if (renaming && !lib.messages.some((m) => m.id === renaming)) renaming = null;
  rebuilding = true;
  try {
    els.lib.replaceChildren(...lib.messages.map((m) => messageRow(m, m.id === lib.activeId)));
  } finally {
    rebuilding = false;
  }
  const input = els.lib.querySelector("input.rename");
  if (input) {
    if (typed) {
      input.value = typed.value;
      input.setSelectionRange(typed.start, typed.end);
    } else {
      input.select();
    }
    input.focus();
  }
}

function renderClock() {
  els.state.textContent = `● Recording ${clock((Date.now() - startedAt) / 1000)} — say your message`;
}

function render() {
  renderList();
  const recording = !!recorder;
  const active = activeEntry();
  els.empty.hidden = lib.messages.length > 0 || recording || !!take;

  // The new recording: recording now, or waiting for a name.
  els.takeBox.hidden = !recording && !take;
  els.takeRow.hidden = recording;
  if (recording) renderClock();
  else if (take) els.state.textContent = `New recording · ${clock(take.secs)} · name it and save`;
  els.state.classList.toggle("live", recording);
  els.takeListen.textContent = listening === "take" ? "■" : "▶";

  els.record.textContent = recording ? "■ Stop" : take ? "● Re-record" : "● Record";
  els.record.disabled = uploading;
  els.upload.disabled = uploading || recording;
  els.upload.textContent = uploading ? "Adding…" : "Upload audio…";

  els.playCall.hidden = recording || (!active && !callPlaying);
  els.playCall.textContent = callPlaying ? "■ Stop playing" : `▶ Play “${active ? active.name : ""}” into the call now`;

  const key = sayitHotkeyLabel(settings.hotkey);
  if (!lib.messages.length) els.hint.textContent = "";
  else if (!active) els.hint.textContent = "Click the message calls should get.";
  else els.hint.textContent = `During a call press ${key} to play it — again to stop. Click another message to switch.`;
}

// ---- Picking, renaming, deleting ----
async function select(id) {
  if (id === lib.activeId) return;
  showError("");
  try {
    await sayitSelect(id);
    await reloadLibrary();
  } catch (e) {
    showError(e);
  }
}

function startRename(id) {
  renaming = id;
  armedDelete = null;
  render();
}

function cancelRename() {
  renaming = null;
  render();
}

async function commitRename(id, value) {
  if (renaming !== id) return; // Enter, then the blur that follows
  renaming = null;
  const current = lib.messages.find((m) => m.id === id);
  if (!current || !value.trim() || value.trim() === current.name) return render();
  try {
    await sayitRename(id, value);
  } catch (e) {
    showError(e);
  }
  await reloadLibrary();
}

async function deleteMessage(id) {
  if (armedDelete !== id) {
    armedDelete = id;
    clearTimeout(armTimer);
    armTimer = setTimeout(() => {
      armedDelete = null;
      render();
    }, 3000);
    return render();
  }
  armedDelete = null;
  clearTimeout(armTimer);
  if (listening === id) stopListening();
  try {
    await sayitDelete(id);
  } catch (e) {
    showError(e);
  }
  await reloadLibrary();
}

// ---- Listening (here, not on the call) ----
function stopListening() {
  els.player.pause();
  const was = listening;
  listening = null;
  if (was) render();
}

async function toggleListen(id) {
  const key = id || "take";
  if (listening === key) return stopListening();
  stopListening();
  try {
    if (id) {
      const audio = await sayitAudioOf(id);
      if (!audio) throw new Error("that message's audio is missing — record or upload it again");
      els.player.src = `data:${audio.type || "audio/wav"};base64,${audio.data}`;
    } else {
      if (blobUrl) URL.revokeObjectURL(blobUrl);
      blobUrl = URL.createObjectURL(new Blob([take.wav], { type: "audio/wav" }));
      els.player.src = blobUrl;
    }
    await els.player.play();
    listening = key;
    render();
  } catch (e) {
    showError(e);
  }
}
els.player.addEventListener("ended", stopListening);
els.takeListen.addEventListener("click", () => toggleListen(null));

// ---- Recording ----
els.record.addEventListener("click", async () => {
  showError("");
  showNote("");
  stopListening();
  if (recorder) {
    recorder.stop();
    return;
  }
  let stream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  } catch (e) {
    els.allowMic.hidden = false;
    showError("Chrome hasn't let SayIt use the microphone yet.");
    return;
  }
  els.allowMic.hidden = true;
  const chunks = [];
  recorder = new MediaRecorder(stream);
  recorder.ondataavailable = (e) => e.data.size && chunks.push(e.data);
  recorder.onstop = async () => {
    stream.getTracks().forEach((t) => t.stop());
    const type = recorder.mimeType;
    recorder = null;
    clearInterval(tick);
    try {
      take = await sayitFromRecording(new Blob(chunks, { type }));
    } catch (e) {
      take = null;
      showError(`Couldn't use that recording: ${errText(e)}`);
    }
    render();
    if (take) {
      els.takeName.value = sayitDefaultName(lib.messages);
      els.takeName.focus();
      els.takeName.select();
    }
  };
  take = null;
  startedAt = Date.now();
  recorder.start();
  tick = setInterval(renderClock, 500);
  render();
});

async function saveTake() {
  if (!take) return;
  stopListening();
  try {
    const entry = await sayitAdd(els.takeName.value, take);
    take = null;
    showNote(`Saved “${entry.name}”.`);
  } catch (e) {
    showError(e);
  }
  await reloadLibrary();
}
els.save.addEventListener("click", saveTake);
els.takeName.addEventListener("keydown", (e) => {
  if (e.key === "Enter") saveTake();
});

els.discard.addEventListener("click", () => {
  stopListening();
  take = null;
  render();
});

// ---- Uploading voicemail files ----
els.upload.addEventListener("click", () => {
  showError("");
  showNote("");
  // Windows opens the file picker over the popup. Elsewhere Chrome closes the
  // popup when a picker opens, so SayIt's own page takes the files instead.
  if (os === "win") els.file.click();
  else chrome.tabs.create({ url: "options.html#upload" });
});

els.file.addEventListener("change", async () => {
  const files = Array.from(els.file.files || []);
  els.file.value = ""; // so picking the same file again still counts
  if (!files.length) return;
  stopListening();
  uploading = true;
  render();
  const report = sayitAddFilesReport(await sayitAddFiles(files, showNote));
  uploading = false;
  showNote(report.note);
  showError(report.error);
  await reloadLibrary();
});

// Uploads from SayIt's page, and changes made in another window.
chrome.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && (changes.messages || changes.activeId || changes.message)) reloadLibrary();
});

els.playCall.addEventListener("click", () => chrome.runtime.sendMessage({ sayit: "toggle-calls" }));
els.allowMic.addEventListener("click", () => chrome.tabs.create({ url: "options.html#mic" }));

// ---- Settings view ----
const hotkeyLabel = $("hotkeyLabel");
const changeHotkey = $("changeHotkey");
let capturing = false;

function renderSettings() {
  hotkeyLabel.textContent = sayitHotkeyLabel(settings.hotkey);
  $("muteMic").checked = settings.muteMic;
  $("hearIt").checked = settings.hearIt;
  $("everywhere").checked = settings.everywhere;
}

async function saveSettings(patch) {
  settings = { ...settings, ...patch };
  await chrome.storage.local.set({ settings });
  renderSettings();
  render();
}

changeHotkey.addEventListener("click", () => {
  capturing = true;
  hotkeyLabel.textContent = "Press keys… (Esc cancels)";
  changeHotkey.disabled = true;
});

// No Windows "ding" for Alt+<letter> here either: claim it as an access key
// (Chrome otherwise passes it on to Windows; see inject.js).
const accessTarget = document.createElement("span");
accessTarget.hidden = true;
document.body.appendChild(accessTarget);
function claimAltKey(e) {
  if (!e.altKey || e.ctrlKey || e.metaKey || e.key.length !== 1) return;
  accessTarget.removeAttribute("accesskey");
  accessTarget.setAttribute("accesskey", e.key.toLowerCase());
}

document.addEventListener(
  "keydown",
  (e) => {
    if (!capturing) return;
    e.preventDefault();
    e.stopPropagation();
    claimAltKey(e);
    if (["Control", "Alt", "Shift", "Meta"].includes(e.key)) return;
    capturing = false;
    changeHotkey.disabled = false;
    if (e.key === "Escape") return renderSettings();
    const isFKey = /^F\d{1,2}$/.test(e.code);
    if (!e.ctrlKey && !e.altKey && !e.metaKey && !isFKey) {
      renderSettings();
      $("hotkeyHint").textContent = "Use Ctrl, Alt or Win with a key (or an F-key), so normal typing never triggers it.";
      return;
    }
    saveSettings({
      hotkey: {
        code: e.code,
        key: e.key.length === 1 ? e.key.toUpperCase() : e.key,
        alt: e.altKey,
        ctrl: e.ctrlKey,
        shift: e.shiftKey,
        meta: e.metaKey,
      },
    });
  },
  true,
);

// The hotkey works with this popup open too.
document.addEventListener("keydown", (e) => {
  if (capturing || e.defaultPrevented || !sayitHotkeyMatches(e, settings.hotkey)) return;
  e.preventDefault();
  claimAltKey(e);
  if (!e.repeat) chrome.runtime.sendMessage({ sayit: "toggle-calls" }).catch(() => {});
});

$("resetHotkey").addEventListener("click", () => saveSettings({ hotkey: SAYIT_DEFAULTS.hotkey }));
$("muteMic").addEventListener("change", (e) => saveSettings({ muteMic: e.target.checked }));
$("hearIt").addEventListener("change", (e) => saveSettings({ hearIt: e.target.checked }));
$("shortcuts").addEventListener("click", (e) => {
  e.preventDefault();
  chrome.tabs.create({ url: "chrome://extensions/shortcuts" });
});
$("openTest").addEventListener("click", () => chrome.tabs.create({ url: "options.html" }));

// ---- Where the call is, and the sites SayIt's microphone runs on ----
let tabOrigin = null; // the active tab's web origin, if any

async function checkTab() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  try {
    const u = new URL((tab && tab.url) || "");
    if (u.protocol === "https:" || u.protocol === "http:") tabOrigin = u.origin;
  } catch {}
  renderSites();
  // Tabs opened before SayIt was installed or updated don't have it yet.
  if (tabOrigin) {
    const has = await chrome.tabs.sendMessage(tab.id, { sayit: "ping" }, { frameId: 0 }).then(
      () => true,
      () => false,
    );
    $("reloadState").hidden = has;
  }
}

// "calls.aloware.com inside app-na2.hubspot.com"
const placeOf = (where) =>
  where && where.length > 1 ? `${where[0]} inside ${where[where.length - 1]}` : (where && where[0]) || "";

async function renderCall() {
  let list = [];
  try {
    list = (await chrome.runtime.sendMessage({ sayit: "calls" })) || [];
  } catch {}
  const playing = list.find((f) => f.playing);
  const live = list.find((f) => f.live > 0);
  const active = activeEntry();
  const what = active ? `“${active.name}”` : "your message";
  let text;
  let tone = "";
  if (playing) [text, tone] = [`Playing ${what} · ${placeOf(playing.where)}`, "ok"];
  else if (live) [text, tone] = [`Live call found — ${sayitHotkeyLabel(settings.hotkey)} plays ${what} · ${placeOf(live.where)}`, "ok"];
  else if (list.length) text = `Microphone found, no connected call yet · ${placeOf(list[0].where)}`;
  else text = "No call found yet — start a call, then look here.";
  const stateLine = $("tabState");
  stateLine.textContent = text;
  stateLine.className = `tabstate ${tone}`;
  stateLine.hidden = false;
  if (!!playing !== callPlaying) {
    callPlaying = !!playing;
    render();
  }
}

function renderSites() {
  $("siteList").replaceChildren(
    ...settings.sites.map((origin) => {
      const li = document.createElement("li");
      const name = document.createElement("span");
      name.textContent = sayitHost(origin);
      const remove = document.createElement("button");
      remove.className = "small";
      remove.textContent = "Remove";
      remove.addEventListener("click", () =>
        saveSettings({ sites: settings.sites.filter((o) => o !== origin) }).then(renderSites),
      );
      li.append(name, remove);
      return li;
    }),
  );
  const add = $("addSite");
  add.hidden = !tabOrigin || settings.everywhere || sayitBuiltIn(tabOrigin) || settings.sites.includes(tabOrigin);
  if (tabOrigin) add.textContent = `+ Add ${sayitHost(tabOrigin)}`;
}

$("addSite").addEventListener("click", async () => {
  await saveSettings({ sites: [...settings.sites, tabOrigin] });
  renderSites();
  $("siteHint").textContent = "Added — SayIt takes the microphone there from the next call on.";
});
$("everywhere").addEventListener("change", (e) => saveSettings({ everywhere: e.target.checked }).then(renderSites));

// ---- Tabs ----
function showView(name) {
  $("viewMessage").hidden = name !== "message";
  $("viewSettings").hidden = name !== "settings";
  $("tabMessage").classList.toggle("on", name === "message");
  $("tabSettings").classList.toggle("on", name === "settings");
}
$("tabMessage").addEventListener("click", () => showView("message"));
$("tabSettings").addEventListener("click", () => showView("settings"));

(async () => {
  try {
    os = (await chrome.runtime.getPlatformInfo()).os;
  } catch {}
  await sayitMigrate().catch(() => {});
  settings = await sayitSettings();
  lib = await sayitLibrary();
  renderSettings();
  render();
  checkTab();
  renderCall();
  setInterval(renderCall, 1000);
})();
