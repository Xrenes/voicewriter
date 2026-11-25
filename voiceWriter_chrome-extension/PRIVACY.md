# VoiceWriter for Chrome: Privacy Policy

_Effective November 18, 2026_

VoiceWriter is a Chrome extension with these features:
- play a voicemail you recorded into calls on web dialers (SayIt);
- dictate into text fields, or onto a see-through board you can copy from (Board);
- hear selected text read aloud;
- rewrite selected text (Refine, Professional, Humanize).

The speech and writing features use **your own Groq API key**. VoiceWriter has no servers of its own, and its developers never receive your data.

## What VoiceWriter sends, and where

VoiceWriter sends data only when you use a feature. Nothing is sent in the background. Requests go directly from your browser to [Groq](https://groq.com) (`api.groq.com`), authenticated with your API key:

| When youâ€¦ | What is sent to Groq |
|---|---|
| Hold the dictation shortcut (default Alt+C), with or without the board open | The microphone audio recorded while you hold the key. If "Clean up with AI" is on, the transcribed text is sent too. |
| Use Read aloud (Control+Alt+C, the right-click menu, or the Read aloud box) | What you selected: selected text, the cell(s) you selected in a spreadsheet, or the text box you clicked into. Or the text you typed in the Read aloud box. |
| Use Refine, Professional or Humanize (Alt+R / Alt+P / Alt+H or the right-click menu) | What you selected: selected text, the cell(s) you selected in a spreadsheet, or the text field you clicked into. |
| Click Test next to an API key | A request that lists Groq models, to check that the key works. |

Groq processes this data under its own terms and [privacy policy](https://groq.com/privacy-policy/). VoiceWriter doesn't send your data to anyone else.

**SayIt sends nothing to Groq or anywhere else.**
- Your recorded voicemail is stored only in your browser.
- On dialer sites (Aloware, GoHighLevel / LeadConnector, HubSpot, and sites you add), VoiceWriter passes the page's microphone through itself. That way it can play your saved message into the call when you press the SayIt key. Your voicemail recording is cleaned up (noise reduction, clearer tone) on your computer.
- Your microphone audio still goes only where the dialer sends it. VoiceWriter doesn't record it or send it anywhere.

## What VoiceWriter stores on your device

Using Chrome's extension storage, only on your device:

- your Groq API key(s);
- your settings (shortcuts, voice, language, microphone choice, insertion mode, the board's look, SayIt options and the dialer sites you added);
- your SayIt voicemail recording;
- where you last put the board, and its text size;
- simple daily usage counters (number of requests and seconds of audio), used for the usage meters;
- the last text you typed in the Read aloud box, and the board's text. These are kept in session storage and cleared when Chrome closes.

None of this is synced to your Google account or sent to the developers. Uninstalling the extension deletes it.

## What VoiceWriter does not do

- No analytics, tracking, advertising or fingerprinting.
- No selling or sharing of your data, and no use of it for any purpose other than the feature you used.
- No reading of pages in the background. On web pages, VoiceWriter only acts when you press one of its shortcuts or choose one of its right-click items. Then it reads only what you selected (text, spreadsheet cells) or the box you are typing in. Password fields are ignored.
- No listening of its own except while you hold the dictation key, or record your voicemail. On a dialer's call it only passes along the microphone the dialer itself opened.

## Permissions and why they are needed

- **Microphone** (asked once, on a VoiceWriter page): to record dictation, and your SayIt voicemail.
- **storage** and **unlimitedStorage**: to keep your API key, settings, usage counters, and voicemail recording on your device. The recording can be larger than Chrome's default storage allowance.
- **contextMenus**: to add VoiceWriter's items to the right-click menu for selected text.
- **offscreen**: a hidden extension page that records the microphone for dictation, plays Read aloud audio and copies results to the clipboard. The background worker can't do these itself.
- **scripting**: to add VoiceWriter to tabs that were already open when it was installed or updated, so they work without a reload.
- **Access to api.groq.com**: to send your requests to Groq.
- **Access to all websites**: so your shortcuts and the board work on whichever site you're on, and the result can be inserted there. It also lets SayIt work on calls in web dialers.
- **Keyboard shortcuts**: Alt+A and Alt+L are also registered as Chrome shortcuts, so they work on every tab. You can change them at `chrome://extensions/shortcuts`.

## Children

VoiceWriter is not directed at children under 13 and does not knowingly collect their information.

## Changes

If this policy changes, the new version will be posted here with a new effective date.

## Contact

Questions or requests: open an issue at [github.com/Xrenes/voicewriter/issues](https://github.com/Xrenes/voicewriter/issues).
