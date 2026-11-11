//! SayIt — play a saved voice message (e.g. a voicemail) into a call with
//! one hotkey, and press it again to stop.
//!
//! A call app only sends the other side what comes in through its
//! microphone, so SayIt works through a virtual audio cable (VB-Audio
//! Virtual Cable): the call app's microphone is set to "CABLE Output", and
//! SayIt feeds the cable's playback side ("CABLE Input") with the real
//! microphone — or, while a message plays, with the message instead (the
//! mic is muted for that time). Without a cable, messages play on the
//! default speakers so they can still be checked, but calls won't hear them.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

/// Mic audio queued before it starts flowing into the cable, to ride out
/// scheduling jitter between the two devices without crackling.
const PRIME_SECS: f64 = 0.03;
/// Upper bound on queued mic audio, so the call never lags behind the user.
const MAX_QUEUE_SECS: f64 = 0.15;

/// Where SayIt messages currently go.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Route {
    /// Messages reach calls (a virtual cable is connected).
    pub to_call: bool,
    /// The cable's playback device, when connected.
    pub device: String,
    /// The real microphone passed through to the call, when it is.
    pub mic: String,
    pub error: Option<String>,
}

/// What one output stream plays: queued mic audio, or a message in its place.
struct Mix {
    rate: u32,
    passthrough: bool,
    mic: VecDeque<f32>,
    primed: bool,
    prime: usize,
    max_queue: usize,
    clip: Option<Clip>,
}

struct Clip {
    samples: Vec<f32>,
    pos: usize,
}

impl Mix {
    fn new(rate: u32, passthrough: bool) -> Self {
        Self {
            rate,
            passthrough,
            mic: VecDeque::new(),
            primed: false,
            prime: (rate as f64 * PRIME_SECS) as usize,
            max_queue: (rate as f64 * MAX_QUEUE_SECS) as usize,
            clip: None,
        }
    }

    fn push_mic(&mut self, samples: &[f32]) {
        // The mic is muted while a message plays.
        if !self.passthrough || self.clip.is_some() {
            return;
        }
        self.mic.extend(samples.iter().copied());
        while self.mic.len() > self.max_queue {
            self.mic.pop_front();
        }
    }

    fn next_sample(&mut self) -> f32 {
        if let Some(clip) = self.clip.as_mut() {
            let v = clip.samples[clip.pos];
            clip.pos += 1;
            let done = clip.pos >= clip.samples.len();
            if done {
                self.clip = None;
            }
            return v;
        }
        if !self.passthrough {
            return 0.0;
        }
        if !self.primed {
            if self.mic.len() < self.prime {
                return 0.0;
            }
            self.primed = true;
        }
        match self.mic.pop_front() {
            Some(v) => v,
            None => {
                self.primed = false;
                0.0
            }
        }
    }

    fn start_clip(&mut self, samples: Vec<f32>) {
        self.mic.clear();
        self.primed = false;
        self.clip = if samples.is_empty() { None } else { Some(Clip { samples, pos: 0 }) };
    }

    fn remaining_secs(&self) -> Option<f64> {
        self.clip
            .as_ref()
            .map(|c| (c.samples.len() - c.pos) as f64 / self.rate as f64)
    }
}

/// Streaming linear-interpolation resampler — plenty for speech on a call.
struct Linear {
    step: f64,
    next: f64,
    prev: f32,
}

impl Linear {
    fn new(from: u32, to: u32) -> Self {
        Self { step: from as f64 / to as f64, next: 1.0, prev: 0.0 }
    }

    fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        for &x in input {
            while self.next <= 1.0 {
                out.push(self.prev + (x - self.prev) * self.next as f32);
                self.next += self.step;
            }
            self.next -= 1.0;
            self.prev = x;
        }
    }
}

fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to {
        return input.to_vec();
    }
    let mut out = Vec::with_capacity((input.len() as f64 * to as f64 / from as f64) as usize + 2);
    Linear::new(from, to).push(input, &mut out);
    out
}

/// Bring a freshly recorded message to a consistent level (peak about
/// -1 dBFS, at most 18 dB of gain) and cut the silence before and after it,
/// so it starts the moment the hotkey is pressed.
pub fn prepare_message(samples: &[f32], rate: u32) -> Vec<f32> {
    let frame = (rate / 50).max(1) as usize; // 20 ms
    let rms: Vec<f32> = samples
        .chunks(frame)
        .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let loudest = rms.iter().copied().fold(0.0f32, f32::max);
    if loudest <= 0.0 {
        return samples.to_vec();
    }
    let voiced = |r: &f32| *r >= loudest * 0.06;
    let first = rms.iter().position(voiced).unwrap_or(0);
    let last = rms.iter().rposition(voiced).unwrap_or(rms.len() - 1);
    let pad = (rate as usize * 15 / 100).div_ceil(frame); // 150 ms
    let start = first.saturating_sub(pad) * frame;
    let end = ((last + 1 + pad) * frame).min(samples.len());
    let mut out = samples[start..end].to_vec();

    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 1e-4 {
        let gain = (0.89 / peak).min(8.0);
        for s in &mut out {
            *s *= gain;
        }
    }
    out
}

