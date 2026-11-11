// Runs inside the dialer page itself (MAIN world), before the page asks for
// the microphone. It hands the page a mic stream that normally carries the
// real microphone, and carries the saved message instead while it plays —
// so the other side of the call (or their voicemail) hears the message.
(() => {
  if (window.__sayitInstalled) return;
  window.__sayitInstalled = true;
  const media = navigator.mediaDevices;
  if (!media || !media.getUserMedia) return;

  const realGetUserMedia = media.getUserMedia.bind(media);
  const calls = new Set(); // { micGain, dest } per microphone stream handed out
  let ctx = null;
  let clipBytes = null;
  let clip = null; // decoded AudioBuffer
  let current = null; // { sources } while the message plays

  const audioCtx = () => (ctx ||= new AudioContext());
  const report = (state) => window.postMessage({ __sayit: "from-page", state }, "*");

  async function decodeClip() {
    clip = null;
    if (!clipBytes) return;
    try {
      clip = await audioCtx().decodeAudioData(clipBytes.slice(0));
    } catch (e) {
      console.warn("SayIt: couldn't read the saved message", e);
    }
  }

  media.getUserMedia = async function (constraints) {
    const stream = await realGetUserMedia(constraints);
    if (!constraints || !constraints.audio || stream.getAudioTracks().length === 0) return stream;
    try {
      const ac = audioCtx();
      if (ac.state === "suspended") ac.resume().catch(() => {});
      const source = ac.createMediaStreamSource(new MediaStream(stream.getAudioTracks()));
      const micGain = ac.createGain();
      const dest = ac.createMediaStreamDestination();
      source.connect(micGain).connect(dest);
      const call = { micGain, dest };
      calls.add(call);

      const track = dest.stream.getAudioTracks()[0];
      const stopOwn = track.stop.bind(track);
      // When the page hangs up and stops "its" mic, release the real one too.
      track.stop = () => {
        stopOwn();
        stream.getAudioTracks().forEach((t) => t.stop());
        source.disconnect();
        calls.delete(call);
      };
      return new MediaStream([track, ...stream.getVideoTracks()]);
    } catch (e) {
      console.warn("SayIt: falling back to the plain microphone", e);
      return stream;
    }
  };

  function restoreMic() {
    const now = audioCtx().currentTime;
    calls.forEach((c) => c.micGain.gain.setValueAtTime(1, now));
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
    restoreMic();
    report("stopped");
  }

  function play() {
    if (!clip) return report("no-message");
    if (calls.size === 0) return report("no-call");
    const ac = audioCtx();
    ac.resume().catch(() => {});
    const sources = [];
    calls.forEach((c) => {
      c.micGain.gain.setValueAtTime(0, ac.currentTime); // mute the mic meanwhile
      const s = ac.createBufferSource();
      s.buffer = clip;
      s.connect(c.dest);
      s.start();
      sources.push(s);
    });
    current = { sources };
    sources[0].onended = () => {
      if (current && current.sources === sources) {
        current = null;
        restoreMic();
        report("done");
      }
    };
    report("playing");
  }

  window.addEventListener("message", (e) => {
    if (e.source !== window || !e.data || e.data.__sayit !== "to-page") return;
    if (e.data.type === "toggle") current ? stop() : play();
    else if (e.data.type === "clip") {
      clipBytes = e.data.bytes || null;
      decodeClip();
    } else if (e.data.type === "clear") {
      clipBytes = null;
      clip = null;
    }
  });
  report("ready");
})();
