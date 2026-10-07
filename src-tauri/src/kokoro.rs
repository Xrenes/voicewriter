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
    if let Some(ipa) = crate::espeak::ipa(app, clause, voice) {
        return Ok(normalize_phonemes(ipa.trim()));
    }
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

/// ONNX Runtime threads: physical-ish core count. Using every logical core
/// (hyper-threads) slows Kokoro down through contention.
fn default_threads() -> usize {
    let logical = std::thread::available_parallelism().map_or(4, |n| n.get());
    (logical / 2).clamp(1, 8)
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
        Self::load_from(&model_path, &voices_path, default_threads())
    }

    fn load_from(model_path: &std::path::Path, voices_path: &std::path::Path, threads: usize) -> Result<Self> {
        let session = Session::builder()
            .context("create ONNX session builder")?
            .with_intra_threads(threads)
            .map_err(|e| anyhow!("set ONNX threads: {e}"))?
            .commit_from_file(model_path)
            .with_context(|| format!("load Kokoro model {}", model_path.display()))?;
        eprintln!(
            "kokoro: model inputs = {:?}",
            session.inputs().iter().map(|i| i.name()).collect::<Vec<_>>()
        );
        eprintln!(
            "kokoro: model outputs = {:?}",
            session.outputs().iter().map(|o| o.name()).collect::<Vec<_>>()
        );
        let voices = load_voices(voices_path)?;
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

/// Load the model in the background if it's downloaded, so the first
/// speak-aloud doesn't pay the multi-second load. Silent on failure — the
/// real error surfaces on the next speak.
pub fn preload(app: AppHandle<Wry>) {
    std::thread::spawn(move || {
        let Ok((model, voices)) = crate::model::kokoro_paths(&app) else { return };
        if !model.is_file() || !voices.is_file() {
            return;
        }
        let t0 = std::time::Instant::now();
        match engine(&app) {
            Ok(k) => {
                // A short warm-up run primes ONNX Runtime's kernels too.
                let _ = phonemize(&app, "Ready.", DEFAULT_VOICE)
                    .map(|ph| k.synthesize(&tokenize(&ph), DEFAULT_VOICE, 1.0));
                eprintln!("kokoro: preloaded in {:?}", t0.elapsed());
            }
            Err(e) => eprintln!("kokoro: preload failed: {e}"),
        }
    });
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
    let mut samples = Vec::new();
    speak_chunks(app, text, voice, speed, |chunk| {
        samples.extend(chunk);
        Ok(true)
    })?;
    crate::wav::encode_wav(&samples, SAMPLE_RATE)
}

/// Synthesize `text` sentence by sentence, handing each chunk's 24 kHz mono
/// samples to `on_chunk` as soon as it's ready so playback can start after
/// the first sentence instead of after the whole selection. `on_chunk`
/// returns `Ok(false)` to stop early (e.g. playback was cancelled).
pub fn speak_chunks(
    app: &AppHandle<Wry>,
    text: &str,
    voice: &str,
    speed: f32,
    mut on_chunk: impl FnMut(Vec<f32>) -> Result<bool>,
) -> Result<()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("nothing to speak"));
    }
    let voice = if voice.is_empty() { DEFAULT_VOICE } else { voice };
    let eng = engine(app)?;

    // Short chunks keep time-to-first-audio low; each also stays well under
    // Kokoro's ~510-token input ceiling.
    let mut produced = false;
    for chunk_text in split_for_length(trimmed, 40, 200) {
        let t0 = std::time::Instant::now();
        let ph = phonemize(app, &chunk_text, voice)?;
        let t_ph = t0.elapsed();
        let toks = tokenize(&ph);
        if toks.is_empty() {
            continue;
        }
        let samples = eng.synthesize(&toks, voice, speed)?;
        eprintln!(
            "kokoro: chunk {} chars / {} tokens: phonemize {t_ph:?}, synth {:?}, audio {:.1}s",
            chunk_text.len(),
            toks.len(),
            t0.elapsed() - t_ph,
            samples.len() as f64 / SAMPLE_RATE as f64
        );
        if samples.is_empty() {
            continue;
        }
        produced = true;
        if !on_chunk(samples)? {
            return Ok(());
        }
    }
    if !produced {
        return Err(anyhow!("couldn't phonemize this text"));
    }
    Ok(())
}

