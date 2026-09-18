//! Microphone + system-audio (WASAPI loopback) capture via cpal.
//!
//! `cpal::Stream` is not `Send`, so it cannot live in Tauri's managed state. We
//! run each stream on its own dedicated OS thread that owns it and takes commands
//! over a channel. `Recorder` is the `Send + Sync` handle to that thread: it
//! records raw f32 samples while active, then down-mixes to mono and resamples
//! to 16 kHz (whisper.cpp's required input format) when stopped.
//!
//! System-audio ("what you hear" — e.g. the other party's voice during a call)
//! capture is WASAPI loopback: cpal has no separate "loopback device" concept —
//! you open a normal OUTPUT device (`default_output_device()`/`output_devices()`)
//! and call `.build_input_stream()` on it exactly like a mic. cpal detects the
//! device's data flow is `eRender` and transparently sets
//! `AUDCLNT_STREAMFLAGS_LOOPBACK` internally. The one non-obvious part: you must
//! query the format via `default_output_config()` (the output-side method) since
//! `default_input_config()` on a render device returns an error. This has worked
//! unchanged on the `cpal = "0.15"` already pinned in Cargo.toml since cpal's
//! WASAPI loopback support was added in 2019 — no crate upgrade was needed.
//!
//! Mic and loopback are kept as two SEPARATE tracks (not mixed), per an
//! explicit product decision for call recording — each is its own `Recorder`
//! running on its own thread, independently resampled (they are not
//! guaranteed to share a sample rate or channel count).

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;

/// Whisper expects 16 kHz mono f32 PCM.
pub const TARGET_SR: u32 = 16_000;
/// Safety cap so a forgotten "listening" session can't grow without bound.
const MAX_SECONDS: usize = 240;

/// Which side of the audio path a `Recorder` captures — selects which cpal
/// device-enumeration/config-query methods to use (see module doc comment).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Mic,
    Loopback,
}

enum Cmd {
    Start { device: String, reply: Sender<Result<()>> },
    Stop { reply: Sender<Result<Vec<f32>>> },
}

pub struct Recorder {
    tx: Sender<Cmd>,
    recording: std::sync::atomic::AtomicBool,
    _guard: Mutex<()>,
}

