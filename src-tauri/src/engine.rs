//! Transcription dispatch: choose Groq or local Whisper per the user's `engine`
//! setting, fall back from Groq to local when appropriate, run a cleanup pass on
//! the result, and record usage.

use anyhow::Result;
use std::path::Path;
use tauri::{AppHandle, Wry};

use crate::settings::Settings;
use crate::{format, groq, keychain, model, transcribe, usage};

/// How a raw transcript should be cleaned up before typing. Pure decision
/// logic, split out so it can be exhaustively tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Route {
    /// Language string to hand `format::tidy`.
    tidy_lang: &'static str,
    /// Use the code path in `tidy` (spoken symbols, no casing / period).
    is_code: bool,
    /// Send the pre-tidied text through the Groq LLM polish pass.
    allow_polish: bool,
}

impl Route {
    fn decide(language: &str, text: &str) -> Route {
        let english_mode = language == "en" || language == "auto" || language.is_empty();

        if english_mode {
            // Code check first: a command like "git status dash dash short" has
            // almost no English stopwords and would otherwise look "non-English".
            if format::looks_like_code(text) {
                return Route {
                    tidy_lang: "en",
                    is_code: true,
                    allow_polish: true, // code-safe prompt keeps it verbatim
                };
            }
            if format::looks_non_english(text) {
                // romanized Bangla / code-switched -> never send to the LLM,
                // tidy as a non-Latin language (whitespace only).
                return Route {
                    tidy_lang: "bn",
                    is_code: false,
                    allow_polish: false,
                };
            }
            return Route {
                tidy_lang: "en",
                is_code: false,
                allow_polish: true,
            };
        }

        // Explicit non-English mode (e.g. Bangla via Alt+X): tidy in that
        // language, allow the (language-aware, no-translate) polish pass.
        Route {
            tidy_lang: match language {
                "de" => "de",
                "fr" => "fr",
                "es" => "es",
                "it" => "it",
                "pt" => "pt",
                "nl" => "nl",
                _ => "bn", // treat every other code as non-Latin/whitespace-only
            },
            is_code: false,
            allow_polish: true,
        }
    }
}

pub enum Outcome {
    /// Text ready to type now. If `polish_input` is `Some`, the caller should
    /// run `groq::polish` on `polish_input` in the worker and insert
    /// the final text once if the cleanup is sane. `polish_input` is `None` when no cleanup is wanted (banglish, no
    /// key, polish disabled) — `text` is final.
    Text {
        text: String,
        engine: &'static str,
        polish_input: Option<String>,
    },
    /// Hard failure the user should see (e.g. groq-only mode, offline).
    Failed(String),
}

/// Is a polish result usable? Rejects near-empty / truncated
/// output. Public so the async path in `lib.rs` can reuse it.
pub fn polish_is_sane(input: &str, output: &str) -> bool {
    let out = output.trim();
    if out.is_empty() {
        return false;
    }
    let inl = input.trim().chars().count();
    let outl = out.chars().count();
    if inl <= 12 {
        return outl >= 2 || out.chars().any(|c| c.is_alphanumeric());
    }
    (outl as f32) >= (inl as f32) * 0.5
}

/// `samples` must be 16 kHz mono f32.
pub fn run(
    app: &AppHandle<Wry>,
    engine_lock: &parking_lot::Mutex<transcribe::Engine>,
    cfg: &Settings,
    samples: &[f32],
) -> Outcome {
    let audio_secs = samples.len() as f64 / crate::audio::TARGET_SR as f64;
    let want = cfg.engine.as_str(); // "auto" | "groq" | "local"

    let raw: String;
    let engine: &'static str;

    // --- Transcribe (Groq first unless the user forced local) ---
    if want != "local" {
        if let Some(key) = keychain::get(keychain::Purpose::Dictation) {
            match try_groq(&key, cfg, samples) {
                Ok(text) => {
                    usage::record_ok(app, usage::Purpose::Dictation, audio_secs, 0.0);
                    return finish(app, cfg, text, "groq");
                }
                Err(e) => {
                    let msg = e.to_string();
                    usage::record_err(app, usage::Purpose::Dictation, &msg);
                    if want == "groq" {
                        return Outcome::Failed(format!("Groq failed: {msg}"));
                    }
                    eprintln!("groq failed, falling back to local: {msg}");
                }
            }
        } else if want == "groq" {
            return Outcome::Failed("Groq selected but no API key set".into());
        }
    }

    // --- Local Whisper --- (not a metered API call, no usage tracked)
    match try_local(app, engine_lock, cfg, samples) {
        Ok(text) => {
            raw = text;
            engine = "local";
        }
        Err(e) => {
            let msg = e.to_string();
            return Outcome::Failed(msg);
        }
    }

    finish(app, cfg, raw, engine)
}

