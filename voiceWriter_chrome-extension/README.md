# VoiceWriter for Chrome

The browser version of VoiceWriter, with **SayIt** built in. It needs Chrome 119 or later. The speech and writing features use only the Groq API, with the user's own key; SayIt needs no key and sends nothing anywhere.

| Feature | Default shortcut |
|---|---|
| **SayIt**: play your recorded voicemail into a web-dialer call (Aloware, GoHighLevel, HubSpot, sites you add). Press again to stop. The recording (raw mic, no Chrome noise removal) is cleaned up after you stop it: rumble and boom out, the room turned down between words only (never inside your voice, which is what sounds metallic), a little clarity, 48 kHz. Settings → SayIt → **My headset already cancels noise** also turns Chrome's noise removal off on dialer calls. | `Alt+L` |
| **Dictate**: hold, speak, release, and the text is typed into the focused field | Hold `Alt+C` |
| **Board**: a see-through black board (60% transparent by default). While it's open, what you dictate goes on the board and to the clipboard instead of the page, ready to paste into any app. Press again to close. | `Alt+A` |
| **Read aloud**: read what's selected aloud (press again to stop) | `Control+Alt+C` |
| **Refine**: fix spelling, grammar and structure of the selection | `Alt+R` |
| **Professional**: turn a draft into a polished reply | `Alt+P` |
| **Humanize**: make stiff or AI-sounding text read naturally | `Alt+H` |

**What counts as "selected"** for Read aloud and the rewrites, in this order:
1. Selected text, as usual.
2. Clicked into a text field without selecting anything: the whole field.
3. In apps that draw their own selection (Google Sheets, Google Docs, Excel for the web), the selected cell, range or words. A single click on a cell is enough. VoiceWriter gets them through a pretend copy event, so the clipboard is untouched.
4. For Read aloud only: clicked into a rich text box without selecting anything: the whole box.

Ranges are read cell by cell with pauses. A rewritten cell can't be typed back into the grid, so it's put on the clipboard and Ctrl+V puts it in place.

Every shortcut can be changed in the popup. `Alt+A` and `Alt+L` are also Chrome shortcuts (`chrome://extensions/shortcuts`), so they work on every tab, including Chrome's own pages. Read aloud and the rewrites are also on the right-click menu.

The toolbar popup looks like the desktop app's settings window: the **SayIt** card first (record, play, save your voicemail, see where the call is), then **Read aloud** and **Board**, then the settings sections. The rewrite and clean-up prompts come from the desktop app, and SayIt is the SayIt for Chrome 0.3.0 extension merged in.

VoiceWriter adds itself to tabs that are already open when it's installed or updated, so nothing needs a reload. The one exception is SayIt's call microphone in a dialer tab that was already open at that moment: reload that tab once.

## Try it locally

1. Open `chrome://extensions` and turn on **Developer mode**.
2. Click **Load unpacked** and pick this `chrome-extension` folder.
3. The welcome page opens. Paste a Groq API key ([console.groq.com/keys](https://console.groq.com/keys)) and click **Allow microphone**.
4. For SayIt, click the VoiceWriter icon. In the SayIt card, click **Record**, say your voicemail, click **Stop**, then **Save**. Settings → SayIt → **Test it** plays it into a pretend call so you can hear what the other side gets.

After editing the code, click the reload icon on the extension's card in `chrome://extensions`. Open tabs pick up the new version by themselves (dialer tabs need one reload for SayIt's call microphone).

If the separate **SayIt** extension is also installed, remove it: both would answer `Alt+L`.

## Publish to the Chrome Web Store

1. Bump `"version"` in `manifest.json` for every new upload.
2. Build the zip:
   ```powershell
   powershell -ExecutionPolicy Bypass -File chrome-extension\pack.ps1
   ```
   This writes `chrome-extension/dist/voicewriter-chrome-<version>.zip` (the `dist/` folder is git-ignored).
3. Deploy the privacy policy: push `site/privacy.html` to `main` so GitHub Pages serves it at https://xrenes.github.io/voicewriter/privacy.html.
4. In the [Developer Dashboard](https://chrome.google.com/webstore/devconsole) (one-time $5 registration), create a new item, upload the zip, and fill in the listing using [STORE_LISTING.md](STORE_LISTING.md). It has the description, permission justifications, data-use answers and the image list.

Store images are in `store/`. To re-render them after changing the popup or the designs in `store/src/`, run `node chrome-extension/store/build-assets.mjs`.

## How it works

| File | Role |
|---|---|
| `manifest.json` | Manifest V3. Permissions: `storage`, `unlimitedStorage`, `contextMenus`, `offscreen`, `scripting`. Host access to `api.groq.com` and to http/https pages. Two Chrome shortcuts. |
| `background.js` | Service worker. Makes the Groq requests for dictation, rewrites (with model fallback) and Read aloud, and tracks usage. Holds SayIt's call finder, which knows every call microphone in every tab, and the board, which moves to whichever tab you're on and collects what you dictate while it's open. Adds VoiceWriter to open tabs on install or update. |
| `offscreen.html/js` | Hidden page. Records the microphone for dictation, plays Read aloud audio and writes to the clipboard. |
| `content.js` | Runs on web pages. Handles the shortcuts, inserts or replaces text (undoable with Ctrl+Z), and draws the toasts and the board. |
| `sayit-inject.js` | SayIt's engine inside the page (MAIN world). On dialer pages it passes the microphone through itself, so it can play your message into the connected WebRTC call. |
| `sayit-relay.js` | SayIt's link between the page engine and the extension: settings, the saved message, and call status. |
| `popup.html/css/js` | Toolbar window: SayIt (with the recording clean-up), Read aloud, Board, then settings. Also used as the Options page. |
| `welcome.html/css/js` | First-run setup: API key, one-time microphone permission, practice box, shortcuts. |
| `sayit-test.html/js` | The "Test it" page: a pretend call that records what the other side would hear (your voice and your SayIt message). |
| `live.html/js` | The board in its own small window, for Chrome's own pages where it can't be drawn. |
| `common.js` | Defaults and helpers shared by all of the above. |
| `icons/` | Logo PNGs (16–128) and their SVG sources. |

Microphone access is granted once to the extension itself, on the welcome page, so dictation works on every site without asking each one. Password fields are never read. See [PRIVACY.md](PRIVACY.md).

Check your local rules on prerecorded voicemail (in the US, the TCPA) before using SayIt.
