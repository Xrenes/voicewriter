# VoiceWriter

Hold **Alt+C** while speaking, then release to type your words into the focused text field.
Wait for transcription to finish before starting another recording.
No window, no cloud — transcription runs locally with whisper.cpp (or Groq, if you set an API key).

- **Website:** https://xrenes.github.io/voicewriter/
- **Downloads:** see [Releases](https://github.com/Xrenes/voicewriter/releases) —
  `.exe` (NSIS installer) and `.msi`.
- **Headless**: starts with no visible window; lives in the system tray.
- **Global hotkey**: `Alt+C` (editable in Settings) records while held and transcribes on release.
  Quick taps shorter than 0.3 seconds of audio are ignored.
- **Local STT**: whisper.cpp via `whisper-rs`. The model is downloaded on first run.
- **Insertion**: types into the focused field, or copies to clipboard, or both.
- **Speak selected text aloud**: select text anywhere, press a hotkey (default `Control+Alt+C`)
  to hear it read back via Groq TTS (English only for now). Press again to stop.
- **Cross-platform**: Windows and Linux (X11). See notes below.

This repository contains both the application source (`src/`, `src-tauri/`) and the
marketing website (`site/`).

---

## Prerequisites

| Tool | Windows | Linux (Debian/Ubuntu) |
|---|---|---|
| Rust (stable) | `winget install Rustlang.Rustup` | `curl https://sh.rustup.rs -sSf \| sh` |
| C++ toolchain | Visual Studio Build Tools 2022 (Desktop C++) | `build-essential` |
| CMake (for whisper.cpp) | bundled with VS Build Tools, or `winget install Kitware.CMake` | `cmake` |
| Node.js 18+ | `winget install OpenJS.NodeJS.LTS` | distro package / nvm |
| WebView | Edge WebView2 (preinstalled on Win10/11) | `libwebkit2gtk-4.1-dev` |
| Input simulation | — (built in) | `libxdo-dev` (X11) |
| Audio | — (WASAPI) | `libasound2-dev` (ALSA) or PipeWire |

Linux one-liner:

```bash
sudo apt install build-essential cmake libwebkit2gtk-4.1-dev libxdo-dev \
  libasound2-dev libssl-dev pkg-config
```

---

## Develop

```bash
npm install
npm run tauri dev
```

The first `cargo` build compiles whisper.cpp and can take several minutes.

No window appears — look for the tray icon. **Left-click the tray icon** to open Settings,
choose a model, and click **Download** (base.en ≈ 148 MB).

## Build installers

```bash
npm run tauri build
```

Output in `src-tauri/target/release/bundle/` — NSIS `.exe` on Windows, `.deb` + AppImage on Linux.

---

## How it works

```
Alt+C ──► cpal records mic ──► resample to 16 kHz mono
                                      │
              (release Alt+C) ▼
                          whisper.cpp transcribes  ──►  enigo types into
                          (background thread)            the focused field
```

- `src-tauri/src/lib.rs` — app setup, tray, hotkey, state machine
- `src-tauri/src/audio.rs` — mic capture + resampling
- `src-tauri/src/transcribe.rs` — whisper-rs wrapper (model cached in memory)
- `src-tauri/src/typer.rs` — keystroke / clipboard insertion
- `src-tauri/src/model.rs` — model file paths + first-run download
- `src-tauri/src/settings.rs` — persisted settings (`settings.json`)
- `src-tauri/src/speak.rs` — "speak selected text aloud": selection capture, Groq TTS, playback
- `src-tauri/src/groq.rs` — Groq API calls (transcription, cleanup, text-to-speech)
- `src-tauri/src/keychain.rs` — OS keychain storage for the two independent Groq API keys
  (dictation and speak-aloud)
- `src/` — the setup/status window (plain TS + Vite)

Models and settings live in the OS app-data dir
(`%APPDATA%\com.voicewriter.app` on Windows, `~/.local/share/com.voicewriter.app` on Linux).

---

## Platform notes

- **Linux / Wayland**: keystroke injection via `enigo` is unreliable under Wayland.
  Run an X11 session, or set **Insert as → Copy to clipboard** in Settings and paste with Ctrl+V.
- **Windows mic privacy**: if recording fails, enable
  *Settings → Privacy & security → Microphone → Let desktop apps access your microphone*.
- **Hotkey conflicts**: if `Alt+C` is taken by another app, pick a different combo in Settings.

---

## Not yet implemented

- macOS support (needs mic + accessibility entitlements and code-signing)
- Streaming / live partial transcription
- Voice punctuation commands ("new line", "comma")
- GPU-accelerated whisper builds
- Speak-aloud in languages other than English

## Inserting into desktop apps and websites

Click inside an editable field, hold Alt+C while speaking, then release all keys.
Keep the cursor in that field until transcription and optional AI cleanup finish.
Cleanup runs before one insertion; it no longer selects and replaces text later.

Settings → Insert text offers:

- **Paste (recommended)**: native Ctrl+V, with delayed clipboard restoration.
- **Paste and keep on clipboard**: useful for slow web editors or manual recovery.
- **Type characters**: Unicode typing for fields that reject clipboard paste.
- **Clipboard only**: copy the transcript and paste it yourself.

For Google Sheets, enter cell editing mode before dictating if the transcript
should stay inside one cell. A page must have an editable field focused.
Windows blocks input into higher-privilege apps: when a target must run as
administrator, VoiceWriter needs the same privilege level. Protected fields may
reject automated input. VoiceWriter cannot verify that every app accepted paste.

### Live insertion verification

`cargo test --lib` covers state and formatting regressions. The opt-in
`tests/insertion-smoke.py` harness runs the actual Windows insertion code against
an owned native TextBox, Chrome input/textarea/contenteditable fields, and the
public Google Search field. It checks English, Bangla, and emoji in both paste
and typing modes, without submitting a search. It requires Python Playwright and
installed Chrome; run it after building the Rust tests. It opens temporary test
windows and refuses to send input unless a test window is foreground.

This verifies insertion separately from microphone/transcription. Signed-in
Gmail, Google Docs, Google Sheets, and every third-party app are not covered by
this automated smoke test.

### Hold-to-talk selection protection (Windows)

The Windows repeat guard suppresses repeated trigger-key downs after a registered
shortcut fires, while allowing key release and unrelated input through. An
unassigned key cancels bare-Alt menu activation; no selection/navigation keys are
sent while listening. Cleanup finishes before a single insertion. If the native
focused window/control changes, the transcript is copied for manual recovery.
The focus check does not track individual DOM fields inside one browser window.

`tests/insertion-100.py` exercises 100 full hold/repeat/release cycles with fixed
transcripts through the production shortcut backend, repeat guard and insertion
helpers. It samples text, selection and focus 12 times during each hold, then
checks existing surrounding text and exactly one insertion. Both release orders,
paste, Unicode typing, English, Bangla and emoji are covered. The matrix includes
20 native TextBox cases, 10 Chrome input cases, 20 textarea cases, 20 rich-text
fixture cases, 20 editable-grid fixture cases and 10 Google Search cases. These
fixtures are not live HubSpot, Aloware or Google Sheets account tests. The harness
requires exclusive foreground interaction and stops if the target loses focus.
Results are written incrementally to `src-tauri/target/insertion-100-results.json`;
a partial file is not evidence of a completed 100-case run.

---

## Website & releases

The `site/` folder is the marketing site, auto-deployed to GitHub Pages via
[`.github/workflows/pages.yml`](.github/workflows/pages.yml) on every push to `main`
that touches `site/`.

### Local site preview

```bash
cd site
python -m http.server 8000
# open http://localhost:8000
```

### Releasing a new version

1. Build the installers (`npm run tauri build`).
2. Draft a GitHub Release here, tag `vX.Y.Z`.
3. Upload `VoiceWriter_X.Y.Z_x64-setup.exe` and `VoiceWriter_X.Y.Z_x64_en-US.msi`.
4. The website reads the latest release from the GitHub API, so the download
   buttons update automatically. The `download.js` fallback URLs use
   `/releases/latest/download/…` and keep working without changes.
