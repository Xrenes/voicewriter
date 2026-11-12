# VoiceWriter

Hold **Alt+C** while speaking, then release to type your words into the focused text field.
Wait for transcription to finish before starting another recording.
No window — speech is transcribed by Groq's Whisper using your own free Groq API key.

- **Website:** https://xrenes.github.io/voicewriter/
- **Download (Windows 10/11, 64-bit):** [VoiceWriter 0.5.4](https://github.com/Xrenes/voicewriter/releases/tag/v0.5.4) —
  `.msi` installer (installs for all users, asks for admin rights) or `.exe` installer
  (installs just for your account).
- **Headless**: starts with no visible window; lives in the system tray.
- **Global hotkey**: `Alt+C` (editable in Settings) records while held and transcribes on release.
  Quick taps shorter than 0.3 seconds of audio are ignored.
- **Transcription**: Groq Whisper (`whisper-large-v3-turbo` by default). Local whisper.cpp
  transcription is switched off in this build.
- **Insertion**: types into the focused field, or copies to clipboard, or both.
- **Speak selected text aloud**: select text anywhere, press a hotkey (default `Control+Alt+C`)
  to hear it read back via Groq TTS (English only for now). Press again to stop.
- **Rewrite selection** (via Groq; the result replaces the selection). Select text anywhere and press:
  - `Alt+R` **Refine**: corrects spelling, grammar and sentence structure, keeping your tone.
  - `Alt+P` **Professional**: turns a casual draft into a polished, courteous reply for clients
    and colleagues on WhatsApp, LinkedIn or email, without inventing or dropping details.
  - `Alt+H` **Humanize**: rewrites stiff or AI-sounding text so it reads naturally.

  All three hotkeys can be changed in Settings. On Linux/Wayland, bind
  `voicewriter --refine`, `--professional` or `--humanize` to a desktop shortcut.
- **Wheel** (`Shift+Alt+C`): Live, Record, SayIt (Play / Message), Web and AI. Whatever is
  running reads Stop.
- **Call recording** (wheel → Record): records your mic and the PC's audio output as two
  time-aligned tracks, saves the mixed audio, and transcribes each track separately into a
  timestamped transcript labeled You / Device.
- **Live transcript** (wheel → Live, or `voicewriter --live`): records like Record, and a
  see-through black window shows the conversation as it's spoken, your words in white and the
  other side in yellow. Drag it anywhere and resize it from any edge. Text appears about a
  second after it's said, and each line settles when the speaker pauses. Uses the Groq key;
  when Groq rate-limits the key, the in-progress preview slows down while finished lines keep
  priority.
- **Platforms**: the 0.5.4 release is Windows-only. The source also builds on Linux (X11);
  see the notes below.

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

The first `cargo` build can take several minutes.

No window appears — look for the tray icon. **Left-click the tray icon** to open Settings
and add your Groq API key.

## Build installers

```bash
npm run tauri build
```

Output in `src-tauri/target/release/bundle/` — `.msi` and NSIS `.exe` on Windows, `.deb` + AppImage on Linux.

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

### Releasing a new version (Windows)

1. Bump the version in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`.
2. Create a draft GitHub release for tag `vX.Y.Z`, then push that tag.
3. [`.github/workflows/windows-release.yml`](.github/workflows/windows-release.yml) builds
   `VoiceWriter_X.Y.Z_x64_en-US.msi` and `VoiceWriter_X.Y.Z_x64-setup.exe`, installs and
   launches each one on a clean Windows runner, and attaches both to the release.
4. Publish the release, then update the version and download links in `site/index.html`
   and the release tag in `site/main.js`.

Linux packages are not part of releases for now; run
[`.github/workflows/linux-release.yml`](.github/workflows/linux-release.yml) manually to build them.
