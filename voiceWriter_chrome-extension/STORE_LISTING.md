# Chrome Web Store listing: VoiceWriter

Copy these into the [Chrome Web Store Developer Dashboard](https://chrome.google.com/webstore/devconsole) when you upload `dist/voicewriter-chrome-<version>.zip`.

---

## Store listing tab

**Name** (from the manifest): VoiceWriter

**Summary** (from the manifest, 125/132 characters):
> Voicemail drop for web dialers (Alt+L), dictation, a copy board, read aloud and one-key rewrites on any site. Uses Groq.

**Category:** Productivity → Tools (or "Communication")

**Language:** English

**Description:**

```
Your voice tools for the browser: drop your voicemail into calls, dictate, hear text read aloud and polish what you write. Everything works from the keyboard on any website.

📞 SAYIT: YOUR VOICEMAIL ON ONE KEY (ALT+L)
Record your voicemail once in VoiceWriter. During a call on Aloware, GoHighLevel / LeadConnector or HubSpot, including a dialer inside a HubSpot calling window, press Alt+L when the voicemail beeps. Your message plays straight into the call, and you hear it in your headset at the same moment. Press Alt+L again to stop early.
• Works from any tab: the message finds the connected call wherever it is.
• Your microphone is muted while the message plays (you can turn that off).
• Silence before your first word is trimmed, so it starts instantly.
• The recording is cleaned up for you: rumble and boom removed, background noise turned down between your words, voice a little clearer.
• Headset with its own noise cancelling? One switch turns off Chrome's extra noise removal on your calls, so your voice doesn't sound metallic.
• Add any other web dialer from its tab, and use "Test it" to hear exactly what the other side gets.

🎙 DICTATE: HOLD ALT+C
Click into any text field, hold Alt+C and speak. Let go, and your words are typed right where the cursor is. Groq's Whisper detects the language automatically and takes about a second. Turn on "Clean up with AI" to fix punctuation and remove filler words.

📋 BOARD: ALT+A
Press Alt+A on any tab for a see-through black board. While it's open, what you dictate goes on the board instead of the page, and the whole board is copied, so you can paste it into any app (email, chat, your CRM, a desktop program). Drag it, resize it, set how transparent it is, make the text bigger. It follows you from tab to tab.

🔊 READ ALOUD: CTRL+ALT+C
Select text on any page and press Ctrl+Alt+C to hear it in a natural voice, or paste text into the VoiceWriter window. In Google Sheets and Excel for the web, just click a cell or select a range, with no double-clicking: ranges are read cell by cell. It also works with text selected in Google Docs, and reads a whole text box when you've clicked into one. There are six English voices.

✍️ REWRITE SELECTED TEXT
• Alt+R Refine: fixes spelling, grammar and sentence structure while keeping your meaning and tone.
• Alt+P Professional: turns a casual draft into a polished, courteous reply.
• Alt+H Humanize: rewrites stiff or AI-sounding text so it reads naturally.
The result replaces your selection, and Ctrl+Z undoes it. On a spreadsheet cell, the result is copied so Ctrl+V puts it in place.

⚙️ MAKE IT YOURS
Change every shortcut, pick your microphone, language and Whisper model, choose how text is inserted, and watch your daily Groq usage.

🔑 YOUR KEY, YOUR DATA
Dictation, Read aloud and the rewrites use your own free Groq API key (console.groq.com/keys, no card required). Requests go straight from your browser to Groq. SayIt needs no key and sends nothing anywhere. There are no VoiceWriter servers, no analytics and no tracking. Your key, settings and voicemail stay on your device.

Notes: Read aloud is English-only. Check your local rules on prerecorded voicemail (in the US, the TCPA) before using SayIt.
```

**Graphic assets** (all in `store/`):

| Asset | File | Size |
|---|---|---|
| Store icon | `icons/icon128.png` | 128×128 |
| Screenshot 1: SayIt | `store/screenshot-1.png` | 1280×800 |
| Screenshot 2: Board | `store/screenshot-4.png` | 1280×800 |
| Screenshot 3: Dictation | `store/screenshot-2.png` | 1280×800 |
| Screenshot 4: Rewrite modes | `store/screenshot-3.png` | 1280×800 |
| Small promo tile | `store/promo-small.png` | 440×280 |

To re-render them after changing the popup or the designs in `store/src/`, run `node store/build-assets.mjs`.

**Official URL / Homepage:** https://xrenes.github.io/voicewriter/
**Support URL:** https://github.com/Xrenes/voicewriter/issues

---

## Privacy practices tab

**Single purpose:**
> VoiceWriter is a voice assistant for working in the browser. It turns your voice into text (dictation into the focused field or onto a copyable board), turns text into voice (reading selected text aloud, and playing the user's own recorded voicemail into web-dialer calls), and rewrites selected text, using the user's own Groq API key for the speech-to-text, text-to-speech and rewriting.

**Permission justifications:**

| Permission | Justification |
|---|---|
| `storage` | Saves the user's Groq API key, settings (shortcuts, voice, language, microphone, insertion mode, the board's look, SayIt options and added dialer sites) and daily usage counters locally in the browser. |
| `unlimitedStorage` | The user's recorded voicemail (SayIt) is stored locally as audio, and it can exceed the default storage quota. |
| `contextMenus` | Adds "Read aloud", "Refine", "Professional" and "Humanize" items to the right-click menu for selected text. |
| `offscreen` | An offscreen document records microphone audio for dictation, plays the text-to-speech audio for Read aloud, and writes results to the clipboard. The service worker has no DOM, so it can't do these itself. |
| `scripting` | When VoiceWriter is installed or updated, it injects its content scripts into tabs that are already open, so the user's shortcuts work without reloading every tab. It also uses this to show the board on the tab the user switches to. |
| Host permission `https://api.groq.com/*` | Sends the user's dictation audio and selected text to the Groq API (speech-to-text, text-to-speech and chat completions), authenticated with the user's own API key. |
| Host permissions `http://*/*`, `https://*/*` and content scripts on all URLs | Listens for the user's VoiceWriter keyboard shortcuts on whichever website they are using. Reads the selected text or focused field only when a shortcut or menu item is used. Inserts dictated text, replaces the selection with rewritten text, and shows the board and small status notes. On web-dialer pages (Aloware, GoHighLevel, HubSpot and sites the user adds), a script in the page passes the microphone stream through VoiceWriter. This lets the user's recorded voicemail play into the call when they press the SayIt key. It does not read or send page content otherwise. |

**Remote code:** No, I am not using remote code. (All scripts are packaged in the extension. The extension only calls Groq's REST API.)

**Data usage:** tick these, because they leave the device (sent to Groq when the user uses a feature):
- **Website content**: the text the user selects for Read aloud or a rewrite (including selected spreadsheet cells, or the text box they clicked into).
- **Personal communications**: dictated speech, and selected text, can include messages the user writes or says.
- **Authentication information**: the user's own Groq API key is sent to Groq as the request credential. It is stored only on the user's device.

(SayIt's voicemail recording never leaves the device except as audio played into the user's own call through their dialer.)

Then certify all three:
- I do not sell or transfer user data to third parties, apart from the approved use cases.
- I do not use or transfer user data for purposes that are unrelated to my item's single purpose.
- I do not use or transfer user data to determine creditworthiness or for lending purposes.

**Privacy policy URL:** https://xrenes.github.io/voicewriter/privacy.html
(The page is `site/privacy.html`. It goes live when the site is deployed to GitHub Pages; a push to `main` that touches `site/` deploys it.)

---

## Distribution tab

- Visibility: Public (or Unlisted while you test with a few people).
- Regions: All regions.
- Pricing: Free.

## Review notes

- Because the content scripts run on all sites, one of them in the page's own context, expect an in-depth review. That usually takes a few days longer than normal.
- VoiceWriter bundles several voice tools. If a reviewer questions the single purpose, explain that every feature is part of one voice assistant: voice to text, text to voice, and rewriting what you say or write.
- The reviewer needs a Groq API key to try dictation, Read aloud and the rewrites. You can paste a test key into **Test instructions** ("Account and credentials") on the Privacy tab, then revoke it after approval. SayIt works without a key: record a voicemail and use Settings → SayIt → **Test it**.
