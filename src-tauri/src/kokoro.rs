//! Local text-to-speech via Kokoro-82M (ONNX Runtime inference), the sole TTS
//! engine in this app — no cloud TTS, no API key, fully offline once its
//! model files are downloaded (see `model.rs`'s "kokoro-82m" entry and the
//! Settings → Voice Models UI).
//!
//! # Phonemization
//! Kokoro's ONNX model takes IPA phoneme tokens as input, not raw text. The
//! reference Rust implementation (lucasjinreal/Kokoros) gets these from
//! `espeak-rs`, a crate that compiles espeak-ng from source via cmake at
//! build time (`espeak-rs-sys`) — undocumented and unproven on Windows, and
//! a second, differently-built copy of espeak-ng alongside the one we
//! already bundle as a binary for a different purpose (see `espeak.rs`).
//! Instead, this module phonemizes by invoking that SAME bundled
//! `espeak-ng` binary as a subprocess with `--ipa`, which prints IPA text
//! directly — no new build dependency, reuses a path we've already solved.
//! Punctuation is stripped by eSpeak NG's IPA mode, so sentences are split
//! on terminal punctuation ourselves and the mark is re-appended to the
//! phoneme stream afterward (Kokoro's vocabulary includes literal
//! punctuation characters — see `VOCAB`).
//!
//! # Model I/O
//! Three ONNX inputs: `input_ids` (int64 `[1, seq_len]`, the phoneme
//! sequence wrapped in a leading/trailing pad token 0), `style` (float32
//! `[1, 256]`, a per-voice, per-length embedding looked up from the voices
//! pack), and `speed` (float32 `[1]`). One output, `waveform`: float32
//! 24 kHz mono PCM. Verified directly against the actual bundled model
//! (onnx-community/Kokoro-82M-v1.0-ONNX, fp16 export) via
//! `Session::inputs()`/`outputs()` at load time — this export uses the
//! reference implementation's "Timestamped" I/O names
//! (`ort_koko.rs::v1_0_timestamped`), not its "Standard" ones (`tokens`/
//! `audio`), even though we never request/use timestamps.
//!
//! # Voices pack format
//! `kokoro-voices-v1.0.bin` is an npz archive (zip of `.npy` files), one
//! array per voice, shape `[510, 1, 256]` — the style vector varies by
//! phoneme-sequence length, indexed by `tokens.len()` (pre-padding) and
//! clamped to the table's last row for longer inputs.

use anyhow::{anyhow, Context, Result};
use ndarray::{Array1, Array2};
use ort::session::Session;
use ort::value::Tensor;
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use tauri::{AppHandle, Wry};

pub const SAMPLE_RATE: u32 = 24_000;
pub const DEFAULT_VOICE: &str = "af_heart";

/// A representative, broadly-liked subset of Kokoro's ~54 English voices for
/// the Settings dropdown — not the full list, to keep that dropdown usable.
/// Prefix: `af`/`am` = American female/male, `bf`/`bm` = British female/male.
pub const VOICES: &[(&str, &str)] = &[
    ("af_heart", "Heart (US female)"),
    ("af_bella", "Bella (US female)"),
    ("af_nicole", "Nicole (US female)"),
    ("am_michael", "Michael (US male)"),
    ("am_fenrir", "Fenrir (US male)"),
    ("bf_emma", "Emma (UK female)"),
    ("bm_george", "George (UK male)"),
];