/// Split text into chunks for streamed synthesis: the first chunk is cut at
/// the first clause boundary (comma, semicolon, sentence end) once it reaches
/// `first_max` chars, so audio starts quickly; later chunks fill up to
/// `max_chars` on sentence boundaries.
fn split_for_length(text: &str, first_max: usize, max_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for piece in text.split_inclusive(['.', '!', '?', ',', ';', ':']) {
        let limit = if out.is_empty() { first_max } else { max_chars };
        if !current.trim().is_empty()
            && (current.len() >= limit
                || current.len() + piece.len() > max_chars
                || (out.is_empty() && current.trim().len() >= 10))
        {
            out.push(std::mem::take(&mut current));
        }
        current.push_str(piece);
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(text.to_string());
    }
    // A long first sentence with no comma would still delay the first audio;
    // cut it after a few words (a tiny prosody seam, much faster start).
    if out[0].len() > first_max + 15 {
        // Byte offset, so step forward to a char boundary before slicing —
        // accented or other multi-byte text would otherwise panic here.
        let start = (first_max..=out[0].len())
            .find(|&i| out[0].is_char_boundary(i))
            .unwrap_or(out[0].len());
        if let Some(cut) = out[0][start..].find(' ').map(|i| i + start) {
            let rest = out[0].split_off(cut);
            out.insert(1, rest);
        }
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

    /// Times synthesis for each downloaded Kokoro model and thread count.
    /// `cargo test --release bench_kokoro -- --ignored --nocapture`
    #[test]
    #[ignore = "needs downloaded Kokoro models"]
    fn bench_kokoro() {
        let dir = std::path::PathBuf::from(std::env::var("APPDATA").unwrap())
            .join("com.voicewriter.app")
            .join("models");
        let voices = dir.join("kokoro-voices-v1.0.bin");
        // "The quick brown fox jumps over the lazy dog, and then it runs far away."
        let ph = "ðə kwˈɪk bɹˈaʊn fˈɑːks dʒˈʌmps ˌoʊvɚ ðə lˈeɪzi dˈɑːɡ, ænd ðˈɛn ɪt ɹˈʌnz fˈɑːɹ ɐwˈeɪ.";
        let toks = tokenize(ph);
        let logical = std::thread::available_parallelism().map_or(4, |n| n.get());
        for file in ["kokoro-82m-fp16.onnx", "kokoro-82m-fp32.onnx", "kokoro-82m-q8.onnx"] {
            let model = dir.join(file);
            if !model.is_file() {
                continue;
            }
            for threads in [logical / 2, logical] {
                let t = std::time::Instant::now();
                let k = Kokoro::load_from(&model, &voices, threads).unwrap();
                let load = t.elapsed();
                k.synthesize(&toks, DEFAULT_VOICE, 1.0).unwrap(); // warm-up
                let t = std::time::Instant::now();
                let n = k.synthesize(&toks, DEFAULT_VOICE, 1.0).unwrap().len();
                let secs = n as f64 / SAMPLE_RATE as f64;
                println!(
                    "{file} threads={threads}: load {load:?}, {:?} for {secs:.1}s audio (x{:.2} real time)",
                    t.elapsed(),
                    secs / t.elapsed().as_secs_f64()
                );
            }
        }
    }

    #[test]
    fn split_starts_with_a_short_clause() {
        let t = "Hello there, this is a fairly long first sentence that keeps going. And a second one. And a third.";
        let parts = split_for_length(t, 40, 200);
        assert_eq!(parts.concat(), t);
        assert!(parts[0].len() <= 50, "first chunk too long: {:?}", parts[0]);
        let long = "This sentence has no commas at all and keeps going for quite a while longer.";
        let parts = split_for_length(long, 40, 200);
        assert_eq!(parts.concat(), long);
        assert!(parts[0].len() <= 50 && parts.len() == 2, "{parts:?}");
        assert!(parts.len() >= 2);
        assert_eq!(split_for_length("Hi.", 40, 200), vec!["Hi.".to_string()]);
        // Multi-byte chars straddling the cut point must not panic.
        for pad in 0..4 {
            let t = format!("{}{}", "a".repeat(pad), "é".repeat(60));
            assert_eq!(split_for_length(&t, 40, 200).concat(), t);
            let t = format!("{}{} word word word word word", "a".repeat(pad), "naïve café ".repeat(5));
            assert_eq!(split_for_length(&t, 40, 200).concat(), t);
        }
    }

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
