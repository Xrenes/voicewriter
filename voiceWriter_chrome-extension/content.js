// VoiceWriter content script: runs on every page (and on the welcome page's
// practice box). Default shortcuts, all editable in the popup:
//   Alt+C (hold)   dictate into the focused field
//   Alt+A          the board: a see-through box that collects what you dictate
//   Control+Alt+C  Read aloud: read the selected text aloud (again to stop)
//   Alt+R / P / H  Refine / Professional / Humanize the selected text
// (Alt+L, SayIt's voicemail, is handled by sayit-inject.js.)
(() => {
  const VW = globalThis.VoiceWriterShared;

  // After an extension update, the copy injected into this tab takes over
  // from the old one, which can no longer reach the extension.
  const TAKEOVER = `voicewriter-content-${chrome.runtime.id}`;
  document.dispatchEvent(new Event(TAKEOVER));
  const lifetime = new AbortController();
  const on = (target, type, fn, opts = {}) => target.addEventListener(type, fn, { ...opts, signal: lifetime.signal });
  const isTop = window.top === window;

  let settings = { ...VW.DEFAULTS };
  const refreshSettings = () =>
    VW.loadSettings()
      .then((s) => {
        settings = s;
        if (livePanel) applyLiveLook();
      })
      .catch(() => {});
  refreshSettings();
  on(window, "focus", refreshSettings);

  const alive = () => {
    try {
      return !lifetime.signal.aborted && !!chrome.runtime?.id;
    } catch {
      return false;
    }
  };

  function send(msg) {
    return new Promise((resolve) => {
      if (!alive()) {
        resolve({ ok: false, error: "VoiceWriter was updated. Reload this page to keep using it." });
        return;
      }
      try {
        chrome.runtime.sendMessage(msg, (r) => {
          if (chrome.runtime.lastError) resolve({ ok: false, error: chrome.runtime.lastError.message });
          else resolve(r || { ok: false, error: "No response from VoiceWriter." });
        });
      } catch (e) {
        resolve({ ok: false, error: e.message });
      }
    });
  }

  // Clipboard: the extension's helper page first, then the (focused) page.
  async function copyText(text) {
    if ((await send({ type: "copy", text })).ok) return true;
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      return false;
    }
  }

  // ------------------------------------------------------------ on-page UI
  // Closed shadow roots keep the page's CSS out and ours in. Styles go
  // through constructed stylesheets, so strict page CSPs don't block them.

  const MARK_PATHS = [
    ["rect", { x: 22, y: 100, width: 28, height: 66, rx: 14, fill: "currentColor" }],
    ["rect", { x: 58, y: 72, width: 28, height: 94, rx: 14, fill: "currentColor" }],
    ["path", { d: "M108 38V139", fill: "none", stroke: "currentColor", "stroke-width": 28, "stroke-linecap": "round" }],
    ["path", { d: "M108 132V139Q108 166 137 166H172", fill: "none", stroke: "currentColor", "stroke-width": 28, "stroke-linecap": "round" }],
  ];

  function svg(tag, attrs) {
    const el = document.createElementNS("http://www.w3.org/2000/svg", tag);
    for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
    return el;
  }

  function buildMark(cls) {
    const mark = svg("svg", { viewBox: "0 0 200 200", class: cls, "aria-hidden": "true" });
    for (const [tag, attrs] of MARK_PATHS) mark.append(svg(tag, attrs));
    return mark;
  }

  function el(tag, cls, text) {
    const node = document.createElement(tag);
    if (cls) node.className = cls;
    if (text != null) node.textContent = text;
    return node;
  }

  function shadowHost(tagName, css) {
    const host = document.createElement(tagName);
    host.style.cssText = "all: initial; position: fixed; top: 0; left: 0; width: 0; height: 0; z-index: 2147483647;";
    const root = host.attachShadow({ mode: "closed" });
    try {
      const sheet = new CSSStyleSheet();
      sheet.replaceSync(css);
      root.adoptedStyleSheets = [sheet];
    } catch {
      root.append(el("style", null, css));
    }
    return { host, root };
  }

  // ---- toast (same look as the desktop app's) ----
  const TOAST_CSS = `
    .toast {
      position: fixed; z-index: 2147483647; display: inline-flex; align-items: center; gap: 8px;
      max-width: min(420px, calc(100vw - 24px)); padding: 7px 12px; border-radius: 12px;
      background: rgba(0, 0, 0, 0.86); color: #fff; border: 1px solid rgba(255, 255, 255, 0.22);
      box-shadow: 0 4px 14px rgba(0, 0, 0, 0.35); box-sizing: border-box; pointer-events: none;
      font: 12.5px/1.35 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; letter-spacing: normal;
      opacity: 0; transform: translateY(4px); transition: opacity .12s ease, transform .12s ease;
    }
    .toast.show { opacity: 1; transform: none; }
    .text { overflow: hidden; display: -webkit-box; -webkit-line-clamp: 3; -webkit-box-orient: vertical; word-break: break-word; }
    .icon { flex: none; width: 12px; height: 12px; border-radius: 50%; box-sizing: border-box; }
    .toast[data-kind="busy"] .icon { border: 2px solid rgba(255,255,255,.3); border-top-color: #fff; animation: spin .8s linear infinite; }
    .toast[data-kind="done"] .icon { background: #3ecf8e; }
    .toast[data-kind="error"] { border-color: rgba(255, 154, 154, 0.7); }
    .toast[data-kind="error"] .icon { background: #ff6b6b; }
    .toast[data-kind="rec"] .icon, .toast[data-kind="speak"] .icon { display: none; }
    .mark { display: none; flex: none; width: 15px; height: 15px; color: #fff; }
    .toast[data-kind="rec"] .mark { display: block; animation: blink-red 1.1s ease-in-out infinite; }
    .toast[data-kind="speak"] .mark { display: block; color: #d6486c; }
    @keyframes spin { to { transform: rotate(360deg); } }
    @keyframes blink-red { 0%, 100% { color: #ffffff; } 50% { color: #f87171; } }
  `;

  let toastHost = null;
  let toastEl = null;
  let toastText = null;
  let toastTimer = null;
  let mouse = null;

  on(
    window,
    "mousemove",
    (e) => {
      mouse = { x: e.clientX, y: e.clientY, at: Date.now() };
    },
    { passive: true, capture: true },
  );

  function ensureToast() {
    if (toastHost?.isConnected) return;
    const { host, root } = shadowHost("voicewriter-ui", TOAST_CSS);
    toastHost = host;
    toastEl = el("div", "toast");
    toastEl.setAttribute("role", "status");
    toastText = el("span", "text");
    toastEl.append(el("span", "icon"), buildMark("mark"), toastText);
    root.append(toastEl);
    (document.body || document.documentElement).append(host);
  }

  // kind: busy | done | error | rec | speak. ms = 0 keeps it up.
  function toast(kind, text, ms = 2500) {
    if (!isTop && !document.hasFocus()) return;
    ensureToast();
    toastEl.dataset.kind = kind;
    toastText.textContent = text;
    // Near the mouse like the desktop app; top centre when the mouse is unknown.
    const recent = mouse && Date.now() - mouse.at < 15000;
    let left = recent ? mouse.x + 14 : (window.innerWidth - 260) / 2;
    let top = recent ? mouse.y + 18 : 16;
    left = Math.max(12, Math.min(left, window.innerWidth - 272));
    top = Math.max(12, Math.min(top, window.innerHeight - 60));
    toastEl.style.left = `${left}px`;
    toastEl.style.top = `${top}px`;
    toastEl.classList.add("show");
    clearTimeout(toastTimer);
    if (ms) toastTimer = setTimeout(hideToast, ms);
  }

  function hideToast() {
    clearTimeout(toastTimer);
    toastEl?.classList.remove("show");
  }

  // ------------------------------------------------------------ the board
  // A see-through black box (60% transparent by default, adjustable in
  // Settings) that fills with your words as you speak. Drag it by its top
  // bar, resize it from the corner; it remembers where you left it.

  const LIVE_CSS = `
    .live {
      position: fixed; z-index: 2147483646; display: flex; flex-direction: column;
      min-width: 260px; min-height: 110px; max-width: calc(100vw - 8px); max-height: calc(100vh - 8px);
      background: rgba(0, 0, 0, var(--alpha, 0.4)); color: #fff;
      border: 1px solid rgba(255, 255, 255, 0.16); border-radius: 10px; overflow: hidden; resize: both;
      box-shadow: 0 10px 30px rgba(0, 0, 0, 0.25); box-sizing: border-box;
      font: 13px/1.4 "Segoe UI", system-ui, -apple-system, Roboto, sans-serif; letter-spacing: normal;
      -webkit-backdrop-filter: blur(var(--blur, 0px)); backdrop-filter: blur(var(--blur, 0px));
    }
    .bar { display: flex; align-items: center; gap: 10px; padding: 6px 6px 6px 12px; cursor: move; user-select: none; flex: none; }
    .state { display: inline-flex; align-items: center; gap: 6px; font-size: 11px; font-weight: 600; letter-spacing: .06em; text-transform: uppercase; color: rgba(255,255,255,.85); text-shadow: 0 1px 2px rgba(0,0,0,.8); flex: 1; }
    .state .mark { width: 13px; height: 13px; color: #d6486c; }
    .dot { width: 8px; height: 8px; border-radius: 50%; background: #ff4d4d; animation: pulse 1.4s ease-in-out infinite; }
    .state.ended .dot { background: #8a8a8a; animation: none; }
    .state.static .dot { background: #d6486c; animation: none; }
    @keyframes pulse { 50% { opacity: .3; } }
    .tools { display: inline-flex; gap: 2px; }
    .tools button {
      background: rgba(0,0,0,.25); border: 0; color: rgba(255,255,255,.85); font: 12px/1 "Segoe UI", system-ui, sans-serif;
      padding: 5px 8px; border-radius: 6px; cursor: pointer;
    }
    .tools button:hover { background: rgba(255,255,255,.18); color: #fff; }
    .notice { margin: 0 10px 4px; padding: 4px 8px; font-size: 12px; border-radius: 6px; background: rgba(255,120,80,.28); color: #ffd9cc; flex: none; }
    .notice[hidden] { display: none; }
    .feed {
      flex: 1; overflow-y: auto; padding: 2px 14px 12px; font-size: calc(18px * var(--scale, 1)); line-height: 1.4;
      user-select: text; cursor: text; scrollbar-width: thin; scrollbar-color: rgba(255,255,255,.3) transparent;
    }
    .feed p { margin: 0 0 .45em; overflow-wrap: anywhere; text-shadow: 0 1px 2px rgba(0,0,0,.95), 0 0 6px rgba(0,0,0,.6); }
    .feed span.partial { opacity: .65; }
    .feed .empty { color: rgba(255,255,255,.75); font-size: .8em; font-style: italic; }
    .feed .empty[hidden] { display: none; }
  `;

  let liveHost = null;
  let livePanel = null;
  let liveParts = null;
  let liveSnap = null;
  let liveCache = {};
  let liveBox = null; // { left, top, width, height, scale }

  function applyLiveLook() {
    if (!livePanel) return;
    const transparency = Math.min(95, Math.max(0, Number(settings.liveTransparency ?? 60)));
    livePanel.style.setProperty("--alpha", String(1 - transparency / 100));
    livePanel.style.setProperty("--blur", transparency >= 50 ? "2px" : "0px");
    livePanel.style.setProperty("--scale", String(liveBox?.scale || 1));
  }

  function placeLive() {
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const width = Math.min(liveBox?.width || 560, vw - 16);
    const height = Math.min(liveBox?.height || 170, vh - 16);
    let left = liveBox?.left ?? (vw - width) / 2;
    let top = liveBox?.top ?? vh - height - 24;
    left = Math.max(8, Math.min(left, vw - width - 8));
    top = Math.max(8, Math.min(top, vh - height - 8));
    Object.assign(livePanel.style, { left: `${left}px`, top: `${top}px`, width: `${width}px`, height: `${height}px` });
  }

  let saveBoxTimer = 0;
  function saveLiveBox() {
    if (!livePanel) return;
    clearTimeout(saveBoxTimer);
    saveBoxTimer = setTimeout(() => {
      if (!livePanel || !alive()) return;
      const r = livePanel.getBoundingClientRect();
      liveBox = { left: Math.round(r.left), top: Math.round(r.top), width: Math.round(r.width), height: Math.round(r.height), scale: liveBox?.scale || 1 };
      chrome.storage.local.set({ liveBox }).catch(() => {});
    }, 300);
  }

  function liveButton(label, title, fn) {
    const b = el("button", null, label);
    b.type = "button";
    b.title = title;
    b.addEventListener("click", (e) => {
      e.stopPropagation();
      fn(b);
    });
    return b;
  }

  async function showLive(snapshot) {
    if (!isTop) return false;
    if (!livePanel || !liveHost?.isConnected) {
      try {
        liveBox = (await chrome.storage.local.get("liveBox")).liveBox || null;
      } catch {
        liveBox = null;
      }
      const { host, root } = shadowHost("voicewriter-live", LIVE_CSS);
      liveHost = host;
      livePanel = el("div", "live");
      livePanel.setAttribute("role", "log");
      livePanel.setAttribute("aria-label", "VoiceWriter board");

      const bar = el("div", "bar");
      const state = el("span", "state");
      const stateText = el("span", null, "Live");
      state.append(el("i", "dot"), stateText);
      const tools = el("span", "tools");
      const scaleBy = (d) => {
        const scale = Math.min(2.4, Math.max(0.6, +((liveBox?.scale || 1) + d).toFixed(2)));
        liveBox = { ...(liveBox || {}), scale };
        applyLiveLook();
        saveLiveBox();
      };
      tools.append(
        liveButton("A−", "Smaller text", () => scaleBy(-0.1)),
        liveButton("A+", "Bigger text", () => scaleBy(0.1)),
        liveButton("Clear", "Clear the text", () => send({ type: "liveClear" })),
        liveButton("Copy", "Copy the text", async (b) => {
          const ok = await copyText(VW.liveText(liveSnap));
          b.textContent = ok ? "Copied" : "Failed";
          setTimeout(() => (b.textContent = "Copy"), 1200);
        }),
        liveButton("✕", `Close (${settings.hotkeys.live || "Alt+A"})`, () => send({ type: "liveStop" })),
      );
      bar.append(state, tools);

      const notice = el("div", "notice");
      notice.hidden = true;
      const feed = el("div", "feed");
      const empty = el("p", "empty", "Listening… start talking.");
      feed.append(empty);
      livePanel.append(bar, notice, feed);
      root.append(livePanel);
      liveParts = { state, stateText, notice, feed, empty };
      liveCache = {};

      // Drag by the top bar.
      let drag = null;
      bar.addEventListener("pointerdown", (e) => {
        if (e.button !== 0 || e.target.closest("button")) return;
        const r = livePanel.getBoundingClientRect();
        drag = { dx: e.clientX - r.left, dy: e.clientY - r.top };
        bar.setPointerCapture(e.pointerId);
        e.preventDefault();
      });
      bar.addEventListener("pointermove", (e) => {
        if (!drag) return;
        const r = livePanel.getBoundingClientRect();
        const left = Math.max(0, Math.min(e.clientX - drag.dx, window.innerWidth - r.width));
        const top = Math.max(0, Math.min(e.clientY - drag.dy, window.innerHeight - r.height));
        livePanel.style.left = `${left}px`;
        livePanel.style.top = `${top}px`;
      });
      const endDrag = () => {
        if (!drag) return;
        drag = null;
        saveLiveBox();
      };
      bar.addEventListener("pointerup", endDrag);
      bar.addEventListener("pointercancel", endDrag);
      // Resized from the corner.
      new ResizeObserver(() => saveLiveBox()).observe(livePanel);

      placeLive();
      applyLiveLook();
      document.documentElement.append(host);
    }
    renderLive(snapshot);
    return true;
  }

  function renderLive(snapshot) {
    if (!livePanel || !snapshot) return;
    liveSnap = snapshot;
    const { state, stateText, notice, feed, empty } = liveParts;
    VW.renderLive(feed, empty, snapshot, liveCache);
    state.classList.toggle("ended", !snapshot.active);
    state.classList.toggle("static", snapshot.mode === "board");
    stateText.textContent = snapshot.label || (snapshot.active ? "Live" : "Stopped");
    notice.hidden = !snapshot.notice;
    notice.textContent = snapshot.notice || "";
  }

  function hideLive() {
    liveHost?.remove();
    liveHost = null;
    livePanel = null;
    liveParts = null;
    liveSnap = null;
  }

  on(window, "resize", () => livePanel && placeLiveWithinView());
  function placeLiveWithinView() {
    const r = livePanel.getBoundingClientRect();
    const left = Math.max(8, Math.min(r.left, window.innerWidth - r.width - 8));
    const top = Math.max(8, Math.min(r.top, window.innerHeight - r.height - 8));
    livePanel.style.left = `${left}px`;
    livePanel.style.top = `${top}px`;
  }

  async function toggleLive() {
    const r = await send({ type: "liveToggle" });
    if (!r.ok) toast("error", r.error, 5000);
  }

  // ------------------------------------------------------------ text helpers

  function deepActive() {
    let node = document.activeElement;
    while (node?.shadowRoot?.activeElement) node = node.shadowRoot.activeElement;
    return node;
  }

  // Password fields are deliberately left out: nothing typed there is sent anywhere.
  const TEXT_INPUTS = /^(text|search|email|url|tel|)$/i;
  const isField = (node) => node && (node.tagName === "TEXTAREA" || (node.tagName === "INPUT" && TEXT_INPUTS.test(node.type || "")));
  const isEditable = (node) => !!node && !node.disabled && !node.readOnly && (isField(node) || node.isContentEditable);

  // Remembers what is selected (and where) so the result can go back in the
  // same place after the request finishes.
  function captureSelection() {
    const node = deepActive();
    if (isField(node)) {
      const start = node.selectionStart ?? node.value.length;
      const end = node.selectionEnd ?? start;
      // A caret in a text box while other page text is selected: use that text.
      const pageText = start === end ? window.getSelection()?.toString() : "";
      if (!pageText) return { el: isEditable(node) ? node : null, text: node.value.slice(start, end), start, end };
      const range = window.getSelection().getRangeAt(0).cloneRange();
      return { el: null, text: pageText, range };
    }
    const root = node?.getRootNode?.();
    const sel = (root instanceof ShadowRoot && root.getSelection?.()) || window.getSelection();
    const range = sel && sel.rangeCount ? sel.getRangeAt(0).cloneRange() : null;
    // Only write back when the selection is inside the focused editor.
    const inside = !range || node?.contains(range.commonAncestorContainer);
    return { el: isEditable(node) && inside ? node : null, text: sel ? sel.toString() : "", range };
  }

  // ---- Flexible "what's selected" for Read aloud and the rewrites ----

  // This frame holds the keyboard focus (not just a frame around it).
  function ownsFocus() {
    const node = deepActive();
    return !!node && node !== document.body && node !== document.documentElement && node.tagName !== "IFRAME";
  }

  const showsText = (node) => {
    const r = node.getBoundingClientRect();
    return r.width > 2 && r.height > 2 && window.innerWidth > 2 && window.innerHeight > 2;
  };

  function textFromHtml(html) {
    try {
      const doc = new DOMParser().parseFromString(html, "text/html");
      const rows = [...doc.querySelectorAll("tr")];
      if (rows.length) return rows.map((r) => [...r.querySelectorAll("td,th")].map((c) => c.textContent.trim()).join("\t")).join("\n");
      return doc.body ? doc.body.textContent : "";
    } catch {
      return html.replace(/<[^>]*>/g, " ").replace(/&nbsp;/g, " ").replace(/&amp;/g, "&").replace(/&lt;/g, "<").replace(/&gt;/g, ">");
    }
  }

  // Apps that draw their own selection (Google Sheets and Docs, Excel for
  // the web, Airtable…) have no text selection on the page, but they hand
  // over the selected cells or words when asked to copy. VoiceWriter asks
  // with a pretend copy event, so the clipboard is never touched.
  function textFromApp(node) {
    const target = node && node !== document.documentElement ? node : document.body;
    if (!target) return "";
    let data;
    try {
      data = new DataTransfer();
      target.dispatchEvent(new ClipboardEvent("copy", { clipboardData: data, bubbles: true, cancelable: true, composed: true }));
    } catch {
      return "";
    }
    let text = data.getData("text/plain");
    if (!text.trim()) {
      const html = data.getData("text/html");
      if (html) text = textFromHtml(html);
    }
    return text.replace(/\r\n?/g, "\n").replace(/\n+$/, "");
  }

  // What to read or rewrite, as flexibly as the page allows:
  //  1. the selected text (or the selection inside a text field);
  //  2. nothing selected in a plain text field: the whole field;
  //  3. a cell or range in a spreadsheet, or words in an app that draws its
  //     own text (Google Sheets and Docs, Excel for the web…);
  //  4. Read aloud only: nothing selected in a rich text box: the whole box.
  // `appCopy: false` skips step 3 (used when nothing was asked for yet).
  function captureTarget(purpose, { appCopy = true } = {}) {
    const info = captureSelection();
    if (info.text.trim()) return { ...info, source: "selection" };
    const node = deepActive();
    if (isField(node) && node.value.trim()) {
      return { el: isEditable(node) ? node : null, text: node.value, start: 0, end: node.value.length, source: "box" };
    }
    if (appCopy) {
      const copied = textFromApp(node);
      if (copied.trim()) return { el: null, text: copied, source: "app" };
    }
    if (purpose === "read" && node?.isContentEditable && showsText(node)) {
      const text = (node.innerText || "").trim();
      if (text) return { el: null, text, source: "box" };
    }
    return { ...info, text: "", source: "none" };
  }

  // Spreadsheet ranges come as tab- and line-separated cells: read them as
  // "a, b, c. d, e, f." so each cell and row gets a natural pause.
  const APP_READ_LIMIT = 1500;
  function speakable(target) {
    let text = target.text;
    if (target.source === "app" && /[\t\n]/.test(text)) {
      text = text
        .split("\n")
        .map((row) =>
          row
            .split("\t")
            .map((c) => c.trim())
            .filter(Boolean)
            .join(", "),
        )
        .filter(Boolean)
        .map((row) => (/[.!?]$/.test(row) ? row : `${row}.`))
        .join(" ");
    }
    if (target.source === "app" && text.length > APP_READ_LIMIT) text = `${text.slice(0, APP_READ_LIMIT).replace(/\s+\S*$/, "")}…`;
    return text;
  }

  function restoreSelection(info) {
    const target = info.el;
    target.focus({ preventScroll: true });
    if (isField(target)) {
      if (info.start != null) target.setSelectionRange(info.start, info.end);
    } else if (info.range) {
      const sel = window.getSelection();
      sel.removeAllRanges();
      sel.addRange(info.range);
    }
  }

  function charBeforeCaret(info) {
    const target = info.el;
    if (isField(target)) return target.value.slice(0, target.selectionStart ?? 0).slice(-1);
    const r = info.range;
    if (r && r.startContainer.nodeType === Node.TEXT_NODE) return r.startContainer.textContent.slice(0, r.startOffset).slice(-1);
    return "";
  }

  // Inserts like real typing so editors (Gmail, WhatsApp Web, LinkedIn,
  // React apps) see the change and Ctrl+Z can undo it.
  function insertText(info, text) {
    const target = info.el;
    if (!target?.isConnected) return false;
    restoreSelection(info);
    if (document.execCommand("insertText", false, text)) return true;
    if (isField(target)) {
      const start = target.selectionStart ?? target.value.length;
      target.setRangeText(text, start, target.selectionEnd ?? start, "end");
      target.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: text }));
      return true;
    }
    // Some rich editors ignore execCommand but accept a synthetic paste.
    const data = new DataTransfer();
    data.setData("text/plain", text);
    return !target.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
  }

  // ------------------------------------------------------------ dictation

  let dict = null;

  function startDictation(e, combo) {
    if (dict) return;
    const info = captureSelection();
    dict = {
      info,
      keyCode: e.code,
      mods: { Control: e.ctrlKey, Alt: e.altKey, Shift: e.shiftKey, Meta: e.metaKey },
      stopping: false,
    };
    toast("rec", `Listening… release ${settings.hotkeys.dictate || combo} to insert`, 0);
    dict.started = send({ type: "dictStart" }).then((r) => {
      if (!r.ok) toast("error", r.error, 6000);
      return r;
    });
  }

  async function stopDictation() {
    const d = dict;
    if (!d || d.stopping) return;
    d.stopping = true;
    const started = await d.started;
    dict = null;
    if (!started.ok) return;
    toast("busy", "Transcribing…", 0);
    const r = await send({ type: "dictStop" });
    if (!r.ok) {
      toast("error", r.error, 6000);
      return;
    }
    const { text, short, mode } = r.result;
    if (short) {
      toast("error", "Too short. Hold the shortcut while you speak.", 2000);
      return;
    }
    if (!text) {
      toast("error", "Didn't catch anything. Try again a little closer to the mic.", 2500);
      return;
    }
    if (r.result.board) {
      // Copy the whole board so it can be pasted into any app right away.
      const copied = await copyText(r.result.all || text);
      toast("done", copied ? "Added to the board and copied" : "Added to the board", 1500);
      return;
    }
    let copied = false;
    if (mode !== "type") copied = await copyText(text);
    if (mode === "clipboard") {
      toast(copied ? "done" : "error", copied ? "Copied to clipboard" : "Couldn't copy to the clipboard.", 1500);
      return;
    }
    const before = charBeforeCaret(d.info);
    const spaced = before && !/\s/.test(before) ? ` ${text}` : text;
    if (d.info.el && insertText(d.info, spaced)) {
      toast("done", copied ? "Inserted and copied" : "Inserted", 1000);
    } else {
      if (!copied) copied = await copyText(text);
      toast(copied ? "done" : "error", copied ? "No text field was focused. Copied to clipboard." : "Click into a text field first.", 2500);
    }
  }

  // ------------------------------------------------------------ Read aloud

  async function readAloud() {
    const target = captureTarget("read");
    toast("busy", "Getting audio…", 0);
    const r = await send({ type: "speakToggle", text: speakable(target) });
    if (!r.ok) toast("error", r.error, 4000);
    else if (r.result === "stopped") toast("done", "Stopped", 900);
  }

  // ------------------------------------------------------------ rewrite

  const BUSY = { refine: "Refining…", professional: "Making it professional…", humanize: "Humanizing…" };
  const DONE = { refine: "Refined", professional: "Professional tone applied", humanize: "Humanized" };

  async function rewrite(style) {
    const target = captureTarget("rewrite");
    if (!target.text.trim()) {
      toast("error", "Select some text or a cell first, or click into a text box.", 2500);
      return;
    }
    toast("busy", BUSY[style], 0);
    const r = await send({ type: "rewrite", text: target.text, style });
    if (!r.ok) {
      toast("error", r.error, 5000);
      return;
    }
    const out = r.result;
    if (out.trim() === target.text.trim()) {
      toast("done", "Already looks good", 1500);
      return;
    }
    if (target.el && insertText(target, out)) {
      toast("done", `${DONE[style]} · Ctrl+Z to undo`, 1800);
      return;
    }
    const copied = await copyText(out);
    if (!copied) toast("error", "Couldn't copy the result.", 3500);
    // A cell (or text in an app like Google Docs) is still selected, so
    // pasting puts the result right back in its place.
    else if (target.source === "app") toast("done", `${DONE[style]} and copied. Press Ctrl+V to put it in place.`, 4000);
    else toast("done", `${DONE[style]} and copied to clipboard (this text can't be edited here)`, 3500);
  }

  // ------------------------------------------------------------ keys

  // No Windows "ding": when Alt+<letter> isn't used by the page as an access
  // key, Chrome passes it on to Windows, which beeps. So for Alt+letter
  // shortcuts VoiceWriter keeps a hidden access-key target and swallows the
  // click Chrome then sends it (the same trick SayIt uses).
  let accessTarget = null;
  function claimAltKey(e) {
    if (!e.altKey || e.ctrlKey || e.metaKey || !/^Key[A-Z]$/.test(e.code)) return;
    if (!accessTarget) {
      accessTarget = document.createElement("voicewriter-key");
      accessTarget.hidden = true;
    }
    if (!accessTarget.isConnected && document.documentElement) document.documentElement.appendChild(accessTarget);
    accessTarget.removeAttribute("accesskey"); // so Chrome re-reads it
    accessTarget.setAttribute("accesskey", e.code.slice(3).toLowerCase());
  }
  for (const type of ["click", "mousedown", "mouseup", "pointerdown", "pointerup"]) {
    on(
      window,
      type,
      (e) => {
        if (!accessTarget || e.target !== accessTarget) return;
        e.preventDefault();
        e.stopImmediatePropagation();
      },
      { capture: true },
    );
  }

  on(
    window,
    "keydown",
    (e) => {
      if (!e.isTrusted) return;
      const combo = VW.comboFromEvent(e);
      if (!combo) return;
      const action = Object.keys(settings.hotkeys).find((a) => settings.hotkeys[a] && settings.hotkeys[a] === combo);
      if (!action) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      claimAltKey(e);
      if (e.repeat) return;
      if (action === "dictate") startDictation(e, combo);
      else if (action === "speak") readAloud();
      else if (action === "live") toggleLive();
      else rewrite(action);
    },
    { capture: true },
  );

  on(
    window,
    "keyup",
    (e) => {
      if (!dict || dict.stopping) return;
      if (e.code === dict.keyCode || dict.mods[e.key]) {
        e.preventDefault();
        stopDictation();
      }
    },
    { capture: true },
  );

  // Switching tabs or windows mid-dictation finishes it, as if released.
  on(window, "blur", () => stopDictation());

  // ------------------------------------------------------------ messages

  function onMessage(msg, _sender, reply) {
    if (!msg || !alive()) return false;
    switch (msg.type) {
      case "vw-menu":
        if (["refine", "professional", "humanize"].includes(msg.action)) rewrite(msg.action);
        return false;
      case "vw-speak":
        if (msg.state === "speaking") toast("speak", `Speaking… ${settings.hotkeys.speak || "Read aloud"} to stop`, 2500);
        else if (msg.state === "error") toast("error", msg.text, 4000);
        else if (msg.state === "done") hideToast();
        return false;
      case "vw-toast":
        if (!isTop) return false;
        toast(msg.kind || "done", msg.text || "", msg.kind === "error" ? 5000 : 2500);
        reply(true);
        return false;
      case "vw-settings":
        refreshSettings();
        return false;
      case "vw-live":
        if (!isTop) return false;
        if (msg.action === "hide") {
          hideLive();
          reply(true);
          return false;
        }
        if (msg.action === "update") {
          renderLive(msg.snapshot);
          reply(true);
          return false;
        }
        showLive(msg.snapshot).then(reply, () => reply(false));
        return true;
      case "vw-get-selection": {
        // The popup's "Use selection" button asks flexibly (a selected cell,
        // the focused box); opening the popup only takes real selections.
        // Only the frame holding the focus answers with anything but a
        // real selection.
        const flexible = !!msg.flexible && ownsFocus();
        const target = flexible ? captureTarget("read") : { ...captureSelection(), source: "selection" };
        const text = speakable(target).trim();
        if (!text) return false;
        reply({ text });
        return false;
      }
      default:
        return false;
    }
  }
  chrome.runtime.onMessage.addListener(onMessage);

  on(
    document,
    TAKEOVER,
    () => {
      lifetime.abort();
      toastHost?.remove();
      hideLive();
      accessTarget?.remove();
      try {
        chrome.runtime.onMessage.removeListener(onMessage);
      } catch {
        /* this copy's extension context is gone */
      }
    },
    { once: true },
  );
})();