/// Kokoro's fixed phoneme vocabulary (pad + punctuation + letters + IPA
/// symbols), index = ONNX token id. Ported directly from the reference
/// implementation's `vocab.rs` (lucasjinreal/Kokoros, Apache-2.0) — this
/// table is intrinsic to the trained model, not something we can change.
fn vocab() -> &'static HashMap<char, i64> {
    static VOCAB: std::sync::OnceLock<HashMap<char, i64>> = std::sync::OnceLock::new();
    VOCAB.get_or_init(|| {
        let pad = "$";
        let punctuation = ";:,.!?¡¿—…\"«»“” ";
        let letters = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
        let letters_ipa = "ɑɐɒæɓʙβɔɕçɗɖðʤəɘɚɛɜɝɞɟʄɡɠɢʛɦɧħɥʜɨɪʝɭɬɫɮʟɱɯɰŋɳɲɴøɵɸθœɶʘɹɺɾɻʀʁɽʂʃʈʧʉʊʋⱱʌɣɤʍχʎʏʑʐʒʔʡʕʢǀǁǂǃˈˌːˑʼʴʰʱʲʷˠˤ˞↓↑→↗↘'̩'ᵻ";
        [pad, punctuation, letters, letters_ipa]
            .concat()
            .chars()
            .enumerate()
            .map(|(i, c)| (c, i as i64))
            .collect()
    })
}

/// Convert eSpeak NG's raw IPA output into Kokoro's expected phoneme
/// characters. Kokoro was trained on `misaki`'s phoneme set, which differs
/// slightly from raw espeak IPA in a few symbols; these substitutions are
/// ported from the reference implementation's `phonemizer.rs`.
fn normalize_phonemes(ipa: &str) -> String {
    ipa.replace('ʲ', "j").replace('r', "ɹ").replace('x', "k").replace('ɬ', "l")
}

fn tokenize(phonemes: &str) -> Vec<i64> {
    let v = vocab();
    phonemes.chars().filter_map(|c| v.get(&c).copied()).collect()
}

/// Run eSpeak NG as a phonemizer (`--ipa`, no audio output) on one clause of
/// text, reusing the same bundled binary + `--path`/cwd setup as `espeak.rs`.
fn phonemize_clause(app: &AppHandle<Wry>, clause: &str, voice: &str) -> Result<String> {
    let located = crate::espeak::locate(app);
    let mut cmd = Command::new(&located.binary);
    if let Some(data_dir) = &located.data_dir {
        if let Some(parent) = data_dir.parent() {
            cmd.current_dir(parent);
            cmd.arg("--path").arg(".");
        }
    }
    let mut child = cmd
        .arg("-v")
        .arg(voice)
        .arg("-q")
        .arg("--ipa")
        .arg("--stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("run eSpeak NG ({})", located.binary.display()))?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("no stdin pipe to eSpeak NG"))?
        .write_all(clause.as_bytes())
        .context("write text to eSpeak NG stdin")?;
    let output = child.wait_with_output().context("wait for eSpeak NG")?;
    if !output.status.success() {
        return Err(anyhow!("eSpeak NG exited with {}", output.status));
    }
    // Multiple lines (one per internal clause break) get joined with a
    // space — Kokoro's model expects one continuous phoneme stream per
    // sentence, not line breaks.
    let ipa = String::from_utf8_lossy(&output.stdout);
    Ok(normalize_phonemes(ipa.lines().collect::<Vec<_>>().join(" ").trim()))
}

/// Split `text` into clauses on sentence-ending punctuation, phonemize each
/// via eSpeak NG, and rejoin with the original punctuation mark appended as
/// a literal character (eSpeak NG's `--ipa` mode drops it, but Kokoro's
/// vocabulary — and prosody — expects it present in the phoneme stream).
fn phonemize(app: &AppHandle<Wry>, text: &str, voice: &str) -> Result<String> {
    let espeak_voice = if voice.starts_with('b') { "en-gb" } else { "en-us" };
    let mut out = String::new();
    let mut clause_start = 0;
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let is_boundary = matches!(c, '.' | '!' | '?' | ',' | ';' | ':') && {
            // Don't split mid-number/abbreviation on a bare comma/period
            // followed immediately by another digit/letter with no space.
            chars.get(i + 1).is_none_or(|n| n.is_whitespace())
        };
        if is_boundary || i == chars.len() - 1 {
            let end = if is_boundary { i } else { i + 1 };
            let clause: String = chars[clause_start..end].iter().collect();
            let clause = clause.trim();
            if !clause.is_empty() {
                let ph = phonemize_clause(app, clause, espeak_voice)?;
                if !ph.is_empty() {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(&ph);
                    if is_boundary {
                        out.push(c);
                    }
                }
            }
            clause_start = i + 1;
        }
    }
    Ok(out)
}

