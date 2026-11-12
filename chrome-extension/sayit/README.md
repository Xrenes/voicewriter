# SayIt for Chrome

Record your voicemail once, then play it into a web-dialer call with one
hotkey (**Alt+L** by default; press again to stop). Works with Aloware,
GoHighLevel / LeadConnector and HubSpot, including a dialer running in a
HubSpot calling window, plus any site you add. Nothing appears on screen
while it plays, and it makes no sounds of its own.

## Install

1. Open `chrome://extensions` and turn on **Developer mode** (top right).
2. Click **Load unpacked** and pick this `sayit` folder.
3. Pin SayIt from the puzzle-piece menu so its button stays in the toolbar.
4. Reload your HubSpot / Aloware / GHL tabs, and close and reopen any calling
   window. Do the same after every SayIt update.

## Use

- **Record:** click the SayIt button → **Record** → speak → **Stop** →
  **Play** to listen → **Save** (or **Discard** and record again). The first
  time, click **Allow microphone access…**: a tab opens where Chrome asks for
  the microphone. Allow it, then record from the SayIt button again.
- **During a call:** when the voicemail beeps, press **Alt+L** on any web page
  in Chrome: the HubSpot record, the calling window, another tab. The message
  plays on your connected call wherever it is, and you hear it in your own
  headset at the same moment, so you know exactly when it starts and ends.
  Your microphone is muted while it plays. Press Alt+L again to stop early.
- **Check:** during a call, the SayIt popup says **Live call found** and where
  (e.g. `…aloware.com inside app-na2.hubspot.com`).
- **Test it:** Settings → **Test it** plays your message into a fake call and
  lets you hear exactly what the other side gets.

## Settings

- **Hotkey:** Change / Reset. It needs Ctrl, Alt or Win (or an F-key).
- **Mute my microphone while the message plays:** on by default.
- **Let me hear the message too:** on by default. It plays on the output the
  dialer uses for the call (or Windows' default output).
- **Calls it plays into:** Aloware, GHL and HubSpot, and anything they embed.
  **+ Add** another site from its tab (a white-label GHL, a mic-test site), or
  tick **Every website**.
- **Extra key (optional):** set in Chrome's shortcuts page; it also works on
  Chrome's own pages, or outside Chrome.

## How it works

On dialer pages, and in every frame embedded in them whatever its address,
the page's microphone stream is SayIt's: your real microphone, or the saved
message while it plays. SayIt notes which of those streams is in a connected
WebRTC call.

The hotkey is handled on every web page. If the connected call is in that
same frame, the message starts there at once. Otherwise the extension's
background, which keeps track of every call microphone in every tab and
window, sends it to the connected call (or, with none connected, to the
newest microphone). That takes about 10 ms. Only one call plays, so you hear
one preview.

- Silence before the first word is skipped, so the voice starts right away.
- The message plays even if you're muted in the dialer.
- On other websites the microphone is left alone (unless **Every website** is on).

Normally Windows plays its "Default Beep" when Alt+<letter> reaches a window
that doesn't use it. SayIt claims the whole keystroke, so that never happens,
and the page never sees the key.

## Good to know

- SayIt has access to all sites so the hotkey works everywhere. It only
  changes the microphone on the dialer sites above (and sites you add).
- If VoiceWriter's desktop SayIt also uses Alt+L, it takes the key first.
  Clear or change one of the two.
- Check your local rules on prerecorded voicemail (in the US, the TCPA)
  before using it.