/// True for a virtual cable / mixer device (never a real microphone).
fn looks_virtual(name: &str) -> bool {
    let l = name.to_lowercase();
    l.contains("cable output") || l.contains("cable input") || l.contains("vb-audio") || l.contains("voicemeeter")
}

/// The playback side of a VB-Audio virtual cable, if one is installed.
pub fn find_cable_input() -> Option<String> {
    cpal::default_host()
        .output_devices()
        .ok()?
        .filter_map(|d| d.name().ok())
        .find(|n| n.to_lowercase().contains("cable input"))
}

/// The real microphone to pass through: the one chosen in Settings, else
/// the system default — never a virtual cable, which would loop SayIt's own
/// output back into itself.
fn physical_mic(preferred: &str) -> Result<cpal::Device> {
    let host = cpal::default_host();
    let name_of = |d: &cpal::Device| d.name().unwrap_or_default();
    let inputs: Vec<cpal::Device> = host.input_devices()?.collect();
    if !preferred.is_empty() && !looks_virtual(preferred) {
        if let Some(d) = inputs.iter().find(|d| name_of(d) == preferred) {
            return Ok(d.clone());
        }
    }
    if let Some(d) = host.default_input_device() {
        if !looks_virtual(&name_of(&d)) {
            return Ok(d);
        }
    }
    inputs
        .into_iter()
        .find(|d| !looks_virtual(&name_of(d)))
        .ok_or_else(|| anyhow!("no physical microphone found"))
}

enum Cmd {
    /// (Re)connect to a cable's playback device, passing `mic` through when
    /// given; `output: None` disconnects.
    Connect { output: Option<String>, mic: Option<String>, reply: Sender<Route> },
    Play { samples: Vec<f32>, rate: u32, reply: Sender<Result<f64>> },
    Stop,
}

/// Handle to the SayIt audio thread (cpal streams aren't `Send`, so they
/// live on a dedicated thread, like `audio::Recorder`).
pub struct SayIt {
    tx: Sender<Cmd>,
    cable: Arc<Mutex<Option<Arc<Mutex<Mix>>>>>,
    speaker: Arc<Mutex<Option<Arc<Mutex<Mix>>>>>,
    route: Arc<Mutex<Route>>,
}

impl SayIt {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel();
        let cable = Arc::new(Mutex::new(None));
        let speaker = Arc::new(Mutex::new(None));
        let route = Arc::new(Mutex::new(Route::default()));
        let (c, s, r) = (cable.clone(), speaker.clone(), route.clone());
        std::thread::Builder::new()
            .name("sayit-audio".into())
            .spawn(move || audio_thread(rx, c, s, r))
            .expect("spawn SayIt audio thread");
        Self { tx, cable, speaker, route }
    }

    pub fn connect(&self, output: Option<String>, mic: Option<String>) -> Route {
        let (reply, resp) = mpsc::channel();
        if self.tx.send(Cmd::Connect { output, mic, reply }).is_err() {
            return Route { error: Some("SayIt audio thread stopped".into()), ..Route::default() };
        }
        resp.recv().unwrap_or_default()
    }

    pub fn route(&self) -> Route {
        self.route.lock().clone()
    }

    /// Start playing `samples` (mono at `rate`); returns its length in seconds.
    pub fn play(&self, samples: Vec<f32>, rate: u32) -> Result<f64> {
        let (reply, resp) = mpsc::channel();
        self.tx
            .send(Cmd::Play { samples, rate, reply })
            .map_err(|_| anyhow!("SayIt audio thread stopped"))?;
        resp.recv().map_err(|_| anyhow!("SayIt audio thread stopped"))?
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }

    /// Seconds of message left, or `None` when nothing is playing.
    pub fn remaining_secs(&self) -> Option<f64> {
        let from = |slot: &Mutex<Option<Arc<Mutex<Mix>>>>| slot.lock().as_ref().and_then(|m| m.lock().remaining_secs());
        from(&self.cable).or_else(|| from(&self.speaker))
    }
}