/// One voice's style-vector table: up to 510 rows of 256 floats (flattened,
/// row-major — row count is `data.len() / 256`), indexed by (pre-padding)
/// phoneme token count and clamped to the last row.
///
/// Stored as a plain flat `Vec<f32>` rather than an `ndarray` type:
/// `ndarray-npy` 0.9 (which reads the voices pack) depends on `ndarray` 0.16
/// internally, while `ort`'s `ndarray` Cargo feature requires 0.17 — two
/// semver-incompatible versions of the same crate, aliased separately in
/// Cargo.toml (`ndarray` / `ndarray016`) so each can be used where it's
/// needed. A flat `Vec<f32>` sidesteps the type mismatch entirely rather
/// than converting between the two array types at this boundary.
struct VoiceTable {
    data: Vec<f32>,
    row_len: usize,
}

impl VoiceTable {
    fn style_for(&self, token_len: usize) -> Vec<f32> {
        let rows = self.data.len() / self.row_len;
        let idx = token_len.min(rows.saturating_sub(1));
        let start = idx * self.row_len;
        self.data[start..start + self.row_len].to_vec()
    }
}

fn load_voices(path: &std::path::Path) -> Result<HashMap<String, VoiceTable>> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut npz = ndarray_npy::NpzReader::new(file).context("read voices npz")?;
    let mut map = HashMap::new();
    for name in npz.names().context("list voices in npz")? {
        // Shape `[rows, 1, 256]` per the voices pack format (see module doc
        // comment) — ndarray016 here is `ndarray` 0.16, matching what
        // ndarray-npy 0.9 actually returns (see VoiceTable's doc comment).
        let arr: ndarray016::Array3<f32> =
            npz.by_name(&name).with_context(|| format!("read voice '{name}'"))?;
        let row_len = arr.shape()[2];
        let (data, _offset): (Vec<f32>, Option<usize>) = arr.into_raw_vec_and_offset();
        // npz entries are saved as "<voice>.npy"; strip that back off.
        let id = name.strip_suffix(".npy").unwrap_or(&name).to_string();
        map.insert(id, VoiceTable { data, row_len });
    }
    Ok(map)
}

/// Owns the loaded ONNX session and voice table so repeated "speak selected
/// text" calls don't reload the ~163 MB model file each time. Lazily
/// initialized on first use (not at app startup), since most users may never
/// touch TTS in a given session.
/// Linux loads ONNX Runtime at run time from the copy bundled with the app:
/// `ort`'s prebuilt static library needs glibc 2.38, newer than the oldest
/// Ubuntu we support. Microsoft's official build runs on much older systems.
#[cfg(target_os = "linux")]
fn init_runtime(app: &AppHandle<Wry>) -> Result<()> {
    use std::sync::OnceLock;
    use tauri::Manager;
    static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        let lib = app
            .path()
            .resource_dir()
            .map_err(|e| e.to_string())?
            .join("libonnxruntime.so");
        let _ = ort::init_from(&lib)
            .map_err(|e| format!("load ONNX Runtime {}: {e}", lib.display()))?
            .commit();
        Ok(())
    })
    .clone()
    .map_err(|e| anyhow!(e))
}

pub struct Kokoro {
    session: Mutex<Session>,
    voices: HashMap<String, VoiceTable>,
}

impl Kokoro {
    fn load(app: &AppHandle<Wry>) -> Result<Self> {
        let (model_path, voices_path) = crate::model::kokoro_paths(app)?;
        if !model_path.is_file() || !voices_path.is_file() {
            return Err(anyhow!(
                "Kokoro voice model not downloaded — add it in Settings → Voice Models"
            ));
        }
        #[cfg(target_os = "linux")]
        init_runtime(app)?;
        let session = Session::builder()
            .context("create ONNX session builder")?
            .commit_from_file(&model_path)
            .with_context(|| format!("load Kokoro model {}", model_path.display()))?;
        eprintln!(
            "kokoro: model inputs = {:?}",
            session.inputs().iter().map(|i| i.name()).collect::<Vec<_>>()
        );
        eprintln!(
            "kokoro: model outputs = {:?}",
            session.outputs().iter().map(|o| o.name()).collect::<Vec<_>>()
        );
        let voices = load_voices(&voices_path)?;
        Ok(Self { session: Mutex::new(session), voices })
    }

