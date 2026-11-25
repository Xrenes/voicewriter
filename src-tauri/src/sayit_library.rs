//! SayIt's messages: as many as you like (one per project, say), recorded or
//! uploaded, and which one the hotkey plays.
//!
//! On disk, in the app data folder's `sayit/`:
//! - `library.json`: the list (id, name, length) and the selected message
//! - `messages/<id>.wav`: each message, mono 16-bit PCM at its own rate
//!
//! Before the list, SayIt kept a single `sayit/message.wav`. It becomes
//! "My message", selected, the first time the list is opened.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Longest message name kept (longer names are cut).
pub const MAX_NAME_CHARS: usize = 60;
/// The single message from before the list.
const LEGACY_ID: &str = "legacy";
const LEGACY_NAME: &str = "My message";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Entry {
    pub id: String,
    pub name: String,
    /// Length in seconds.
    pub secs: f64,
    /// When it was added (Unix milliseconds).
    pub added: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Library {
    /// The message the hotkey plays.
    pub active: Option<String>,
    /// In the order they were added.
    pub messages: Vec<Entry>,
}

/// Ids name files, so they're kept to letters, digits, `-` and `_`.
fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A name as typed or taken from a file name: one line, single spaces, at
/// most `MAX_NAME_CHARS` characters. May come back empty.
pub fn clean_name(raw: &str) -> String {
    let one_line: String = raw.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let joined = one_line.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(MAX_NAME_CHARS).collect::<String>().trim_end().to_string()
}

impl Library {
    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.messages.iter().find(|m| m.id == id)
    }

    /// The message the hotkey plays, if one is selected.
    pub fn active_entry(&self) -> Option<&Entry> {
        self.active.as_deref().and_then(|id| self.get(id))
    }

    fn name_taken(&self, name: &str, except: Option<&str>) -> bool {
        let name = name.to_lowercase();
        self.messages
            .iter()
            .any(|m| Some(m.id.as_str()) != except && m.name.to_lowercase() == name)
    }

    /// "Message 1", "Message 2"…: the suggested name for a new recording.
    pub fn next_default_name(&self) -> String {
        (self.messages.len() + 1..)
            .map(|n| format!("Message {n}"))
            .find(|n| !self.name_taken(n, None))
            .expect("an unused number")
    }

    /// `name`, cleaned, or "Name (2)", "Name (3)"… when another message
    /// (other than `except`) already has it. Empty gets the default name.
    pub fn unique_name(&self, name: &str, except: Option<&str>) -> String {
        let base = clean_name(name);
        if base.is_empty() {
            return self.next_default_name();
        }
        if !self.name_taken(&base, except) {
            return base;
        }
        (2..)
            .map(|i| format!("{base} ({i})"))
            .find(|n| !self.name_taken(n, except))
            .expect("an unused number")
    }

    fn new_id(&self, now_ms: u64) -> String {
        (now_ms..)
            .map(|n| format!("m{n}"))
            .find(|id| self.get(id).is_none())
            .expect("an unused id")
    }

    /// Add a message. It's selected when nothing else is.
    pub fn add(&mut self, entry: Entry) {
        if self.active_entry().is_none() {
            self.active = Some(entry.id.clone());
        }
        self.messages.push(entry);
    }

    /// Select the message the hotkey plays. False if there's no such message.
    pub fn select(&mut self, id: &str) -> bool {
        if self.get(id).is_none() {
            return false;
        }
        self.active = Some(id.to_string());
        true
    }

    /// Rename a message; returns the name it got (made unique).
    pub fn rename(&mut self, id: &str, name: &str) -> Result<String> {
        if clean_name(name).is_empty() {
            return Err(anyhow!("give the message a name"));
        }
        let name = self.unique_name(name, Some(id));
        let entry = self
            .messages
            .iter_mut()
            .find(|m| m.id == id)
            .ok_or_else(|| anyhow!("that message no longer exists"))?;
        entry.name = name.clone();
        Ok(name)
    }

    /// Remove a message. If it was the selected one, nothing is selected
    /// afterwards (so a call never gets a message you didn't pick), unless a
    /// single message is left, which then is.
    pub fn remove(&mut self, id: &str) -> Option<Entry> {
        let at = self.messages.iter().position(|m| m.id == id)?;
        let removed = self.messages.remove(at);
        self.normalize();
        Some(removed)
    }

    /// Repair a list that was edited by hand or partly written: unsafe or
    /// repeated ids are dropped, and the selection must point at a message.
    /// A lone message is always the selected one.
    fn normalize(&mut self) {
        let mut seen = HashSet::new();
        self.messages.retain(|m| is_safe_id(&m.id) && seen.insert(m.id.clone()));
        for m in &mut self.messages {
            if !m.secs.is_finite() || m.secs < 0.0 {
                m.secs = 0.0;
            }
            let name = clean_name(&m.name);
            m.name = if name.is_empty() { "Message".to_string() } else { name };
        }
        if self.active_entry().is_none() {
            self.active = match self.messages.as_slice() {
                [only] => Some(only.id.clone()),
                _ => None,
            };
        }
    }
}

