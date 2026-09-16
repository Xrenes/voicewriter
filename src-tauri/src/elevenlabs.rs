//! Natural-voice text-to-speech via ElevenLabs, for languages Groq's
//! Orpheus TTS doesn't cover (Bangla, Spanish, Italian, ...). Used as the
//! quality alternative to the offline-but-robotic eSpeak NG engine — see
//! `espeak.rs`. Free tier: ~10,000 credits/month, no credit card required.

use anyhow::{anyhow, Context, Result};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20);

/// Multilingual v2 covers Spanish/Italian/English and 26 others, but not
/// Bengali. Eleven v3 adds Bengali (and 70+ languages total) but is a newer,
/// more expensive model per ElevenLabs' own docs — used only when needed.
const MODEL_MULTILINGUAL_V2: &str = "eleven_multilingual_v2";
const MODEL_V3: &str = "eleven_v3";

/// WAV output requires a Pro-tier-and-above ElevenLabs plan — free accounts
/// requesting a `wav_*` format silently get MP3 back instead (confirmed by
/// inspecting the actual response bytes: an MP3 file with an ID3 header).
/// Request MP3 explicitly so this stays correct if the ignored-parameter
/// behavior ever changes, and decode it via rodio's `symphonia-mp3` feature.
const OUTPUT_FORMAT: &str = "mp3_44100_128";

fn model_for(voice_language_needs_v3: bool) -> &'static str {
    if voice_language_needs_v3 {
        MODEL_V3
    } else {
        MODEL_MULTILINGUAL_V2
    }
}

/// Synthesize `text` as speech using ElevenLabs. `voice_id` selects the
/// voice (configurable in Settings — see `settings.rs`). `needs_v3` should
/// be true only for languages Multilingual v2 doesn't cover (e.g. Bengali).
pub fn speak(text: &str, api_key: &str, voice_id: &str, needs_v3: bool) -> Result<Vec<u8>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("nothing to speak"));
    }
    if voice_id.trim().is_empty() {
        return Err(anyhow!("no ElevenLabs voice selected — set one in Settings"));
    }

    let client = reqwest::blocking::Client::builder().timeout(TIMEOUT).build()?;

    let payload = serde_json::json!({
        "text": trimmed,
        "model_id": model_for(needs_v3),
        "output_format": OUTPUT_FORMAT,
    });

    let url = format!("https://api.elevenlabs.io/v1/text-to-speech/{voice_id}");
    eprintln!("elevenlabs: POST {url} model={} chars={}", model_for(needs_v3), trimmed.len());
    let t0 = std::time::Instant::now();
    let resp = client
        .post(&url)
        .header("xi-api-key", api_key)
        .json(&payload)
        .send()
        .context("send ElevenLabs request")?;
    eprintln!("elevenlabs: response after {:?}, status={}", t0.elapsed(), resp.status());

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        let detail = extract_error(&body).unwrap_or(body);
        return Err(anyhow!("ElevenLabs {}: {}", status.as_u16(), truncate(&detail, 200)));
    }

    let bytes = resp.bytes().context("read ElevenLabs audio")?.to_vec();
    eprintln!("elevenlabs: got {} bytes of audio", bytes.len());
    if bytes.is_empty() {
        return Err(anyhow!("ElevenLabs returned no audio"));
    }
    Ok(bytes)
}

/// List voices available to this account, with the fields needed to tell
/// which ones a free-tier account can actually drive via the API (shared
/// Voice Library voices are frequently gated to paid plans — see `speak`'s
/// 402 handling). Returns (name, voice_id, category) tuples.
pub fn list_voices(api_key: &str) -> Result<Vec<(String, String, String)>> {
    let client = reqwest::blocking::Client::builder().timeout(TIMEOUT).build()?;
    let resp = client
        .get("https://api.elevenlabs.io/v1/voices")
        .header("xi-api-key", api_key)
        .send()
        .context("send ElevenLabs voices request")?;

    let status = resp.status();
    let body = resp.text().context("read ElevenLabs voices response")?;
    if !status.is_success() {
        let detail = extract_error(&body).unwrap_or_else(|| body.clone());
        return Err(anyhow!("ElevenLabs {}: {}", status.as_u16(), truncate(&detail, 200)));
    }

    let parsed: serde_json::Value = serde_json::from_str(&body)?;
    let voices = parsed
        .get("voices")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("no 'voices' array in ElevenLabs response"))?;

    Ok(voices
        .iter()
        .map(|v| {
            let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("?").to_string();
            let id = v.get("voice_id").and_then(|n| n.as_str()).unwrap_or("?").to_string();
            let category = v.get("category").and_then(|n| n.as_str()).unwrap_or("?").to_string();
            (name, id, category)
        })
        .collect())
}

fn extract_error(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    // ElevenLabs errors are typically {"detail": {"message": "..."}} or {"detail": "..."}.
    v.get("detail")
        .and_then(|d| d.get("message").and_then(|m| m.as_str()).map(str::to_string).or_else(|| d.as_str().map(str::to_string)))
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}