fn audio_thread(
    rx: Receiver<Cmd>,
    cable_slot: Arc<Mutex<Option<Arc<Mutex<Mix>>>>>,
    speaker_slot: Arc<Mutex<Option<Arc<Mutex<Mix>>>>>,
    route: Arc<Mutex<Route>>,
) {
    let mut cable_streams: Vec<cpal::Stream> = Vec::new();
    let mut speaker_stream: Option<cpal::Stream> = None;
    loop {
        let cmd = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(cmd) => cmd,
            Err(RecvTimeoutError::Timeout) => {
                // Close the speaker stream once its message has finished.
                let finished = speaker_slot.lock().as_ref().is_some_and(|m| m.lock().clip.is_none());
                if finished {
                    speaker_stream = None;
                    *speaker_slot.lock() = None;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match cmd {
            Cmd::Connect { output, mic, reply } => {
                cable_streams.clear();
                *cable_slot.lock() = None;
                let r = match output {
                    Some(name) => match open_cable(&name, mic.as_deref()) {
                        Ok((streams, mix, r)) => {
                            cable_streams = streams;
                            *cable_slot.lock() = Some(mix);
                            r
                        }
                        Err(e) => Route { error: Some(e.to_string()), ..Route::default() },
                    },
                    None => Route::default(),
                };
                *route.lock() = r.clone();
                let _ = reply.send(r);
            }
            Cmd::Play { samples, rate, reply } => {
                let cable = cable_slot.lock().clone();
                let result = match cable {
                    Some(mix) => {
                        let mut m = mix.lock();
                        let clip = resample(&samples, rate, m.rate);
                        let secs = clip.len() as f64 / m.rate as f64;
                        m.start_clip(clip);
                        Ok(secs)
                    }
                    None => open_speaker(&samples, rate).map(|(stream, mix, secs)| {
                        speaker_stream = Some(stream);
                        *speaker_slot.lock() = Some(mix);
                        secs
                    }),
                };
                let _ = reply.send(result);
            }
            Cmd::Stop => {
                if let Some(m) = cable_slot.lock().as_ref() {
                    m.lock().clip = None;
                }
                speaker_stream = None;
                *speaker_slot.lock() = None;
            }
        }
    }
    drop(speaker_stream);
}

fn output_device(name: &str) -> Result<cpal::Device> {
    cpal::default_host()
        .output_devices()?
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
        .ok_or_else(|| anyhow!("playback device '{name}' not found"))
}

/// Open the cable's playback side, plus the mic pass-through when asked.
fn open_cable(output: &str, mic: Option<&str>) -> Result<(Vec<cpal::Stream>, Arc<Mutex<Mix>>, Route)> {
    let device = output_device(output)?;
    let config = device.default_output_config().context("query cable output format")?;
    let rate = config.sample_rate().0;
    let mix = Arc::new(Mutex::new(Mix::new(rate, mic.is_some())));
    let out = build_output(&device, config, mix.clone())?;
    let mut route = Route { to_call: true, device: output.to_string(), ..Route::default() };
    let mut streams = vec![out];
    if let Some(preferred) = mic {
        match open_mic(preferred, rate, mix.clone()) {
            Ok((stream, name)) => {
                streams.push(stream);
                route.mic = name;
            }
            Err(e) => {
                mix.lock().passthrough = false;
                route.error = Some(format!("your microphone isn't reaching the call: {e}"));
            }
        }
    }
    Ok((streams, mix, route))
}

/// No cable: play the message on the default speakers instead.
fn open_speaker(samples: &[f32], rate: u32) -> Result<(cpal::Stream, Arc<Mutex<Mix>>, f64)> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| anyhow!("no speakers found"))?;
    let config = device.default_output_config().context("query speaker format")?;
    let out_rate = config.sample_rate().0;
    let mut mix = Mix::new(out_rate, false);
    let clip = resample(samples, rate, out_rate);
    let secs = clip.len() as f64 / out_rate as f64;
    mix.start_clip(clip);
    let mix = Arc::new(Mutex::new(mix));
    let stream = build_output(&device, config, mix.clone())?;
    Ok((stream, mix, secs))
}

fn build_output(device: &cpal::Device, config: cpal::SupportedStreamConfig, mix: Arc<Mutex<Mix>>) -> Result<cpal::Stream> {
    let channels = config.channels().max(1) as usize;
    let format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();
    let err = |e| eprintln!("sayit output stream error: {e}");
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            &stream_config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut m = mix.lock();
                for frame in data.chunks_mut(channels) {
                    let v = m.next_sample();
                    frame.fill(v);
                }
            },
            err,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_output_stream(
            &stream_config,
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                let mut m = mix.lock();
                for frame in data.chunks_mut(channels) {
                    let v = (m.next_sample().clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                    frame.fill(v);
                }
            },
            err,
            None,
        )?,
        other => return Err(anyhow!("unsupported playback sample format {other:?}")),
    };
    stream.play().context("start playback")?;
    Ok(stream)
}

