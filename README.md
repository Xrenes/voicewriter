# VoiceWriter — website & downloads

Landing page and Windows installers for **VoiceWriter**, a background voice-dictation
app: hold `Alt+C`, speak, release, and your words are typed into whatever field has
focus.

- **Website:** https://xrenes.github.io/voicewriter/
- **Downloads:** see [Releases](https://github.com/Xrenes/voicewriter/releases) —
  `.exe` (NSIS installer) and `.msi`.

This repository contains only the marketing site (`site/`) and release binaries.
The application source is maintained privately.

## Local preview

```bash
cd site
python -m http.server 8000
# open http://localhost:8000
```

## Deploy

The site auto-deploys to GitHub Pages via
[`.github/workflows/pages.yml`](.github/workflows/pages.yml) on every push to `main`
that touches `site/`.

## Releasing a new version

1. Build the installers privately (`npm run tauri build`).
2. Draft a GitHub Release here, tag `vX.Y.Z`.
3. Upload `VoiceWriter_X.Y.Z_x64-setup.exe` and `VoiceWriter_X.Y.Z_x64_en-US.msi`.
4. The website reads the latest release from the GitHub API, so the download
   buttons update automatically. The `download.js` fallback URLs use
   `/releases/latest/download/…` and keep working without changes.