    /// Synthesize `text` with `voice` (a `VOICES` id) at `speed` (1.0 =
    /// normal), returning 24 kHz mono f32 PCM samples.
    fn synthesize(&self, tokens: &[i64], voice: &str, speed: f32) -> Result<Vec<f32>> {
        let table = self
            .voices
            .get(voice)
            .or_else(|| self.voices.get(DEFAULT_VOICE))
            .ok_or_else(|| anyhow!("voice '{voice}' not found in voices pack"))?;
        let style = table.style_for(tokens.len());

        let mut padded = Vec::with_capacity(tokens.len() + 2);
        padded.push(0i64);
        padded.extend_from_slice(tokens);
        padded.push(0i64);

        let tokens_arr = Array2::from_shape_vec((1, padded.len()), padded).context("build tokens tensor")?;
        let style_arr = Array2::from_shape_vec((1, 256), style.to_vec()).context("build style tensor")?;
        let speed_arr = Array1::from_vec(vec![speed]);

        let mut session = self.session.lock().map_err(|_| anyhow!("Kokoro session lock poisoned"))?;
        let tokens_value = Tensor::from_array(tokens_arr).context("tokens tensor")?;
        let style_value = Tensor::from_array(style_arr).context("style tensor")?;
        let speed_value = Tensor::from_array(speed_arr).context("speed tensor")?;
        let outputs = session
            .run(ort::inputs![
                "input_ids" => tokens_value,
                "style" => style_value,
                "speed" => speed_value,
            ])
            .map_err(|e| anyhow!("run Kokoro inference: {e}"))?;

        let audio = outputs
            .get("audio")
            .or_else(|| outputs.get("waveform"))
            .or_else(|| outputs.get("waveforms"))
            .ok_or_else(|| anyhow!("Kokoro model produced no recognizable audio output"))?;
        let (_, data) = audio.try_extract_tensor::<f32>().context("extract audio tensor")?;
        Ok(data.to_vec())
    }
}

/// Process-wide lazily-loaded engine, shared across calls so the model stays
/// resident in memory after first use.
static ENGINE: std::sync::OnceLock<Mutex<Option<std::sync::Arc<Kokoro>>>> = std::sync::OnceLock::new();

fn engine(app: &AppHandle<Wry>) -> Result<std::sync::Arc<Kokoro>> {
    let cell = ENGINE.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().map_err(|_| anyhow!("Kokoro engine lock poisoned"))?;
    if let Some(k) = guard.as_ref() {
        return Ok(k.clone());
    }
    let k = std::sync::Arc::new(Kokoro::load(app)?);
    *guard = Some(k.clone());
    Ok(k)
}

/// Drop the loaded model from memory (e.g. after the user deletes it from
/// Settings → Voice Models) so a stale session isn't reused.
pub fn unload() {
    if let Some(cell) = ENGINE.get() {
        if let Ok(mut guard) = cell.lock() {
            *guard = None;
        }
    }
}

