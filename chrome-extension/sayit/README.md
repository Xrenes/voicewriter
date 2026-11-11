# SayIt for Chrome

Record your voicemail once, then play it into a web-dialer call with one
hotkey (**Alt+L** by default; press again to stop). Works on Aloware and
GoHighLevel / LeadConnector, plus any site you add. Nothing appears on screen
while it plays.

## Install

1. Open `chrome://extensions` and turn on **Developer mode** (top right).
2. Click **Load unpacked** and pick this `sayit` folder.
3. Pin SayIt from the puzzle-piece menu so its button stays in the toolbar.
4. Reload any Aloware / GHL tab that was already open.

## Use

- **Record:** click the SayIt button → **Record** → speak → **Stop** →
  **Play** to listen → **Save** (or **Discard** and record again). The first
  time, click **Allow microphone access…**: a tab opens where Chrome asks for
  the microphone. Allow it, then record from the SayIt button again.
- **During a call:** with the dialer tab in front, press **Alt+L** when the
  voicemail beeps. Your microphone is muted while the message plays and comes
  back when it ends. Press Alt+L again to stop early.
- **Test it:** Settings → **Test it** plays your message into a fake call and
  lets you hear exactly what the other side gets.

## Settings

- **Hotkey:** Change / Reset. It needs Ctrl, Alt or Win (or an F-key).
- **Mute my microphone while the message plays:** on by default.
- **From any Chrome tab (optional):** a second key, set in Chrome's own
  shortcuts page, that works whichever tab is in front.
- **Works on:** the built-in dialers, plus **+ Add** for the tab you're on
  (a white-label GHL address, or a CRM that embeds the dialer). Reload that
  tab afterwards.

## How it works

The dialer page gets a microphone stream that SayIt builds from your real
mic. The hotkey is handled inside the page and starts the pre-decoded message
on that stream straight away: about 28 ms from key press to call audio,
which is the same as Chrome's own audio path without SayIt. If the call is in
another frame or tab, the key press is passed on to it. The message plays
even if you're muted in the dialer.

## Good to know

- After installing or updating SayIt, reload dialer tabs once (not during a call).
- If VoiceWriter's desktop SayIt also uses Alt+L, it takes the key first.
  Clear or change one of the two.
- Check your local rules on prerecorded voicemail (in the US, the TCPA)
  before using it.