/// The message list on disk. Callers serialize changes (see `AppState`).
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// Open the list in `dir` (created if needed), bringing in the single
    /// message from before the list if it's still there.
    pub fn open(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(dir.join("messages")).context("create the SayIt folder")?;
        let store = Self { dir };
        if let Err(e) = store.migrate_legacy() {
            eprintln!("sayit: couldn't bring in the old message: {e:#}");
        }
        Ok(store)
    }

    fn library_path(&self) -> PathBuf {
        self.dir.join("library.json")
    }

    fn audio_path(&self, id: &str) -> PathBuf {
        self.dir.join("messages").join(format!("{id}.wav"))
    }

    pub fn load(&self) -> Library {
        let mut lib: Library = std::fs::read(self.library_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        lib.normalize();
        lib
    }

    /// Written to a temp file first, so the list is never left half-written.
    fn save(&self, lib: &Library) -> Result<()> {
        let tmp = self.dir.join("library.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(lib)?).context("save the message list")?;
        std::fs::rename(&tmp, self.library_path()).context("save the message list")?;
        Ok(())
    }

    /// Save `samples` (mono at `rate`) as a new message called `name`.
    pub fn add(&self, name: &str, samples: &[f32], rate: u32) -> Result<Entry> {
        let mut lib = self.load();
        let now = now_ms();
        let id = lib.new_id(now);
        let path = self.audio_path(&id);
        std::fs::write(&path, encode_wav(samples, rate)?).context("save the message")?;
        let entry = Entry {
            name: lib.unique_name(name, None),
            secs: samples.len() as f64 / rate.max(1) as f64,
            added: now,
            id,
        };
        lib.add(entry.clone());
        if let Err(e) = self.save(&lib) {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        Ok(entry)
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<String> {
        let mut lib = self.load();
        let name = lib.rename(id, name)?;
        self.save(&lib)?;
        Ok(name)
    }

    pub fn select(&self, id: &str) -> Result<()> {
        let mut lib = self.load();
        if !lib.select(id) {
            return Err(anyhow!("that message no longer exists"));
        }
        self.save(&lib)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let mut lib = self.load();
        if lib.remove(id).is_none() {
            return Ok(()); // already gone
        }
        self.save(&lib)?;
        // After the list: a stray file is harmless, a listed message without
        // its audio isn't.
        let _ = std::fs::remove_file(self.audio_path(id));
        Ok(())
    }

    /// A message's audio: mono samples and their rate.
    pub fn read(&self, id: &str) -> Result<(Vec<f32>, u32)> {
        if self.load().get(id).is_none() {
            return Err(anyhow!("that message no longer exists"));
        }
        read_wav(&self.audio_path(id))
    }

    /// A message's WAV file as it is on disk, for listening to it.
    pub fn wav_bytes(&self, id: &str) -> Result<Vec<u8>> {
        if self.load().get(id).is_none() {
            return Err(anyhow!("that message no longer exists"));
        }
        std::fs::read(self.audio_path(id)).context("read the message")
    }

    /// The single `message.wav` from before the list becomes "My message",
    /// and stays the one that plays. Copied, listed, then removed, so a
    /// crash at any point loses nothing.
    fn migrate_legacy(&self) -> Result<()> {
        let old = self.dir.join("message.wav");
        if !old.exists() {
            return Ok(());
        }
        let mut lib = self.load();
        if lib.get(LEGACY_ID).is_none() {
            let secs = wav_secs(&old)?;
            std::fs::copy(&old, self.audio_path(LEGACY_ID)).context("copy the old message")?;
            let entry = Entry {
                id: LEGACY_ID.to_string(),
                name: lib.unique_name(LEGACY_NAME, None),
                secs,
                added: now_ms(),
            };
            lib.add(entry);
            lib.active = Some(LEGACY_ID.to_string());
            self.save(&lib)?;
        }
        std::fs::remove_file(&old).context("remove the old message")?;
        Ok(())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Mono 16-bit PCM WAV.
pub fn encode_wav(samples: &[f32], rate: u32) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = Cursor::new(Vec::with_capacity(samples.len() * 2 + 44));
    {
        let mut w = hound::WavWriter::new(&mut buf, spec).context("wav writer")?;
        for &s in samples {
            w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
        w.finalize().context("finalize wav")?;
    }
    Ok(buf.into_inner())
}

/// Any PCM WAV, as mono samples and their rate.
pub fn read_wav(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let full = (1i64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / full))
                .collect::<Result<_, _>>()?
        }
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
    };
    let mono = if channels == 1 {
        interleaved
    } else {
        interleaved
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    Ok((mono, spec.sample_rate))
}

fn wav_secs(path: &Path) -> Result<f64> {
    let reader = hound::WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    Ok(reader.duration() as f64 / reader.spec().sample_rate.max(1) as f64)
}

/// Undo `encodeURIComponent` (how the window sends a file's name).
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, name: &str) -> Entry {
        Entry { id: id.into(), name: name.into(), secs: 1.0, added: 0 }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sayit-test-{tag}-{}-{}", std::process::id(), now_ms()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn tone(secs: f64, rate: u32) -> Vec<f32> {
        (0..(secs * rate as f64) as usize)
            .map(|i| (i as f32 * 0.05).sin() * 0.5)
            .collect()
    }

    #[test]
    fn first_message_is_selected_later_ones_are_not() {
        let mut lib = Library::default();
        lib.add(entry("a", "Alpha"));
        lib.add(entry("b", "Beta"));
        assert_eq!(lib.active.as_deref(), Some("a"));
        assert!(lib.select("b"));
        assert_eq!(lib.active_entry().unwrap().name, "Beta");
        assert!(!lib.select("nope"));
        assert_eq!(lib.active.as_deref(), Some("b"));
    }

    #[test]
    fn removing_the_selected_message_selects_nothing_unless_one_is_left() {
        let mut lib = Library::default();
        for id in ["a", "b", "c"] {
            lib.add(entry(id, id));
        }
        lib.select("b");
        lib.remove("b");
        assert_eq!(lib.active, None, "two left: you pick");
        lib.select("a");
        lib.remove("a");
        assert_eq!(lib.active.as_deref(), Some("c"), "one left: it plays");
        assert!(lib.remove("zzz").is_none());
    }

    #[test]
    fn removing_another_message_keeps_the_selection() {
        let mut lib = Library::default();
        lib.add(entry("a", "A"));
        lib.add(entry("b", "B"));
        lib.add(entry("c", "C"));
        lib.remove("c");
        assert_eq!(lib.active.as_deref(), Some("a"));
    }

    #[test]
    fn new_message_is_selected_when_nothing_is() {
        let mut lib = Library::default();
        lib.add(entry("a", "A"));
        lib.add(entry("b", "B"));
        lib.add(entry("c", "C"));
        lib.remove("a"); // selected one gone, two left: nothing selected
        assert_eq!(lib.active, None);
        lib.add(entry("d", "D"));
        assert_eq!(lib.active.as_deref(), Some("d"));
    }

    #[test]
    fn names_are_cleaned_and_kept_unique() {
        let mut lib = Library::default();
        lib.add(entry("a", "Project X"));
        assert_eq!(lib.unique_name("  project   x ", None), "project x (2)");
        assert_eq!(lib.unique_name("Project X", Some("a")), "Project X", "its own name is free");
        assert_eq!(lib.unique_name("line\nbreak\tand tab", None), "line break and tab");
        assert_eq!(clean_name(&"y".repeat(100)).chars().count(), MAX_NAME_CHARS);
        assert_eq!(lib.unique_name("   ", None), "Message 2");
    }

    #[test]
    fn default_names_skip_taken_numbers() {
        let mut lib = Library::default();
        assert_eq!(lib.next_default_name(), "Message 1");
        lib.add(entry("a", "Message 2"));
        assert_eq!(lib.next_default_name(), "Message 3");
        lib.add(entry("b", "Other"));
        assert_eq!(lib.next_default_name(), "Message 3");
    }

    #[test]
    fn rename_needs_a_name_and_an_existing_message() {
        let mut lib = Library::default();
        lib.add(entry("a", "A"));
        lib.add(entry("b", "B"));
        assert_eq!(lib.rename("a", "  Client  Q4 ").unwrap(), "Client Q4");
        assert_eq!(lib.rename("b", "client q4").unwrap(), "client q4 (2)");
        assert!(lib.rename("a", " \n ").is_err());
        assert!(lib.rename("zzz", "Name").is_err());
    }

    #[test]
    fn hand_edited_list_is_repaired() {
        let json = r#"{"active":"gone","messages":[
            {"id":"../escape","name":"bad","secs":1},
            {"id":"ok","name":"","secs":-3},
            {"id":"ok","name":"dup","secs":1}
        ]}"#;
        let mut lib: Library = serde_json::from_str(json).unwrap();
        lib.normalize();
        assert_eq!(lib.messages.len(), 1);
        assert_eq!(lib.messages[0].id, "ok");
        assert_eq!(lib.messages[0].name, "Message");
        assert_eq!(lib.messages[0].secs, 0.0);
        assert_eq!(lib.active.as_deref(), Some("ok"), "a lone message is selected");
    }

    #[test]
    fn ids_are_unique_and_file_safe() {
        let mut lib = Library::default();
        let a = lib.new_id(5);
        lib.add(entry(&a, "A"));
        let b = lib.new_id(5);
        assert_ne!(a, b);
        assert!(is_safe_id(&a) && is_safe_id(&b));
        assert!(!is_safe_id("../x") && !is_safe_id("") && !is_safe_id("a b"));
    }

    #[test]
    fn store_adds_reads_renames_selects_and_removes() {
        let dir = temp_dir("store");
        let store = Store::open(dir.clone()).unwrap();
        let a = store.add("Alpha", &tone(1.0, 16_000), 16_000).unwrap();
        let b = store.add("Beta", &tone(2.0, 24_000), 24_000).unwrap();
        assert!((b.secs - 2.0).abs() < 1e-9);

        let lib = store.load();
        assert_eq!(lib.messages.len(), 2);
        assert_eq!(lib.active.as_deref(), Some(a.id.as_str()));

        let (samples, rate) = store.read(&b.id).unwrap();
        assert_eq!(rate, 24_000);
        assert_eq!(samples.len(), 48_000);
        assert!(store.wav_bytes(&a.id).unwrap().starts_with(b"RIFF"));

        store.select(&b.id).unwrap();
        assert_eq!(store.rename(&b.id, "Beta v2").unwrap(), "Beta v2");
        let lib = store.load();
        assert_eq!(lib.active_entry().unwrap().name, "Beta v2");

        store.remove(&b.id).unwrap();
        assert!(!dir.join("messages").join(format!("{}.wav", b.id)).exists());
        assert!(store.read(&b.id).is_err(), "removed messages can't be read");
        assert!(store.select("../../etc").is_err());
        assert_eq!(store.load().active.as_deref(), Some(a.id.as_str()), "one left: it plays");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_single_message_becomes_my_message() {
        let dir = temp_dir("legacy");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("message.wav"), encode_wav(&tone(1.5, 16_000), 16_000).unwrap()).unwrap();

        let store = Store::open(dir.clone()).unwrap();
        let lib = store.load();
        assert_eq!(lib.messages.len(), 1);
        assert_eq!(lib.messages[0].name, "My message");
        assert!((lib.messages[0].secs - 1.5).abs() < 1e-6);
        assert_eq!(lib.active.as_deref(), Some(LEGACY_ID));
        assert!(!dir.join("message.wav").exists());
        assert_eq!(store.read(LEGACY_ID).unwrap().0.len(), 24_000);

        // Opening again changes nothing.
        let again = Store::open(dir.clone()).unwrap().load();
        assert_eq!(again.messages.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wav_round_trip_downmixes_and_keeps_the_rate() {
        let dir = temp_dir("wav");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stereo.wav");
        let spec = hound::WavSpec { channels: 2, sample_rate: 44_100, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..100 {
            w.write_sample((0.5 * i16::MAX as f32) as i16).unwrap();
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
        let (mono, rate) = read_wav(&path).unwrap();
        assert_eq!(rate, 44_100);
        assert_eq!(mono.len(), 100);
        assert!((mono[0] - 0.25).abs() < 1e-3, "{}", mono[0]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn percent_decoding_matches_encode_uri_component() {
        // encodeURIComponent("Project Ä – voicemail 1.mp3")
        assert_eq!(
            percent_decode("Project%20%C3%84%20%E2%80%93%20voicemail%201.mp3"),
            "Project Ä – voicemail 1.mp3"
        );
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode(""), "");
    }
}
