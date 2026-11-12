// SayIt inside the page itself (MAIN world), in every frame of every website.
//
// - The hotkey works on any page, and the whole keystroke is claimed, so
//   neither the page nor Windows (no "ding") ever gets it.
// - On dialer pages (Aloware, GHL, HubSpot, sites added in Settings) and in
//   everything embedded in them — a dialer inside a CRM, a calling window —
//   the page's microphone stream is SayIt's: your real microphone, or the
//   saved message while it plays. So the call (or the voicemail) hears it.
// - The background (background.js) knows every frame that holds a call
//   microphone, in every tab and window, and sends the message to the call
//   that's actually connected. When the hotkey is pressed in the frame that
//   has that call, it starts right here, with no round trip at all.
//
// Messages to and from this script are told apart by their `__sayit` field
// alone, so the page's own message handlers ignore them.
(() => {
  if (window.__sayitInstalled) return;
  window.__sayitInstalled = true;

  // ---- Where SayIt's microphone runs ----
  // Same list as SAYIT_BUILT_IN in common.js (this script can't load it).
  const BUILT_IN = ["aloware.com", "gohighlevel.com", "leadconnectorhq.com", "hubspot.com"];
  const hostOf = (origin) => {
    try {
      return new URL(origin).hostname;
    } catch {
      return "";
    }
  };
  // This frame and the frames it's embedded in, innermost first.
  const origins = [location.origin, ...Array.from(location.ancestorOrigins || [])];
  const where = origins.map(hostOf).filter(Boolean);
  const builtInHere =
    location.protocol === "chrome-extension:" ||
    origins.some((o) => {
      const h = hostOf(o);
      return BUILT_IN.some((d) => h === d || h.endsWith(`.${d}`));
    });
  let covered = builtInHere; // + sites added in Settings, from relay.js
  let configured = false;
  const onConfigured = [];

  const calls = new Set(); // { micGain, dest, muted, pcs } per microphone stream handed out
  const callOf = new WeakMap(); // handed-out track → its call
  let ctx = null;
  let clip = null; // decoded AudioBuffer, ready to play
  let span = [0, 0]; // [start, length] in seconds: the voice, without silence around it
  let current = null; // { sources, targets } while the message plays
  let hotkey = { code: "KeyL", key: "L", alt: true, ctrl: false, shift: false, meta: false };
  let muteMic = true;
  let hearIt = true; // also play it in your own headset
  let newestCallAt = 0; // when the newest microphone stream was handed out
  let liveAt = 0; // when a call here last got connected
  let sinkId = ""; // the output the dialer plays the call on

  const post = (msg) => window.postMessage(msg, "*");
  const report = (state) => post({ __sayit: "state", state, at: performance.now() });

  const audioCtx = () => {
    if (!ctx) {
      ctx = new AudioContext({ latencyHint: "interactive" });
      if (sinkId && ctx.setSinkId) ctx.setSinkId(sinkId).catch(() => {});
    }
    return ctx;
  };

  // ---- Which call is live: the one whose microphone is in a connected WebRTC call ----
  const pcLive = (pc) =>
    pc.connectionState === "connected" || pc.iceConnectionState === "connected" || pc.iceConnectionState === "completed";
  const isLive = (c) => {
    for (const pc of c.pcs) if (pcLive(pc)) return true;
    return false;
  };
  const liveCalls = () => [...calls].filter(isLive);

  // What the background needs to route the hotkey (sent when it changes, and
  // every 15 s during a call, which also keeps the background awake).
  let lastStatus = "";
  let heartbeat = 0;
  function status(force) {
    if (!covered) return;
    const live = liveCalls().length;
    const st = { calls: calls.size, live, playing: !!current, ready: !!clip, newestCallAt, liveAt, where };
    const key = JSON.stringify(st);
    if (!force && key === lastStatus) return;
    lastStatus = key;
    post({ __sayit: "status", status: st });
    if (calls.size && !heartbeat) heartbeat = setInterval(() => status(true), 15000);
    if (!calls.size && heartbeat) heartbeat = clearInterval(heartbeat) || 0;
  }
  function liveChanged() {
    let anyLive = false;
    calls.forEach((c) => {
      const live = isLive(c);
      if (live && !c.wasLive) liveAt = Date.now();
      c.wasLive = live;
      anyLive ||= live;
    });
    status();
  }

  const senderPc = new WeakMap();
  function sentOn(pc, track) {
    const call = track && callOf.get(track);
    if (!pc || !call || call.pcs.has(pc)) return;
    call.pcs.add(pc);
    pc.addEventListener("connectionstatechange", liveChanged);
    pc.addEventListener("iceconnectionstatechange", liveChanged);
    liveChanged();
  }
  function hook(proto, name, fn) {
    const real = proto && proto[name];
    if (typeof real !== "function") return;
    proto[name] = function (...args) {
      return fn.call(this, real, args);
    };
  }
  const PC = window.RTCPeerConnection;
  if (PC) {
    const P = PC.prototype;
    hook(P, "addTrack", function (real, args) {
      const sender = real.apply(this, args);
      if (sender) senderPc.set(sender, this);
      sentOn(this, args[0]);
      return sender;
    });
    hook(P, "addTransceiver", function (real, args) {
      const tr = real.apply(this, args);
      if (tr && tr.sender) senderPc.set(tr.sender, this);
      if (args[0] && typeof args[0] === "object") sentOn(this, args[0]);
      return tr;
    });
    hook(P, "addStream", function (real, args) {
      const r = real.apply(this, args);
      if (args[0] && args[0].getAudioTracks) args[0].getAudioTracks().forEach((t) => sentOn(this, t));
      return r;
    });
    hook(P, "getSenders", function (real, args) {
      const list = real.apply(this, args);
      list.forEach((s) => senderPc.set(s, this));
      return list;
    });
    hook(P, "getTransceivers", function (real, args) {
      const list = real.apply(this, args);
      list.forEach((t) => t.sender && senderPc.set(t.sender, this));
      return list;
    });
    hook(P, "close", function (real, args) {
      const r = real.apply(this, args);
      liveChanged();
      return r;
    });
    if (window.RTCRtpSender) {
      hook(RTCRtpSender.prototype, "replaceTrack", function (real, args) {
        sentOn(senderPc.get(this), args[0]);
        return real.apply(this, args);
      });
    }
  }
  // A copy of SayIt's track belongs to the same call.
  hook(window.MediaStreamTrack && MediaStreamTrack.prototype, "clone", function (real, args) {
    const copy = real.apply(this, args);
    const call = callOf.get(this);
    if (call && copy) callOf.set(copy, call);
    return copy;
  });

  // ---- The preview plays on the output the dialer uses for the call ----
  const followSink = (id) => {
    if (typeof id !== "string" || id === sinkId) return;
    sinkId = id;
    if (ctx && ctx.setSinkId) ctx.setSinkId(id).catch(() => {});
  };
  for (const proto of [window.HTMLMediaElement && HTMLMediaElement.prototype, window.AudioContext && AudioContext.prototype]) {
    hook(proto, "setSinkId", function (real, args) {
      if (this !== ctx) followSink(args[0]);
      return real.apply(this, args);
    });
  }

  // While a call is up, keep the audio output awake with an inaudible signal
  // (-120 dB): Chrome parks a silent output after 30 s, and waking it would
  // delay the preview you hear.
  let keepAwake = null;
  function updateKeepAwake() {
    const want = calls.size > 0 && hearIt;
    if (want && !keepAwake) {
      keepAwake = audioCtx().createConstantSource();
      keepAwake.offset.value = 1e-6;
      keepAwake.connect(ctx.destination);
      keepAwake.start();
    } else if (!want && keepAwake) {
      keepAwake.stop();
      keepAwake.disconnect();
      keepAwake = null;
    }
  }

  // ---- The microphone the page gets ----
  const media = navigator.mediaDevices;
  if (media && media.getUserMedia) {
    const realGetUserMedia = media.getUserMedia.bind(media);
    media.getUserMedia = async function (constraints) {
      const stream = await realGetUserMedia(constraints);
      const mic = stream.getAudioTracks()[0];
      if (!constraints || !constraints.audio || !mic) return stream;
      if (!covered && !configured) {
        // A site added in Settings: its settings arrive from relay.js.
        await new Promise((r) => {
          onConfigured.push(r);
          setTimeout(r, 500);
        });
      }
      if (!covered) return stream;
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
        const call = { micGain, dest, muted: false, pcs: new Set(), wasLive: false };
        const track = dest.stream.getAudioTracks()[0];
        callOf.set(track, call);
        calls.add(call);
        newestCallAt = Date.now();
        updateKeepAwake();
        mirror(track, mic, call, () => {
          source.disconnect();
          calls.delete(call);
          updateKeepAwake();
          status();
        });
        status();
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

  // ---- Playing the message ----
  // Your voice goes out unless the dialer muted it, or the message is playing
  // on that call (with "mute my microphone" on).
  function setMicGain(c) {
    const t = audioCtx().currentTime;
    const on = !c.muted && !(current && muteMic && current.targets.has(c));
    c.micGain.gain.cancelScheduledValues(t);
    c.micGain.gain.setValueAtTime(on ? 1 : 0, t);
  }

  function stop() {
    if (!current) return;
    current.sources.forEach((s) => {
      s.onended = null;
      try {
        s.stop();
      } catch {}
    });
    current = null;
    calls.forEach(setMicGain);
    report("stopped");
    status();
  }

  function play() {
    if (current) return;
    if (!clip) {
      report("no-message");
      return status(true);
    }
    // Into the connected call(s) only; other microphones here stay as they are.
    const live = liveCalls();
    const targets = live.length ? live : [...calls];
    if (!targets.length) {
      report("no-call");
      return status(true);
    }
    const ac = audioCtx();
    const sources = [];
    for (const c of targets) {
      const s = ac.createBufferSource();
      s.buffer = clip;
      s.connect(c.dest);
      s.start(0, span[0], span[1]);
      sources.push(s);
    }
    if (hearIt) {
      // So you hear exactly when it starts and ends (only you; not the call).
      const s = ac.createBufferSource();
      const g = ac.createGain();
      g.gain.value = 0.6;
      s.buffer = clip;
      s.connect(g).connect(ac.destination);
      s.start(0, span[0], span[1]);
      sources.push(s);
    }
    current = { sources, targets: new Set(targets) };
    calls.forEach(setMicGain);
    sources[0].onended = () => {
      if (!current || current.sources !== sources) return;
      current = null;
      calls.forEach(setMicGain);
      report("done");
      status();
    };
    report("playing");
    status();
  }

  // Where the voice actually starts and ends (5 ms frames at least 6% as loud
  // as the loudest one), so the first word comes out the instant the hotkey
  // is pressed — room noise or a pause before you spoke is skipped.
  function voiceSpan(buf) {
    const d = buf.getChannelData(0);
    const rate = buf.sampleRate;
    const frame = Math.max(1, Math.round(rate * 0.005));
    const rms = [];
    let loudest = 0;
    for (let i = 0; i < d.length; i += frame) {
      let acc = 0;
      const end = Math.min(d.length, i + frame);
      for (let j = i; j < end; j++) acc += d[j] * d[j];
      const r = Math.sqrt(acc / (end - i));
      rms.push(r);
      if (r > loudest) loudest = r;
    }
    if (!loudest) return [0, buf.duration];
    const voiced = (r) => r >= loudest * 0.06;
    const first = Math.max(0, rms.findIndex(voiced));
    let last = rms.length - 1;
    while (last > first && !voiced(rms[last])) last--;
    const start = Math.max(0, first * frame - Math.round(rate * 0.01)); // keep 10 ms of the onset
    const end = Math.min(d.length, (last + 1) * frame + Math.round(rate * 0.06));
    return [start / rate, (end - start) / rate];
  }

  // ---- The hotkey, on every page ----
  // No Windows "ding": when Alt+<letter> isn't used by the page as an access
  // key, Chrome passes it on to Windows, which beeps. So for an Alt+letter
  // hotkey SayIt keeps a hidden access-key target, and swallows the click
  // Chrome then sends it, so the page never sees a thing.
  let accessTarget = null;
  let altKeysClaimed = 0;
  function claimAltKey() {
    if (!hotkey.alt || hotkey.ctrl || hotkey.meta) return;
    const ch = String(hotkey.key || "").toLowerCase();
    if (ch.length !== 1 || !document.documentElement) return;
    if (!accessTarget) {
      accessTarget = document.createElement("sayit-key");
      accessTarget.hidden = true;
    }
    if (!accessTarget.isConnected) document.documentElement.appendChild(accessTarget);
    accessTarget.removeAttribute("accesskey"); // so Chrome re-reads it
    accessTarget.setAttribute("accesskey", ch);
  }
  for (const type of ["click", "mousedown", "mouseup", "pointerdown", "pointerup"]) {
    window.addEventListener(
      type,
      (e) => {
        if (!accessTarget || e.target !== accessTarget) return;
        e.preventDefault();
        e.stopImmediatePropagation();
        if (type === "click") altKeysClaimed++;
      },
      true,
    );
  }

  function onHotkey() {
    if (current) return stop(); // playing here: stop it here
    if (liveCalls().length) return play(); // the connected call is here: start now
    post({ __sayit: "hotkey" }); // the background finds the call: another frame, tab or window
  }

  const sameCombo = (e) =>
    e.code === hotkey.code &&
    e.altKey === !!hotkey.alt &&
    e.ctrlKey === !!hotkey.ctrl &&
    e.shiftKey === !!hotkey.shift &&
    e.metaKey === !!hotkey.meta;

  // The whole keystroke is SayIt's: the page (and its keypad tones) never
  // sees the key going down or coming back up.
  let heldCode = null;
  window.addEventListener(
    "keydown",
    (e) => {
      if (!hotkey || !hotkey.code || !sameCombo(e)) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      heldCode = e.code;
      claimAltKey();
      if (!e.repeat) onHotkey();
    },
    true,
  );
  window.addEventListener(
    "keyup",
    (e) => {
      if (!heldCode || e.code !== heldCode) return;
      heldCode = null;
      e.preventDefault();
      e.stopImmediatePropagation();
    },
    true,
  );

  // ---- Messages from relay.js (the extension), in this same window ----
  window.addEventListener("message", (e) => {
    const d = e.data;
    if (e.source !== window || !d || !d.__sayit) return;
    switch (d.__sayit) {
      case "play":
        return play();
      case "stop":
        return stop();
      case "no-call":
        return report("no-call");
      case "config":
        if (d.hotkey) hotkey = d.hotkey;
        if (typeof d.muteMic === "boolean") muteMic = d.muteMic;
        if (typeof d.hearIt === "boolean") hearIt = d.hearIt;
        covered = builtInHere || !!d.covered;
        configured = true;
        onConfigured.splice(0).forEach((r) => r());
        if (ctx) updateKeepAwake();
        return;
      case "clip":
        // Decode ahead of time (offline, so no audio permission is needed) —
        // the hotkey then only has to start it.
        new OfflineAudioContext(1, 1, 48000)
          .decodeAudioData(d.bytes)
          .then((buf) => {
            span = voiceSpan(buf);
            clip = buf;
            status();
          })
          .catch((err) => {
            clip = null;
            console.warn("SayIt: couldn't read the saved message", err);
          });
        return;
      case "clear":
        clip = null;
        return status();
    }
  });

  // For tests and troubleshooting.
  Object.defineProperty(window, "__sayitStatus", {
    value: () => ({ covered, call: calls.size > 0, live: liveCalls().length > 0, playing: !!current, ready: !!clip, altKeysClaimed, sink: ctx ? ctx.sinkId : sinkId }),
  });

  report("ready");
})();