/// Synthesize `text` aloud and return 16-bit PCM WAV bytes at 24 kHz mono.
/// `voice` empty = `DEFAULT_VOICE`. English/Latin-script text only — see
/// `needs_unsupported_language` for the check callers should make first.
pub fn speak(app: &AppHandle<Wry>, text: &str, voice: &str, speed: f32) -> Result<Vec<u8>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("nothing to speak"));
    }
    let voice = if voice.is_empty() { DEFAULT_VOICE } else { voice };
    let eng = engine(app)?;

    let phonemes = phonemize(app, trimmed, voice)?;
    if phonemes.trim().is_empty() {
        return Err(anyhow!("couldn't phonemize this text"));
    }
    let tokens = tokenize(&phonemes);
    if tokens.is_empty() {
        return Err(anyhow!("couldn't phonemize this text"));
    }

    // Kokoro's model has a practical input-length ceiling; chunk long text
    // on clause boundaries and concatenate the resulting audio so "speak
    // selected text" works on more than a sentence or two.
    const MAX_TOKENS: usize = 480;
    let mut samples = Vec::new();
    if tokens.len() <= MAX_TOKENS {
        samples = eng.synthesize(&tokens, voice, speed)?;
    } else {
        for chunk_text in split_for_length(trimmed, 400) {
            let ph = phonemize(app, &chunk_text, voice)?;
            let toks = tokenize(&ph);
            if toks.is_empty() {
                continue;
            }
            samples.extend(eng.synthesize(&toks, voice, speed)?);
        }
    }
    if samples.is_empty() {
        return Err(anyhow!("Kokoro produced no audio"));
    }
    crate::wav::encode_wav(&samples, SAMPLE_RATE)
}

/// Split long text into roughly `max_chars`-sized pieces on sentence
/// boundaries, for chunked synthesis of text longer than Kokoro's practical
/// single-pass input length.
fn split_for_length(text: &str, max_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for sentence in text.split_inclusive(['.', '!', '?']) {
        if !current.is_empty() && current.len() + sentence.len() > max_chars {
            out.push(std::mem::take(&mut current));
        }
        current.push_str(sentence);
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(text.to_string());
    }
    out
}

/// True if `s` contains a character Kokoro (English-only) and its eSpeak NG
/// phonemizer can't meaningfully read — e.g. Bangla. Callers should check
/// this and surface a clear "unsupported language" error rather than
/// silently producing garbled or empty audio.
pub fn needs_unsupported_language(s: &str) -> bool {
    s.chars().any(|c| c as u32 > 0x024F && !c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocab_maps_pad_punctuation_and_letters() {
        let v = vocab();
        assert_eq!(v[&'$'], 0); // pad token, must be index 0 (used as padding)
        assert!(v.contains_key(&'H'));
        assert!(v.contains_key(&'ˈ')); // stress mark
        assert!(v.contains_key(&'ɹ')); // IPA letter used post-normalization
    }

    #[test]
    fn tokenize_round_trips_through_vocab() {
        // "heɪ" from the reference implementation's own test fixture.
        let tokens = tokenize("heɪ");
        assert_eq!(tokens.len(), 3);
        assert!(tokens.iter().all(|&t| t >= 0));
    }

    #[test]
    fn tokenize_skips_unknown_characters() {
        // A char with no vocab entry (e.g. a stray emoji) is dropped, not an error.
        let tokens = tokenize("a🎉b");
        assert_eq!(tokens.len(), 2);
    }

    #[test]
    fn normalize_phonemes_applies_kokoro_substitutions() {
        assert_eq!(normalize_phonemes("r"), "ɹ");
        assert_eq!(normalize_phonemes("ʲ"), "j");
        assert_eq!(normalize_phonemes("x"), "k");
        assert_eq!(normalize_phonemes("ɬ"), "l");
        // Non-substituted IPA passes through untouched.
        assert_eq!(normalize_phonemes("həlˈoʊ"), "həlˈoʊ");
    }

    #[test]
    fn voice_table_clamps_to_last_row_for_long_input() {
        let table = VoiceTable { data: vec![1.0, 2.0, 3.0, 4.0], row_len: 2 };
        assert_eq!(table.style_for(0), vec![1.0, 2.0]);
        assert_eq!(table.style_for(1), vec![3.0, 4.0]);
        // token_len beyond the table clamps to the last row rather than panicking.
        assert_eq!(table.style_for(100), vec![3.0, 4.0]);
    }

    #[test]
    fn detects_unsupported_non_latin_script() {
        assert!(needs_unsupported_language("আমি ভালো আছি"));
        assert!(!needs_unsupported_language("hello world"));
        assert!(!needs_unsupported_language(""));
    }
}
