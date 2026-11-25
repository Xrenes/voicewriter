// Shared by the background worker, the extension pages and the content
// scripts. It assigns one global and declares none, so it can safely run
// again in a page that already has an older copy (after an update).
globalThis.VoiceWriterShared = (() => {
  const SAYIT_DEFAULTS = {
    hotkey: { code: "KeyL", key: "L", alt: true, ctrl: false, shift: false, meta: false },
    muteMic: true, // mute your microphone on the call while the message plays
    hearIt: true, // also play the message in your own headset
    ownNoiseCancel: false, // the headset cancels noise itself: turn Chrome's off on dialer calls
    sites: [], // origins added in Settings, e.g. "https://app.agency.com"
    everywhere: false, // SayIt's microphone on every website
  };
  // Dialers SayIt's microphone runs on, and every frame embedded in them.
  // (sayit-inject.js has its own copy of this list.)
  const SAYIT_BUILT_IN = ["aloware.com", "gohighlevel.com", "leadconnectorhq.com", "hubspot.com"];

  const host = (origin) => {
    try {
      return new URL(origin).hostname;
    } catch {
      return "";
    }
  };

  const VW = {
    DEFAULTS: {
      apiKey: "", // main Groq key: dictation and rewrites
      speakKey: "", // optional separate Groq key for Read aloud (falls back to apiKey)
      sttModel: "whisper-large-v3-turbo",
      language: "auto",
      micDevice: "",
      insertion: "type", // type | both | clipboard
      polish: false, // "Clean up with AI" for dictation
      voice: "austin",
      liveTransparency: 60, // how see-through the board (Alt+A) is, in %
      hotkeys: {
        dictate: "Alt+C",
        speak: "Control+Alt+C",
        refine: "Alt+R",
        professional: "Alt+P",
        humanize: "Alt+H",
        live: "Alt+A",
      },
    },

    // Groq Orpheus English voices.
    VOICES: ["austin", "autumn", "daniel", "diana", "hannah", "troy"],

    LABELS: {
      dictate: "Dictate",
      speak: "Read aloud",
      refine: "Refine",
      professional: "Professional",
      humanize: "Humanize",
      live: "Board",
      sayit: "SayIt voicemail",
    },

    // Free-tier daily limits used for the usage meters (same numbers as the
    // desktop app's usage.rs). Estimates only, not account billing.
    LIMITS: {
      dictation: { requests: 2000, audioSec: 28800 },
      speak: { requests: 100 },
    },

    async loadSettings() {
      const s = await chrome.storage.local.get(VW.DEFAULTS);
      s.hotkeys = { ...VW.DEFAULTS.hotkeys, ...(s.hotkeys || {}) };
      return s;
    },

    // "Control+Alt+C" for a keydown event. Null while only modifier keys are
    // held, and for AltGr, so European keyboards can still type ć or ł.
    comboFromEvent(e) {
      if (["Control", "Alt", "Shift", "Meta", "AltGraph"].includes(e.key)) return null;
      if (e.getModifierState && e.getModifierState("AltGraph")) return null;
      const key = VW.keyName(e.code);
      if (!key) return null;
      const parts = [];
      if (e.ctrlKey) parts.push("Control");
      if (e.altKey) parts.push("Alt");
      if (e.shiftKey) parts.push("Shift");
      if (e.metaKey) parts.push("Meta");
      parts.push(key);
      return parts.join("+");
    },

    keyName(code) {
      if (!code) return "";
      if (/^Key[A-Z]$/.test(code)) return code.slice(3);
      if (/^Digit[0-9]$/.test(code)) return code.slice(5);
      return code;
    },

    // Local calendar day, used to reset the daily usage counters.
    today() {
      return new Date().toLocaleDateString("en-CA");
    },

    // ---- SayIt (voicemail drop) ----
    SAYIT: {
      DEFAULTS: SAYIT_DEFAULTS,
      BUILT_IN: SAYIT_BUILT_IN,
      host,
      builtIn(origin) {
        const h = host(origin);
        return SAYIT_BUILT_IN.some((d) => h === d || h.endsWith(`.${d}`));
      },
      // This frame and the frames it's embedded in, innermost first.
      frameOrigins() {
        return [location.origin, ...Array.from(location.ancestorOrigins || [])];
      },
      covers(origins, s) {
        return !!s.everywhere || origins.some((o) => VW.SAYIT.builtIn(o) || (s.sites || []).includes(o));
      },
      // The hotkey object as a VoiceWriter combo string, e.g. "Alt+L".
      combo(h) {
        if (!h || !h.code) return "";
        const parts = [];
        if (h.ctrl) parts.push("Control");
        if (h.alt) parts.push("Alt");
        if (h.shift) parts.push("Shift");
        if (h.meta) parts.push("Meta");
        parts.push(VW.keyName(h.code));
        return parts.join("+");
      },
      async settings() {
        const { sayit } = await chrome.storage.local.get("sayit");
        return { ...SAYIT_DEFAULTS, ...(sayit || {}) };
      },
      base64ToBuffer(b64) {
        const bin = atob(b64);
        const bytes = new Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
        return bytes.buffer;
      },
    },

    // ---- The board (Alt+A) ----
    // Draws a board snapshot into `feed`, touching only what changed so
    // a text selection survives new words arriving. Lines closer than 3 s
    // flow together as one paragraph; dimmed text is still being spoken.
    renderLive(feed, empty, snap, cache) {
      if (!snap) return;
      if (cache.session !== snap.session) {
        cache.session = snap.session;
        cache.shown = -1;
        cache.spans = new Map();
        cache.paras = new Map();
        feed.querySelectorAll("p.line").forEach((p) => p.remove());
      }
      if (snap.version <= cache.shown) return;
      cache.shown = snap.version;
      const stick = feed.scrollHeight - feed.scrollTop - feed.clientHeight < 48;

      const groups = [];
      for (const line of snap.lines) {
        const group = groups[groups.length - 1];
        const last = group && group[group.length - 1];
        if (group && line.start - last.end < 3) group.push(line);
        else groups.push([line]);
      }
      const liveSpans = new Set();
      const liveParas = new Set();
      let prevPara = empty;
      for (const group of groups) {
        const key = group[0].id;
        liveParas.add(key);
        let p = cache.paras.get(key);
        if (!p) {
          p = document.createElement("p");
          p.className = "line";
          cache.paras.set(key, p);
        }
        if (prevPara.nextElementSibling !== p) prevPara.after(p);
        prevPara = p;
        let prevSpan = null;
        for (const line of group) {
          liveSpans.add(line.id);
          let s = cache.spans.get(line.id);
          if (!s) {
            s = document.createElement("span");
            cache.spans.set(line.id, s);
          }
          const cls = line.isFinal ? "" : "partial";
          if (s.className !== cls) s.className = cls;
          const text = `${line.text} `;
          if (s.textContent !== text) s.textContent = text;
          const inPlace = prevSpan ? prevSpan.nextElementSibling === s : p.firstElementChild === s;
          if (!inPlace) {
            if (prevSpan) prevSpan.after(s);
            else p.prepend(s);
          }
          prevSpan = s;
        }
        while (prevSpan && prevSpan.nextElementSibling) prevSpan.nextElementSibling.remove();
      }
      for (const [id, s] of cache.spans) {
        if (!liveSpans.has(id)) {
          s.remove();
          cache.spans.delete(id);
        }
      }
      for (const [id, p] of cache.paras) {
        if (!liveParas.has(id)) {
          p.remove();
          cache.paras.delete(id);
        }
      }
      empty.hidden = snap.lines.length > 0;
      empty.textContent = snap.empty || (snap.active ? "Listening… start talking." : "Stopped.");
      if (stick) feed.scrollTop = feed.scrollHeight;
    },

    // The board as plain text, one paragraph per dictation.
    liveText(snap) {
      if (!snap) return "";
      const out = [];
      let lastEnd = -Infinity;
      for (const l of snap.lines) {
        if (out.length && l.start - lastEnd < 3) out[out.length - 1] += ` ${l.text}`;
        else out.push(l.text);
        lastEnd = l.end;
      }
      return out.join("\n\n");
    },
  };
  return VW;
})();