fn open_mic(preferred: &str, out_rate: u32, mix: Arc<Mutex<Mix>>) -> Result<(cpal::Stream, String)> {
    let device = physical_mic(preferred)?;
    let name = device.name().unwrap_or_default();
    let config = device.default_input_config().context("query microphone format")?;
    let channels = config.channels().max(1) as usize;
    let mic_rate = config.sample_rate().0;
    let format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();
    let mut resampler = Linear::new(mic_rate, out_rate);
    let mut mono: Vec<f32> = Vec::new();
    let mut out: Vec<f32> = Vec::new();
    let mut feed = move |samples: &mut dyn Iterator<Item = f32>| {
        mono.clear();
        let mut frame_sum = 0.0;
        for (i, s) in samples.enumerate() {
            frame_sum += s;
            if (i + 1) % channels == 0 {
                mono.push(frame_sum / channels as f32);
                frame_sum = 0.0;
            }
        }
        out.clear();
        if mic_rate == out_rate {
            out.extend_from_slice(&mono);
        } else {
            resampler.push(&mono, &mut out);
        }
        mix.lock().push_mic(&out);
    };
    let err = |e| eprintln!("sayit microphone stream error: {e}");
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| feed(&mut data.iter().copied()),
            err,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                feed(&mut data.iter().map(|&v| v as f32 / i16::MAX as f32))
            },
            err,
            None,
        )?,
        other => return Err(anyhow!("unsupported microphone sample format {other:?}")),
    };
    stream.play().context("start microphone")?;
    Ok((stream, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_replaces_mic_and_mutes_it() {
        let mut m = Mix::new(1000, true);
        m.push_mic(&[0.1; 40]);
        assert_eq!(m.next_sample(), 0.1);
        m.start_clip(vec![0.5, 0.6]);
        m.push_mic(&[0.9; 40]); // muted while the message plays
        assert_eq!(m.next_sample(), 0.5);
        assert_eq!(m.remaining_secs(), Some(0.001));
        assert_eq!(m.next_sample(), 0.6);
        assert!(m.clip.is_none());
        assert_eq!(m.next_sample(), 0.0); // mic queue was cleared
    }

    #[test]
    fn mic_waits_for_a_small_buffer_then_flows() {
        let mut m = Mix::new(1000, true); // prime = 30 samples
        m.push_mic(&[0.2; 10]);
        assert_eq!(m.next_sample(), 0.0);
        m.push_mic(&[0.2; 30]);
        assert_eq!(m.next_sample(), 0.2);
    }

    #[test]
    fn mic_queue_is_capped_so_calls_dont_lag() {
        let mut m = Mix::new(1000, true); // max 150 samples
        m.push_mic(&[0.3; 1000]);
        assert_eq!(m.mic.len(), 150);
    }

    #[test]
    fn speaker_mode_plays_only_the_message() {
        let mut m = Mix::new(1000, false);
        m.push_mic(&[0.4; 100]);
        assert_eq!(m.next_sample(), 0.0);
    }

    #[test]
    fn linear_resampler_keeps_rate_ratio_and_values() {
        assert_eq!(resample(&[0.1, 0.2, 0.3], 8000, 8000), vec![0.1, 0.2, 0.3]);
        let up = resample(&[0.0, 1.0, 0.0, 1.0], 16_000, 48_000);
        assert!((up.len() as i64 - 10).abs() <= 2, "{}", up.len());
        let down = resample(&vec![0.5; 4800], 48_000, 16_000);
        assert!((down.len() as i64 - 1600).abs() <= 2);
        assert!(down.iter().all(|&v| (v - 0.5).abs() < 1e-6));
    }

    #[test]
    fn message_is_trimmed_and_leveled() {
        let rate = 1000;
        let mut s = vec![0.0f32; 1000]; // 1 s silence
        s.extend(vec![0.1f32; 500]); // 0.5 s speech
        s.extend(vec![0.0f32; 1000]);
        let out = prepare_message(&s, rate);
        assert!(out.len() < 1000, "silence trimmed, got {}", out.len());
        let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.8).abs() < 1e-4, "gain is capped at 8x: {peak}");
    }

    #[test]
    fn cables_are_never_used_as_the_microphone() {
        assert!(looks_virtual("CABLE Output (VB-Audio Virtual Cable)"));
        assert!(looks_virtual("VoiceMeeter Output (VB-Audio VoiceMeeter VAIO)"));
        assert!(!looks_virtual("Microphone Array (Intel® Smart Sound Technology)"));
        assert!(!looks_virtual("External Microphone (Realtek(R) Audio)"));
    }
}
