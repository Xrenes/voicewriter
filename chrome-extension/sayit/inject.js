// Runs inside the dialer page itself (MAIN world) before the page asks for
// the microphone. The page gets a mic stream that carries the real
// microphone, or the saved message while it plays — so the other side of the
// call (or their voicemail) hears the message.
//
// The hotkey is caught right here in the page and starts the pre-decoded
// message synchronously: no round trip through the extension, so there's
// no added delay between the key press and the sound.
//
// Messages to and from this script are told apart by their `__sayit` field
// alone (no generic `type` field), so the dialer's own handlers ignore them.
(() => {
  if (window.__sayitInstalled) return;
  window.__sayitInstalled = true;

  const calls = new Set(); // { micGain, dest } per microphone stream handed out
  let ctx = null;
  let clip = null; // decoded AudioBuffer, ready to play
  let current = null; // { sources } while the message plays
  let hotkey = { code: "KeyL", alt: true, ctrl: false, shift: false, meta: false };
  let muteMic = true;
  let hearIt = true; // also play it quietly in your own headset
  let asked = null; // a toggle passed to the other frames, until one of them takes it

  const audioCtx = () => {
    if (!ctx) {
      ctx = new AudioContext({ latencyHint: "interactive" });
      if (sinkId && ctx.setSinkId) ctx.setSinkId(sinkId).catch(() => {});
    }
    return ctx;
  };
  const post = (msg) => window.postMessage(msg, "*");
  const report = (state) => post({ __sayit: "state", state, at: performance.now() });

  // Read by the SayIt popup for its "This tab" line.
  Object.defineProperty(window, "__sayitStatus", {
    value: () => ({ call: calls.size > 0, playing: !!current, ready: !!clip }),
  });

  // Play your preview on the same output the dialer plays the call on (its
  // chosen headset); until it picks one, Windows' default output.
  let sinkId = "";
  const followSink = (id) => {
    if (typeof id !== "string" || id === sinkId) return;
    sinkId = id;
    if (ctx && ctx.setSinkId) ctx.setSinkId(id).catch(() => {});
  };
  window.__sayitSink = () => (ctx ? ctx.sinkId : sinkId);
  for (const proto of [window.HTMLMediaElement && HTMLMediaElement.prototype, window.AudioContext && AudioContext.prototype]) {
    if (!proto || !proto.setSinkId) continue;
    const real = proto.setSinkId;
    proto.setSinkId = function (id) {
      if (this !== ctx) followSink(id);
      return real.apply(this, arguments);
    };
  }

  const media = navigator.mediaDevices;
  if (media && media.getUserMedia) {
    const realGetUserMedia = media.getUserMedia.bind(media);
    media.getUserMedia = async function (constraints) {
      const stream = await realGetUserMedia(constraints);
      const mic = stream.getAudioTracks()[0];
      if (!constraints || !constraints.audio || !mic) return stream;
      try {
        const ac = audioCtx();
        if (ac.state !== "running") {
          await Promise.race([ac.resume(), new Promise((r) => setTimeout(r, 300))]);
          if (ac.state !== "running") {
            // Never risk a silent call: without a running audio engine, hand
            // over the plain microphone (SayIt is off for this call only).
            console.warn("SayIt: audio is blocked until you click the page — this call uses your plain microphone.");
            return stream;
          }
        }
        const source = ac.createMediaStreamSource(new MediaStream([mic]));
        const micGain = ac.createGain();
        const dest = ac.createMediaStreamDestination();
        dest.channelCount = 1;
        source.connect(micGain).connect(dest);
        const call = { micGain, dest, muted: false };
        calls.add(call);
        const track = dest.stream.getAudioTracks()[0];
        mirror(track, mic, call, () => {
          source.disconnect();
          calls.delete(call);
        });
        return new MediaStream([track, ...stream.getVideoTracks()]);
      } catch (e) {
        console.warn("SayIt: using the plain microphone", e);
        return stream;
      }
    };
  }

  // Make the handed-out track behave like the real mic to the dialer.
  function mirror(track, mic, call, cleanup) {
    // The dialer's mute button switches off your voice only, so the message
    // still plays if you're muted when you press the hotkey.
    Object.defineProperty(track, "enabled", {
      configurable: true,
      get: () => !call.muted,
      set: (on) => {
        call.muted = !on;
        setMicGain(call);
      },
    });
    const stopOwn = track.stop.bind(track);
    track.stop = () => {
      stopOwn();
      mic.stop();
      cleanup();
    };
    track.getSettings = () => mic.getSettings();
    track.getConstraints = () => mic.getConstraints();
    track.applyConstraints = (c) => mic.applyConstraints(c);
    if (mic.getCapabilities) track.getCapabilities = () => mic.getCapabilities();
    try {
      Object.defineProperty(track, "label", { get: () => mic.label });
    } catch {}
    mic.addEventListener("ended", () => {
      stopOwn();
      cleanup();
      track.dispatchEvent(new Event("ended"));
    });
  }

  // Your voice goes out unless the dialer muted it, or the message is
  // playing (with "mute my microphone" on).
  function setMicGain(c) {
    const t = audioCtx().currentTime;
    const on = !c.muted && !(current && muteMic);
    c.micGain.gain.cancelScheduledValues(t);
    c.micGain.gain.setValueAtTime(on ? 1 : 0, t);
  }

  const restoreMic = () => calls.forEach(setMicGain);

  function stop() {
    if (!current) return;
    current.sources.forEach((s) => {
      s.onended = null;
      try {
        s.stop();
      } catch {}
    });
    current = null;
    restoreMic();
    report("stopped");
  }

  function play(quiet) {
    if (!clip) return quiet || report("no-message");
    if (calls.size === 0) return quiet || report("no-call");
    const ac = audioCtx();
    const sources = [];
    calls.forEach((c) => {
      const s = ac.createBufferSource();
      s.buffer = clip;
      s.connect(c.dest);
      s.start();
      sources.push(s);
    });
    if (hearIt) {
      // So you hear exactly when it starts and ends (only you; not the call).
      const s = ac.createBufferSource();
      const g = ac.createGain();
      g.gain.value = 0.6;
      s.buffer = clip;
      s.connect(g).connect(ac.destination);
      s.start();
      sources.push(s);
    }
    current = { sources };
    calls.forEach(setMicGain);
    sources[0].onended = () => {
      if (current && current.sources === sources) {
        current = null;
        restoreMic();
        report("done");
      }
    };
    report("playing");
  }

  const toggle = (quiet) => (current ? stop() : play(quiet));

  function allFrames(w, out = []) {
    out.push(w);
    try {
      for (let i = 0; i < w.frames.length; i++) allFrames(w.frames[i], out);
    } catch {}
    return out;
  }

  // The call may be in another frame than the one with keyboard focus, or in
  // another tab.
  function toggleInCallFrame() {
    if (calls.size || current) return toggle(false);
    const others = allFrames(window.top).filter((f) => f !== window);
    if (!others.length) return elsewhere();
    const id = `${performance.now()}-${Math.random()}`;
    asked = id;
    for (const f of others) {
      try {
        f.postMessage({ __sayit: "frame-toggle", id }, "*");
      } catch {}
    }
    setTimeout(() => {
      if (asked !== id) return;
      asked = null;
      elsewhere();
    }, 150);
  }

  // No frame of this tab has a call: the extension passes the toggle on to
  // the other dialer tabs. (SayIt's own test page has nowhere else to look.)
  function elsewhere() {
    if (location.protocol === "chrome-extension:") report("no-call");
    else post({ __sayit: "toggle-elsewhere" });
  }

  const sameCombo = (e) =>
    e.code === hotkey.code &&
    e.altKey === !!hotkey.alt &&
    e.ctrlKey === !!hotkey.ctrl &&
    e.shiftKey === !!hotkey.shift &&
    e.metaKey === !!hotkey.meta;

  window.addEventListener(
    "keydown",
    (e) => {
      if (!hotkey || !hotkey.code || !sameCombo(e)) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      if (!e.repeat) toggleInCallFrame();
    },
    true,
  );

  window.addEventListener("message", (e) => {
    const d = e.data;
    const kind = d && d.__sayit;
    if (!kind) return;

    // Between the frames of one tab: a toggle passed along, or the answer to ours.
    if (kind === "frame-toggle") {
      if (!calls.size && !current) return;
      toggle(true);
      try {
        e.source.postMessage({ __sayit: "frame-taken", id: d.id }, "*");
      } catch {}
      return;
    }
    if (kind === "frame-taken") {
      if (d.id === asked) asked = null;
      return;
    }

    // From the extension (relay.js) in this same window.
    if (e.source !== window) return;
    if (kind === "toggle") toggle(true);
    else if (kind === "config") {
      if (d.hotkey) hotkey = d.hotkey;
      if (typeof d.muteMic === "boolean") muteMic = d.muteMic;
      if (typeof d.hearIt === "boolean") hearIt = d.hearIt;
    } else if (kind === "clip") {
      // Decode ahead of time (offline, so no audio permission is needed) —
      // the hotkey then only has to start it.
      new OfflineAudioContext(1, 1, 48000)
        .decodeAudioData(d.bytes)
        .then((buf) => (clip = buf))
        .catch((err) => {
          clip = null;
          console.warn("SayIt: couldn't read the saved message", err);
        });
    } else if (kind === "clear") {
      clip = null;
    }
  });

  report("ready");
})();
