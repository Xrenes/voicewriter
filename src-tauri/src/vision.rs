//! Screenshot/photo Q&A for the wheel's "AI" chat wedge, via Groq's vision
//! model. Same Groq key and `chat/completions` endpoint as `refine.rs`, but
//! with an image attached to the user message (OpenAI-compatible multimodal
//! content blocks: a `text` part plus an `image_url` part carrying a base64
//! data URI — Groq's vision models accept this shape directly, no separate
//! image-upload endpoint needed).

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use std::time::Duration;

const CHAT_ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";
const TIMEOUT: Duration = Duration::from_secs(30);

/// Fallback used only if the user hasn't picked a model yet (empty
/// `vision_model`). Groq has deprecated/renamed vision models out from under
/// a fixed constant here more than once — including a `3.6` vs `3.8` naming
/// slip in an earlier version of this exact constant — so `ask` is now
/// always called with a model the AI chat window's own dropdown fetched live
/// from the account's real `/v1/models` list (see list_vision_models /
/// debug_list_groq_models in lib.rs); this constant is just a starting
/// guess, not guaranteed to exist.
const FALLBACK_MODEL: &str = "qwen/qwen3.8-27b";

const SYSTEM_PROMPT: &str = "You are a helpful assistant answering questions about an image \
(a screenshot or photo) the user has shared. Answer only what is asked, based on what is \
actually visible in the image — do not guess at content that isn't shown. If the image is \
unclear or doesn't contain what's being asked about, say so plainly instead of inventing an \
answer. Keep answers concise and conversational.";

/// One turn in the vision chat: either a plain text message, or a message
/// with one attached image (as raw bytes, already read from disk — this
/// module handles MIME-sniffing and base64 encoding). Captured screenshots
/// are always PNG, but a file picked from disk via "attach a file" can be
/// any common format, so the actual bytes are sniffed rather than assumed.
pub enum Turn<'a> {
    User { text: &'a str, image_bytes: Option<&'a [u8]> },
    Assistant { text: &'a str },
}

/// Guess an image's MIME type from its magic bytes rather than trusting a
/// filename extension (which a user-picked file's path may lack or lie
/// about). Falls back to PNG — the format every in-app capture actually is —
/// if the bytes don't match a known signature.
fn sniff_mime(bytes: &[u8]) -> &'static str {
    match image::guess_format(bytes) {
        Ok(image::ImageFormat::Png) => "image/png",
        Ok(image::ImageFormat::Jpeg) => "image/jpeg",
        Ok(image::ImageFormat::WebP) => "image/webp",
        Ok(image::ImageFormat::Gif) => "image/gif",
        Ok(image::ImageFormat::Bmp) => "image/bmp",
        _ => "image/png",
    }
}

/// Send the whole conversation so far (so follow-up questions have context
/// from earlier turns/images) and get the assistant's next reply. `model`
/// should be a model id the caller actually confirmed exists for this key
/// (empty falls back to `FALLBACK_MODEL`, a best-effort guess).
pub fn ask(turns: &[Turn], api_key: &str, model: &str) -> Result<String> {
    let model = if model.trim().is_empty() { FALLBACK_MODEL } else { model.trim() };
    let client = reqwest::blocking::Client::builder().timeout(TIMEOUT).build()?;

    let mut messages = vec![serde_json::json!({ "role": "system", "content": SYSTEM_PROMPT })];
    for turn in turns {
        messages.push(match turn {
            Turn::Assistant { text } => serde_json::json!({ "role": "assistant", "content": text }),
            Turn::User { text, image_bytes: None } => {
                serde_json::json!({ "role": "user", "content": text })
            }
            Turn::User { text, image_bytes: Some(bytes) } => {
                let mime = sniff_mime(bytes);
                let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                serde_json::json!({
                    "role": "user",
                    "content": [
                        { "type": "text", "text": text },
                        { "type": "image_url", "image_url": { "url": format!("data:{mime};base64,{b64}") } }
                    ]
                })
            }
        });
    }

    let payload = serde_json::json!({
        "model": model,
        "temperature": 0.4,
        "max_tokens": 1024,
        "messages": messages,
    });

    let resp = client
        .post(CHAT_ENDPOINT)
        .bearer_auth(api_key)
        .json(&payload)
        .send()
        .context("send Groq vision request")?;

    let status = resp.status();
    let body = resp.text().context("read Groq vision response")?;
    if !status.is_success() {
        let detail = extract_error(&body).unwrap_or_else(|| body.clone());
        return Err(anyhow!("Groq vision {}: {}", status.as_u16(), truncate(&detail, 200)));
    }

    let parsed: serde_json::Value = serde_json::from_str(&body)?;
    let out = parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| anyhow!("no content in Groq vision response"))?;

    let cleaned = out.trim();
    if cleaned.is_empty() {
        return Err(anyhow!("Groq returned an empty answer"));
    }
    Ok(cleaned.to_string())
}

/// Quick text-only connectivity check for one model id — used to test every
/// model in the account's list at once (see list_vision_models_tested in
/// lib.rs). Only confirms the model id exists and responds to a chat
/// completion for this key; it does NOT confirm actual vision/image support
/// (that would need a real image in the request), so a model can pass this
/// and still not be able to "see" anything — a cheap existence check, not a
/// full vision capability test.
pub fn test_model(api_key: &str, model: &str) -> Result<()> {
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(15)).build()?;
    let payload = serde_json::json!({
        "model": model,
        "max_tokens": 4,
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let resp = client
        .post(CHAT_ENDPOINT)
        .bearer_auth(api_key)
        .json(&payload)
        .send()
        .context("send Groq test request")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        let detail = extract_error(&body).unwrap_or(body);
        return Err(anyhow!("{}: {}", status.as_u16(), truncate(&detail, 120)));
    }
    Ok(())
}

fn extract_error(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("error")?.get("message")?.as_str().map(|s| s.to_string())
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}