/// Prepare the transcript and decide whether cleanup should precede insertion.
///
/// Returns `Outcome::Text` with:
/// - `text`: the locally-tidied transcript, safe to type right now.
/// - `polish_input`: `Some(text_for_llm)` when cleanup is wanted,
///   else `None` (banglish, no key, or polish disabled).
fn finish(app: &AppHandle<Wry>, cfg: &Settings, raw: String, engine: &'static str) -> Outcome {
    let _ = app;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Outcome::Text {
            text: String::new(),
            engine,
            polish_input: None,
        };
    }

    let route = Route::decide(&cfg.language, trimmed);

    // Deterministic pass: for code this also maps spoken symbols
    // ("dash dash force" -> "--force") so a later polish sees real tokens.
    let pre = format::tidy(trimmed, route.tidy_lang, route.is_code);
    let pre = if pre.is_empty() { trimmed.to_string() } else { pre };

    let wants_polish =
        cfg.polish && route.allow_polish && keychain::get(keychain::Purpose::Dictation).is_some();

    Outcome::Text {
        text: pre.clone(),
        engine,
        polish_input: if wants_polish { Some(pre) } else { None },
    }
}

fn try_groq(key: &str, cfg: &Settings, samples: &[f32]) -> Result<String> {
    let wav = groq::encode_wav_16k_mono(samples)?;
    groq::transcribe(wav, key, &cfg.groq_model, &cfg.language)
}

fn try_local(
    app: &AppHandle<Wry>,
    engine_lock: &parking_lot::Mutex<transcribe::Engine>,
    cfg: &Settings,
    samples: &[f32],
) -> Result<String> {
    let path: std::path::PathBuf = model::model_path(app, &cfg.model)?;
    let p: &Path = &path;
    engine_lock.lock().transcribe(p, samples, &cfg.language)
}

// ===========================================================================
// Scenario table: for every kind of transcript, assert the routing decision
// AND the final deterministic (no-network) text. This is the "deep test".
// ===========================================================================
#[cfg(test)]
mod scenarios {
    use super::Route;
    use crate::format::tidy;

    /// Simulate the offline path of `finish()`: decide the route, run tidy.
    fn offline_result(language: &str, transcript: &str) -> (Route, String) {
        let r = Route::decide(language, transcript.trim());
        let out = tidy(transcript.trim(), r.tidy_lang, r.is_code);
        (r, out)
    }

    #[derive(Clone, Copy)]
    enum Kind {
        EnProse,
        Code,
        Banglish,
        Bangla,
    }

    fn check(language: &str, input: &str, kind: Kind, expected_offline: &str) {
        let (r, out) = offline_result(language, input);
        match kind {
            Kind::EnProse => {
                assert!(!r.is_code, "[{input}] should not be code");
                assert!(r.allow_polish, "[{input}] prose should allow polish");
                assert_eq!(r.tidy_lang, "en", "[{input}]");
            }
            Kind::Code => {
                assert!(r.is_code, "[{input}] should be detected as code");
                assert!(r.allow_polish, "[{input}] code still allows (safe) polish");
            }
            Kind::Banglish => {
                assert!(!r.allow_polish, "[{input}] banglish must skip polish");
                assert!(!r.is_code, "[{input}]");
                assert_eq!(r.tidy_lang, "bn", "[{input}]");
            }
            Kind::Bangla => {
                assert_eq!(r.tidy_lang, "bn", "[{input}]");
            }
        }
        assert_eq!(out, expected_offline, "offline output for [{input}]");
    }