impl Recorder {
    pub fn spawn(source: Source) -> Self {
        let (tx, rx) = mpsc::channel::<Cmd>();
        std::thread::Builder::new()
            .name(match source {
                Source::Mic => "audio-capture-mic".into(),
                Source::Loopback => "audio-capture-loopback".into(),
            })
            .spawn(move || audio_thread(rx, source))
            .expect("spawn audio thread");
        Self {
            tx,
            recording: std::sync::atomic::AtomicBool::new(false),
            _guard: Mutex::new(()),
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recording.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn start(&self, device_name: &str) -> Result<()> {
        let (reply, resp) = mpsc::channel();
        self.tx
            .send(Cmd::Start {
                device: device_name.to_string(),
                reply,
            })
            .map_err(|_| anyhow!("audio thread gone"))?;
        let r = resp.recv().map_err(|_| anyhow!("audio thread gone"))?;
        if r.is_ok() {
            self.recording
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        r
    }

    pub fn stop(&self) -> Result<Vec<f32>> {
        let (reply, resp) = mpsc::channel();
        self.tx
            .send(Cmd::Stop { reply })
            .map_err(|_| anyhow!("audio thread gone"))?;
        self.recording
            .store(false, std::sync::atomic::Ordering::SeqCst);
        resp.recv().map_err(|_| anyhow!("audio thread gone"))?
    }
}

struct Active {
    stream: cpal::Stream,
    buffer: std::sync::Arc<Mutex<Vec<f32>>>,
    channels: u16,
    sample_rate: u32,
}

fn audio_thread(rx: Receiver<Cmd>, source: Source) {
    let mut active: Option<Active> = None;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Start { device, reply } => {
                if active.is_some() {
                    let _ = reply.send(Ok(()));
                    continue;
                }
                let opened = match source {
                    Source::Mic => open_mic_stream(&device),
                    Source::Loopback => open_loopback_stream(&device),
                };
                match opened {
                    Ok(a) => {
                        active = Some(a);
                        let _ = reply.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            Cmd::Stop { reply } => {
                let Some(a) = active.take() else {
                    let _ = reply.send(Ok(Vec::new()));
                    continue;
                };
                drop(a.stream); // stop the callback
                let raw = std::mem::take(&mut *a.buffer.lock().unwrap());
                let _ = reply.send(finish(raw, a.channels, a.sample_rate));
            }
        }
    }
}

/// Build the `Active` stream state common to both mic and loopback capture —
/// the only difference between the two is which `cpal::Device` is handed in
/// and which config-query method was used to get `config` (see callers).
fn build_active_from_config(
    device: &cpal::Device,
    config: cpal::SupportedStreamConfig,
    err_label: &'static str,
) -> Result<Active> {
    let channels = config.channels();
    let sample_rate = config.sample_rate().0;
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();

    let buffer = std::sync::Arc::new(Mutex::new(Vec::<f32>::with_capacity(
        sample_rate as usize * 8,
    )));
    let cap_limit = (sample_rate as usize) * channels as usize * MAX_SECONDS;

    let buf_cb = buffer.clone();
    let err_cb = move |e| eprintln!("{err_label} stream error: {e}");

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _| push(&buf_cb, data.iter().copied(), cap_limit),
            err_cb,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _| {
                push(
                    &buf_cb,
                    data.iter().map(|s| *s as f32 / i16::MAX as f32),
                    cap_limit,
                )
            },
            err_cb,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _| {
                push(
                    &buf_cb,
                    data.iter().map(|s| (*s as f32 / u16::MAX as f32) * 2.0 - 1.0),
                    cap_limit,
                )
            },
            err_cb,
            None,
        )?,
        other => return Err(anyhow!("unsupported sample format: {other:?}")),
    };

    stream.play().context("start input stream")?;

    Ok(Active {
        stream,
        buffer,
        channels,
        sample_rate,
    })
}

fn open_mic_stream(device_name: &str) -> Result<Active> {
    let host = cpal::default_host();
    let device = if device_name.is_empty() {
        host.default_input_device()
            .ok_or_else(|| anyhow!("no default input device"))?
    } else {
        host.input_devices()?
            .find(|d| d.name().map(|n| n == device_name).unwrap_or(false))
            .or_else(|| host.default_input_device())
            .ok_or_else(|| anyhow!("input device '{device_name}' not found"))?
    };

    let config = device
        .default_input_config()
        .context("query default input config")?;
    build_active_from_config(&device, config, "mic")
}

/// System-audio capture: opens an OUTPUT device and reads it as input.
/// cpal transparently sets the WASAPI loopback flag when it sees the
/// device's data flow is render, not capture — see the module doc comment.
/// The device is queried via `default_output_config()` (NOT
/// `default_input_config()`, which fails on a render device).
fn open_loopback_stream(device_name: &str) -> Result<Active> {
    let host = cpal::default_host();
    let device = if device_name.is_empty() {
        host.default_output_device()
            .ok_or_else(|| anyhow!("no default output device"))?
    } else {
        host.output_devices()?
            .find(|d| d.name().map(|n| n == device_name).unwrap_or(false))
            .or_else(|| host.default_output_device())
            .ok_or_else(|| anyhow!("output device '{device_name}' not found"))?
    };

    let config = device
        .default_output_config()
        .context("query default output config for loopback")?;
    build_active_from_config(&device, config, "loopback")
}

fn finish(raw: Vec<f32>, channels: u16, sample_rate: u32) -> Result<Vec<f32>> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let mono = downmix(&raw, channels);
    if sample_rate == TARGET_SR {
        Ok(mono)
    } else {
        resample(&mono, sample_rate, TARGET_SR)
    }
}

fn push<I: Iterator<Item = f32>>(
    buf: &std::sync::Arc<Mutex<Vec<f32>>>,
    samples: I,
    cap_limit: usize,
) {
    let Ok(mut b) = buf.lock() else { return };
    if b.len() >= cap_limit {
        return;
    }
    b.extend(samples);
}

fn downmix(interleaved: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let ch = channels as usize;
    let mut out = Vec::with_capacity(interleaved.len() / ch);
    for frame in interleaved.chunks_exact(ch) {
        out.push(frame.iter().sum::<f32>() / ch as f32);
    }
    out
}

fn resample(input: &[f32], from: u32, to: u32) -> Result<Vec<f32>> {
    use rubato::{
        Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
        WindowFunction,
    };

    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };
    let ratio = to as f64 / from as f64;
    let chunk = 1024usize;
    let mut resampler =
        SincFixedIn::<f32>::new(ratio, 2.0, params, chunk, 1).context("build resampler")?;

    let mut out = Vec::with_capacity((input.len() as f64 * ratio) as usize + chunk);
    let mut pos = 0;
    while pos + chunk <= input.len() {
        let frames = resampler.process(&[&input[pos..pos + chunk]], None)?;
        out.extend_from_slice(&frames[0]);
        pos += chunk;
    }
    if pos < input.len() {
        let mut last = vec![0.0f32; chunk];
        last[..input.len() - pos].copy_from_slice(&input[pos..]);
        let frames = resampler.process(&[&last], None)?;
        let keep = ((input.len() - pos) as f64 * ratio).round() as usize;
        out.extend_from_slice(&frames[0][..keep.min(frames[0].len())]);
    }
    Ok(out)
}

/// Names of available input (microphone) devices, for the settings dropdown.
pub fn list_input_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for d in devices {
            if let Ok(name) = d.name() {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

/// Names of available output (playback) devices, for picking which one to
/// loopback-capture — useful when the user has more than one (e.g. speakers
/// plus a virtual cable).
pub fn list_output_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    if let Ok(devices) = host.output_devices() {
        for d in devices {
            if let Ok(name) = d.name() {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_48k_to_16k_thirds_the_length() {
        let src: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.01).sin()).collect();
        let out = resample(&src, 48_000, 16_000).unwrap();
        let expected = 16_000isize;
        assert!(
            (out.len() as isize - expected).abs() < 400,
            "got {} samples, expected ~{expected}",
            out.len()
        );
    }

    #[test]
    fn downmix_stereo_averages_channels() {
        let stereo = [1.0, -1.0, 0.5, 0.5];
        let mono = downmix(&stereo, 2);
        assert_eq!(mono, vec![0.0, 0.5]);
    }
}
