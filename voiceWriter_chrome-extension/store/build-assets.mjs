// Renders the Chrome Web Store images into store/:
//   screenshot-1.png … screenshot-4.png  (1280x800)
//   promo-small.png                      (440x280)
// It first captures the real popup into src/popup.png: VoiceWriter is loaded
// as an unpacked extension in headless Chrome, with a saved SayIt message and
// a pretend dialer (a real WebRTC call to itself on dialer.example.com), so
// the SayIt card shows a live call.
//
// Usage:  node store/build-assets.mjs ["path\to\chrome.exe"]
// Needs Chrome 126+ (uses the DevTools Extensions.loadUnpacked command).
import { spawn } from "node:child_process";
import http from "node:http";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const STORE = path.dirname(fileURLToPath(import.meta.url));
const EXT = path.resolve(STORE, "..");
const SRC = path.join(STORE, "src");
const CHROME =
  process.argv[2] ||
  [
    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
    "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/usr/bin/google-chrome",
  ].find((p) => fs.existsSync(p));
if (!CHROME) throw new Error("Chrome not found. Pass its path as the first argument.");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// A pretend dialer: clicking Call starts a WebRTC call to itself.
const DIALER = `<!doctype html><title>Dialer</title><button id="call">Call</button><script>
document.getElementById("call").onclick = async () => {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const a = new RTCPeerConnection(), b = new RTCPeerConnection();
  a.onicecandidate = (e) => e.candidate && b.addIceCandidate(e.candidate);
  b.onicecandidate = (e) => e.candidate && a.addIceCandidate(e.candidate);
  stream.getAudioTracks().forEach((t) => a.addTrack(t, stream));
  const offer = await a.createOffer(); await a.setLocalDescription(offer); await b.setRemoteDescription(offer);
  const answer = await b.createAnswer(); await b.setLocalDescription(answer); await a.setRemoteDescription(answer);
  window.pcs = [a, b];
};
</script>`;
const server = http.createServer((_req, res) => {
  res.writeHead(200, { "content-type": "text/html" });
  res.end(DIALER);
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const PORT = server.address().port;
const DIALER_ORIGIN = `http://dialer.example.com:${PORT}`;

const profile = fs.mkdtempSync(path.join(os.tmpdir(), "vw-store-"));
const chrome = spawn(
  CHROME,
  [
    "--headless=new",
    "--remote-debugging-pipe",
    "--enable-unsafe-extension-debugging",
    `--user-data-dir=${profile}`,
    "--no-first-run",
    "--no-default-browser-check",
    "--autoplay-policy=no-user-gesture-required",
    "--use-fake-device-for-media-stream",
    "--use-fake-ui-for-media-stream",
    "--host-resolver-rules=MAP dialer.example.com 127.0.0.1",
    `--unsafely-treat-insecure-origin-as-secure=${DIALER_ORIGIN}`,
    "--hide-scrollbars",
    "about:blank",
  ],
  { stdio: ["ignore", "ignore", "ignore", "pipe", "pipe"] },
);

let nextId = 0;
const pending = new Map();
const listeners = [];
let buf = "";
chrome.stdio[4].on("data", (chunk) => {
  buf += chunk.toString("utf8");
  let i;
  while ((i = buf.indexOf("\0")) >= 0) {
    const msg = JSON.parse(buf.slice(0, i));
    buf = buf.slice(i + 1);
    const p = msg.id && pending.get(msg.id);
    if (p) {
      pending.delete(msg.id);
      msg.error ? p.reject(new Error(`${p.method}: ${msg.error.message}`)) : p.resolve(msg.result);
    } else listeners.forEach((l) => l(msg));
  }
});
function cdp(method, params = {}, sessionId) {
  const msg = { id: ++nextId, method, params, ...(sessionId ? { sessionId } : {}) };
  chrome.stdio[3].write(JSON.stringify(msg) + "\0");
  return new Promise((resolve, reject) => pending.set(msg.id, { resolve, reject, method }));
}
async function open(url, width, height, scale = 1) {
  const { targetId } = await cdp("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await cdp("Target.attachToTarget", { targetId, flatten: true });
  await cdp("Page.enable", {}, sessionId);
  await cdp("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: scale, mobile: false }, sessionId);
  await cdp("Page.navigate", { url }, sessionId);
  await sleep(1200);
  return sessionId;
}
async function js(sessionId, expression) {
  const r = await cdp("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true }, sessionId);
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
  return r.result.value;
}
async function shot(sessionId, file) {
  await cdp("Page.bringToFront", {}, sessionId);
  await sleep(250);
  const { data } = await cdp("Page.captureScreenshot", { format: "png" }, sessionId);
  fs.writeFileSync(file, Buffer.from(data, "base64"));
  console.log("wrote", path.relative(EXT, file));
}

// Ten seconds of silence, so the popup stays in its "Speaking" state.
function silentWav(seconds, rate = 24000) {
  const n = rate * seconds;
  const b = Buffer.alloc(44 + n * 2);
  b.write("RIFF", 0);
  b.writeUInt32LE(36 + n * 2, 4);
  b.write("WAVEfmt ", 8);
  b.writeUInt32LE(16, 16);
  b.writeUInt16LE(1, 20);
  b.writeUInt16LE(1, 22);
  b.writeUInt32LE(rate, 24);
  b.writeUInt32LE(rate * 2, 28);
  b.writeUInt16LE(2, 32);
  b.writeUInt16LE(16, 34);
  b.write("data", 36);
  b.writeUInt32LE(n * 2, 40);
  return b;
}

try {
  await cdp("Target.setDiscoverTargets", { discover: true });
  const { id: extId } = await cdp("Extensions.loadUnpacked", { path: EXT });
  await sleep(1500);
  const { targetInfos } = await cdp("Target.getTargets");
  const sw = targetInfos.find((t) => t.type === "service_worker" && t.url.includes(extId));
  const { sessionId: swS } = await cdp("Target.attachToTarget", { targetId: sw.targetId, flatten: true });
  await cdp("Fetch.enable", { patterns: [{ urlPattern: "https://api.groq.com/*" }] }, swS);
  const wav = silentWav(10).toString("base64");
  listeners.push((m) => {
    if (m.method !== "Fetch.requestPaused") return;
    const speech = m.params.request.url.endsWith("/audio/speech");
    cdp(
      "Fetch.fulfillRequest",
      {
        requestId: m.params.requestId,
        responseCode: 200,
        responseHeaders: [{ name: "content-type", value: speech ? "audio/wav" : "application/json" }],
        body: speech ? wav : Buffer.from('{"data":[]}').toString("base64"),
      },
      swS,
    ).catch(() => {});
  });

  // 1. The real popup: a saved SayIt message and a live call on the pretend dialer.
  const popupUrl = `chrome-extension://${extId}/popup.html`;
  const setup = await open(popupUrl, 380, 600, 2);
  const voicemail = silentWav(12.4, 24000).toString("base64");
  await js(
    setup,
    `chrome.storage.local.set({
       apiKey: "gsk_storeScreenshotKey000000000000000000",
       sayit: { sites: ["${DIALER_ORIGIN}"] },
       sayitMessage: { data: "${voicemail}", type: "audio/wav", secs: 12.4 }
     }).then(() => true)`,
  );
  const dialer = await open(`${DIALER_ORIGIN}/`, 800, 600, 1);
  await cdp("Page.bringToFront", {}, dialer);
  const at = await js(dialer, `(() => { const r = document.getElementById("call").getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()`);
  for (const type of ["mousePressed", "mouseReleased"]) {
    await cdp("Input.dispatchMouseEvent", { type, x: at.x, y: at.y, button: "left", clickCount: 1 }, dialer);
  }
  for (let i = 0; i < 30; i++) {
    await sleep(300);
    const live = await js(dialer, `!!(window.__vwSayitStatus && window.__vwSayitStatus().live)`);
    if (live) break;
  }
  const popup = await open(popupUrl, 380, 600, 2);
  await js(
    popup,
    `document.documentElement.classList.remove("in-tab");
     document.getElementById("useSelection").hidden = false;
     document.getElementById("sayText").value = "Thanks for sending the proposal over. I've gone through it, and the timeline looks good to me.";
     true`,
  );
  await sleep(1800);
  await shot(popup, path.join(SRC, "popup.png"));

  // 2. Store images.
  for (const [name, w, h] of [
    ["screenshot-1", 1280, 800],
    ["screenshot-2", 1280, 800],
    ["screenshot-3", 1280, 800],
    ["screenshot-4", 1280, 800],
    ["promo-small", 440, 280],
  ]) {
    const s = await open(pathToFileURL(path.join(SRC, `${name}.html`)).href, w, h, 1);
    await shot(s, path.join(STORE, `${name}.png`));
  }
} finally {
  chrome.kill();
  server.close();
  setTimeout(() => {
    try {
      fs.rmSync(profile, { recursive: true, force: true });
    } catch {
      /* Chrome may still hold files for a moment */
    }
    process.exit(0);
  }, 800);
}