    // ---------- English prose ----------
    #[test]
    fn english_prose_cases() {
        check("en", "the meeting is at three pm", Kind::EnProse, "The meeting is at three pm.");
        check("en", "um so i think we should ship it", Kind::EnProse, "I think we should ship it.");
        check("en", "can you review my pull request", Kind::EnProse, "Can you review my pull request.");
        check("en", "wait comma then send it question mark", Kind::EnProse, "Wait, then send it?");
        check("en", "add a dash of salt to the soup", Kind::EnProse, "Add a dash of salt to the soup.");
        check("en", "The build already passed.", Kind::EnProse, "The build already passed.");
        check("en", "okay", Kind::EnProse, "Okay.");
        check(
            "en",
            "first point new paragraph second point",
            Kind::EnProse,
            "First point\n\nSecond point.",
        );
    }

    // ---------- Linux commands / code ----------
    #[test]
    fn command_cases() {
        check("en", "git status dash dash short", Kind::Code, "git status --short");
        check("en", "git checkout dash dash force main", Kind::Code, "git checkout --force main");
        check("en", "ls dash la slash var slash log", Kind::Code, "ls -la /var/log");
        check("en", "cd dot dot slash src", Kind::Code, "cd ../src");
        check("en", "cat slash etc slash hosts", Kind::Code, "cat /etc/hosts");
        check("en", "sudo apt-get install libssl-dev", Kind::Code, "sudo apt-get install libssl-dev");
        check("en", "npm run tauri build", Kind::Code, "npm run tauri build");
        check("en", "cargo test dash dash lib", Kind::Code, "cargo test --lib");
        check("en", "export PATH=$HOME/bin:$PATH", Kind::Code, "export PATH=$HOME/bin:$PATH");
        check(
            "en",
            "docker run dash dash rm dash it ubuntu bash",
            Kind::Code,
            "docker run --rm -it ubuntu bash",
        );
        check(
            "en",
            "grep dash r pattern dot slash src",
            Kind::Code,
            "grep -r pattern ./src",
        );
    }

    // ---------- Banglish on Alt+C ----------
    #[test]
    fn banglish_cases() {
        check("en", "ami kalke office jabo na", Kind::Banglish, "ami kalke office jabo na");
        check(
            "en",
            "tumi ki meeting er kotha bolba boss ke",
            Kind::Banglish,
            "tumi ki meeting er kotha bolba boss ke",
        );
        check(
            "en",
            "amar mone hoy eta merge kora uchit akhon",
            Kind::Banglish,
            "amar mone hoy eta merge kora uchit akhon",
        );
    }

    // ---------- Bangla script on Alt+X ----------
    #[test]
    fn bangla_cases() {
        check("bn", "আমি ভালো আছি তুমি কেমন আছো", Kind::Bangla, "আমি ভালো আছি তুমি কেমন আছো");
        check("bn", "  কালকে   অফিসে যাবো না ।  ", Kind::Bangla, "কালকে অফিসে যাবো না।");
    }

    // ---------- edge cases ----------
    #[test]
    fn polish_sanity_guard() {
        use super::polish_is_sane;
        assert!(polish_is_sane("the meeting is at three", "The meeting is at three."));
        assert!(!polish_is_sane("git checkout main", "")); // empty
        assert!(!polish_is_sane("the plan is ready now", ".")); // truncated
        assert!(!polish_is_sane(
            "we should ship the release tomorrow",
            "OK"
        )); // lost >50%
        assert!(polish_is_sane("okay", "Okay.")); // tiny input, fine
        assert!(!polish_is_sane("okay", ".")); // tiny but junk
    }

    #[test]
    fn edge_cases() {
        let (_, out) = offline_result("en", "");
        assert_eq!(out, "");
        let (_, out) = offline_result("en", "   \n  ");
        assert_eq!(out, "");
        // one short non-english utterance -> treated as english (too short to judge)
        let (r, _) = offline_result("en", "kemon acho");
        assert!(r.allow_polish);
        // a sentence that merely mentions git is still prose
        let (r, out) = offline_result("en", "i pushed the fix to git yesterday");
        assert!(!r.is_code, "mentioning git in prose is not a command");
        assert_eq!(out, "I pushed the fix to git yesterday.");
    }
}
