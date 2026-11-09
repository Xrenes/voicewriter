//! Microphone + system-audio (WASAPI loopback) capture via cpal.
//!
//! `cpal::Stream` is not `Send`, so it cannot live in Tauri's managed state.
//! Each `Recorder` owns a dedicated OS thread that owns its stream and takes
//! commands over a channel. The device callback only down-mixes to mono and
//! queues the samples; resampling to 16 kHz (whisper's input format) happens
//! on the recorder thread.
//!
//! Two capture modes:
//! - memory (`start`/`stop`): hold-to-talk dictation. Samples come back as a
//!   `Vec<f32>`, capped at `MAX_MEMORY_SECONDS`.
//! - file (`start_to_file`/`stop_file`): call recording. Samples are
//!   resampled and appended to a 16 kHz mono WAV as they arrive, so memory
//!   stays flat however long the call runs.
//!
//! System audio is WASAPI loopback: cpal opens an OUTPUT device as an input
//! stream and sets `AUDCLNT_STREAMFLAGS_LOOPBACK` itself; the format must come
//! from `default_output_config()` (`default_input_config()` fails on a render
//! device). Loopback delivers no packets at all while nothing is playing, so
//! file-mode loopback fills those gaps with silence from the wall clock —
//! otherwise the device track would shrink and drift out of sync with the mic.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Whisper expects 16 kHz mono f32 PCM.
pub const TARGET_SR: u32 = 16_000;
/// Safety cap so a forgotten dictation session can't grow without bound.
const MAX_MEMORY_SECONDS: u64 = 240;
/// Upper bound for a single call recording.
const MAX_FILE_SECONDS: u64 = 4 * 60 * 60;
/// How often file-mode capture moves queued samples into the WAV file.
const DRAIN_INTERVAL: Duration = Duration::from_millis(250);
/// Rewrite the WAV header this often so a crash still leaves a playable file.
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
/// Loopback packets can arrive this late from ordinary scheduling jitter
/// before the gap is treated as "nothing was playing" and filled.
const GAP_TOLERANCE_SECS: f64 = 0.2;
const RESAMPLE_CHUNK: usize = 1024;

/// Which side of the audio path a `Recorder` captures.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Mic,
    Loopback,
}

/// File-mode capture: where to write, and the instant the recording's
/// timeline starts at. The mic and loopback tracks of one call share an
/// origin so their samples line up.
pub struct FileTarget {
    pub path: PathBuf,
    pub origin: Instant,
}

enum Cmd {
    Start { device: String, file: Option<FileTarget>, reply: Sender<Result<()>> },
    Stop { reply: Sender<Result<Captured>> },
}

enum Captured {
    Samples(Vec<f32>),
    File { samples: u64 },
}

pub struct Recorder {
    tx: Sender<Cmd>,
    recording: AtomicBool,
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
        Self { tx, recording: AtomicBool::new(false) }
    }

    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::SeqCst)
    }

    /// Start an in-memory capture (dictation). Starting while already
    /// capturing is a no-op.
    pub fn start(&self, device_name: &str) -> Result<()> {
        self.send_start(device_name, None)
    }

    /// Start capturing straight into a 16 kHz mono WAV file.
    pub fn start_to_file(&self, device_name: &str, target: FileTarget) -> Result<()> {
        self.send_start(device_name, Some(target))
    }

    /// Stop an in-memory capture and return its 16 kHz mono samples.
    pub fn stop(&self) -> Result<Vec<f32>> {
        match self.send_stop()? {
            Captured::Samples(s) => Ok(s),
            Captured::File { .. } => Err(anyhow!("this recorder was writing to a file")),
        }
    }

    /// Stop a file capture, finalize the WAV, and return how many 16 kHz
    /// samples it holds (0 if nothing was being captured).
    pub fn stop_file(&self) -> Result<u64> {
        match self.send_stop()? {
            Captured::File { samples } => Ok(samples),
            Captured::Samples(s) if s.is_empty() => Ok(0),
            Captured::Samples(_) => Err(anyhow!("this recorder was not writing to a file")),
        }
    }

    fn send_start(&self, device: &str, file: Option<FileTarget>) -> Result<()> {
        let (reply, resp) = mpsc::channel();
        self.tx
            .send(Cmd::Start { device: device.to_string(), file, reply })
            .map_err(|_| anyhow!("audio thread gone"))?;
        let r = resp.recv().map_err(|_| anyhow!("audio thread gone"))?;
        if r.is_ok() {
            self.recording.store(true, Ordering::SeqCst);
        }
        r
    }

    fn send_stop(&self) -> Result<Captured> {
        let (reply, resp) = mpsc::channel();
        self.tx.send(Cmd::Stop { reply }).map_err(|_| anyhow!("audio thread gone"))?;
        self.recording.store(false, Ordering::SeqCst);
        resp.recv().map_err(|_| anyhow!("audio thread gone"))?
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GapFill {
    /// Dictation: keep exactly what the device delivered.
    None,
    /// Call-recording mic: pad only the start-up delay so the track begins
    /// at the shared origin.
    Leading,
    /// Loopback: also fill every stretch where nothing was playing.
    Full,
}

#[derive(Default)]
struct Queue {
    /// Mono samples at the device rate, not yet consumed by the recorder thread.
    samples: Vec<f32>,
    /// Total mono frames queued since the capture started, including filled silence.
    frames: u64,
    started: bool,
}

/// The device callback's side of a capture: down-mixes each packet to mono,
/// fills timeline gaps per `fill`, and queues the result.
#[derive(Clone)]
struct Sink {
    queue: Arc<Mutex<Queue>>,
    channels: usize,
    rate: f64,
    cap_frames: u64,
    fill: GapFill,
    origin: Instant,
}

impl Sink {
    fn new(channels: usize, rate: u32, cap_secs: u64, fill: GapFill, origin: Instant) -> Self {
        Self {
            queue: Arc::new(Mutex::new(Queue::default())),
            channels: channels.max(1),
            rate: rate as f64,
            cap_frames: cap_secs * rate as u64,
            fill,
            origin,
        }
    }

    fn push<T: Copy>(&self, data: &[T], convert: impl Fn(T) -> f32) {
        let frames = data.len() / self.channels;
        if frames == 0 {
            return;
        }
        let elapsed = Instant::now().saturating_duration_since(self.origin).as_secs_f64();
        let mut q = self.queue.lock();
        if q.frames >= self.cap_frames {
            return;
        }
        let first = !q.started;
        q.started = true;
        let pad = match self.fill {
            GapFill::None => 0,
            GapFill::Leading if !first => 0,
            _ => {
                // The packet ends roughly now; anything missing before it is
                // time the stream wasn't delivering. The first packet is
                // padded exactly (start-up delay), later ones only past jitter.
                let tolerance = if first { 0.0 } else { GAP_TOLERANCE_SECS };
                silence_deficit(elapsed, self.rate, q.frames + frames as u64, tolerance)
            }
        };
        let pad = pad.min(self.cap_frames - q.frames);
        if pad > 0 {
            let len = q.samples.len();
            q.samples.resize(len + pad as usize, 0.0);
            q.frames += pad;
        }
        let take = frames.min((self.cap_frames - q.frames) as usize);
        let ch = self.channels;
        q.samples.reserve(take);
        for frame in data.chunks_exact(ch).take(take) {
            let sum: f32 = frame.iter().map(|&s| convert(s)).sum();
            q.samples.push(sum / ch as f32);
        }
        q.frames += take as u64;
    }
}

/// Frames of silence needed so a stream that has delivered `delivered`
/// frames after `elapsed_secs` lines up with the wall clock again, or 0 while
/// the shortfall is within `tolerance_secs`.
fn silence_deficit(elapsed_secs: f64, rate: f64, delivered: u64, tolerance_secs: f64) -> u64 {
    let missing = elapsed_secs * rate - delivered as f64;
    if missing > tolerance_secs * rate {
        missing as u64
    } else {
        0
    }
}

/// What's producing samples into the `Sink`. Dropping it stops capture.
enum Capture {
    Cpal(#[allow(dead_code)] cpal::Stream),
    /// Linux system audio: a `parec` subprocess recording the default sink's
    /// monitor source, plus the thread copying its stdout into the sink.
    #[cfg(target_os = "linux")]
    Process(std::process::Child, Option<std::thread::JoinHandle<()>>),
}

impl Drop for Capture {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        if let Capture::Process(child, reader) = self {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(r) = reader.take() {
                let _ = r.join();
            }
        }
    }
}

type WavFile = hound::WavWriter<std::io::BufWriter<std::fs::File>>;

enum Output {
    Memory,
    File { writer: WavFile, resampler: StreamResampler, written: u64, last_flush: Instant },
}

struct Active {
    capture: Capture,
    sink: Sink,
    rate: u32,
    output: Output,
}

impl Active {
    fn open(source: Source, device: &str, file: Option<FileTarget>) -> Result<Self> {
        let (fill, cap_secs, origin) = match (&file, source) {
            (None, _) => (GapFill::None, MAX_MEMORY_SECONDS, Instant::now()),
            (Some(t), Source::Mic) => (GapFill::Leading, MAX_FILE_SECONDS, t.origin),
            (Some(t), Source::Loopback) => (GapFill::Full, MAX_FILE_SECONDS, t.origin),
        };
        let writer = match &file {
            Some(t) => {
                let spec = hound::WavSpec {
                    channels: 1,
                    sample_rate: TARGET_SR,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                };
                Some(
                    hound::WavWriter::create(&t.path, spec)
                        .with_context(|| format!("create {}", t.path.display()))?,
                )
            }
            None => None,
        };
        let (capture, sink, rate) = match source {
            Source::Mic => open_mic_stream(device, fill, cap_secs, origin)?,
            Source::Loopback => open_loopback_stream(device, fill, cap_secs, origin)?,
        };
        let output = match writer {
            Some(writer) => Output::File {
                writer,
                resampler: StreamResampler::new(rate, TARGET_SR)?,
                written: 0,
                last_flush: Instant::now(),
            },
            None => Output::Memory,
        };
        Ok(Self { capture, sink, rate, output })
    }

    fn drain_to_file(&mut self) -> Result<()> {
        let Output::File { writer, resampler, written, last_flush } = &mut self.output else {
            return Ok(());
        };
        let queued = std::mem::take(&mut self.sink.queue.lock().samples);
        if !queued.is_empty() {
            let mut out = Vec::with_capacity(queued.len() / 2 + RESAMPLE_CHUNK);
            resampler.push(&queued, &mut out)?;
            write_samples(writer, &out)?;
            *written += out.len() as u64;
        }
        if last_flush.elapsed() >= FLUSH_INTERVAL {
            writer.flush().context("flush recording file")?;
            *last_flush = Instant::now();
        }
        Ok(())
    }

    fn finish(self) -> Result<Captured> {
        let Active { capture, sink, rate, output } = self;
        let stopped_at = Instant::now();
        drop(capture); // stop the device callback before taking the last samples
        let mut q = std::mem::take(&mut *sink.queue.lock());
        if sink.fill == GapFill::Full {
            // Nothing playing at the very end still counts as recorded time.
            let elapsed = stopped_at.saturating_duration_since(sink.origin).as_secs_f64();
            let pad = silence_deficit(elapsed, sink.rate, q.frames, GAP_TOLERANCE_SECS)
                .min(sink.cap_frames.saturating_sub(q.frames));
            let len = q.samples.len();
            q.samples.resize(len + pad as usize, 0.0);
        }
        match output {
            Output::Memory => Ok(Captured::Samples(resample(&q.samples, rate, TARGET_SR)?)),
            Output::File { mut writer, mut resampler, written, .. } => {
                let mut out = Vec::with_capacity(q.samples.len() / 2 + RESAMPLE_CHUNK);
                resampler.push(&q.samples, &mut out)?;
                resampler.flush(&mut out)?;
                write_samples(&mut writer, &out)?;
                writer.finalize().context("finalize recording file")?;
                Ok(Captured::File { samples: written + out.len() as u64 })
            }
        }
    }
}

fn write_samples(writer: &mut WavFile, samples: &[f32]) -> Result<()> {
    for &s in samples {
        writer
            .write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .context("write recording file")?;
    }
    Ok(())
}

fn audio_thread(rx: Receiver<Cmd>, source: Source) {
    let mut active: Option<Active> = None;
    loop {
        let writing = matches!(active.as_ref().map(|a| &a.output), Some(Output::File { .. }));
        let cmd = if writing {
            match rx.recv_timeout(DRAIN_INTERVAL) {
                Ok(cmd) => cmd,
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(a) = active.as_mut() {
                        if let Err(e) = a.drain_to_file() {
                            eprintln!("audio: writing recording failed: {e}");
                        }
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(cmd) => cmd,
                Err(_) => break,
            }
        };
        match cmd {
            Cmd::Start { device, file, reply } => {
                if active.is_some() {
                    let _ = reply.send(if file.is_some() { Err(anyhow!("already recording")) } else { Ok(()) });
                    continue;
                }
                match Active::open(source, &device, file) {
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
                let result = match active.take() {
                    Some(a) => a.finish(),
                    None => Ok(Captured::Samples(Vec::new())),
                };
                let _ = reply.send(result);
            }
        }
    }
}

/// Build the cpal input stream shared by mic and loopback capture — the only
/// difference between the two is which device is handed in and which
/// config-query method produced `config` (see callers).
fn build_capture(
    device: &cpal::Device,
    config: cpal::SupportedStreamConfig,
    fill: GapFill,
    cap_secs: u64,
    origin: Instant,
    label: &'static str,
) -> Result<(Capture, Sink, u32)> {
    let channels = config.channels() as usize;
    let rate = config.sample_rate().0;
    let format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();
    let sink = Sink::new(channels, rate, cap_secs, fill, origin);

    let s = sink.clone();
    let err_cb = move |e| eprintln!("{label} stream error: {e}");
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| s.push(data, |v| v),
            err_cb,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| s.push(data, |v| v as f32 / i16::MAX as f32),
            err_cb,
            None,
        )?,
        cpal::SampleFormat::I32 => device.build_input_stream(
            &stream_config,
            move |data: &[i32], _: &cpal::InputCallbackInfo| s.push(data, |v| v as f32 / i32::MAX as f32),
            err_cb,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                s.push(data, |v| (v as f32 / u16::MAX as f32) * 2.0 - 1.0)
            },
            err_cb,
            None,
        )?,
        other => return Err(anyhow!("unsupported {label} sample format: {other:?}")),
    };
    stream.play().context("start input stream")?;
    Ok((Capture::Cpal(stream), sink, rate))
}

fn open_mic_stream(device_name: &str, fill: GapFill, cap_secs: u64, origin: Instant) -> Result<(Capture, Sink, u32)> {
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
    build_capture(&device, config, fill, cap_secs, origin, "mic")
}

/// Linux system audio. There's no WASAPI-style "open the output device as an
/// input" on ALSA; instead PulseAudio and PipeWire (via pipewire-pulse) both
/// expose every output sink's audio as a `<sink>.monitor` source, which
/// `parec` records directly — already 16 kHz mono f32, so nothing is left to
/// convert.
#[cfg(target_os = "linux")]
fn open_loopback_stream(device_name: &str, fill: GapFill, cap_secs: u64, origin: Instant) -> Result<(Capture, Sink, u32)> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let pulse_sink = if device_name.is_empty() {
        crate::linux::default_sink().ok_or_else(|| {
            anyhow!("couldn't find the default audio output (is PulseAudio/PipeWire running, and pulseaudio-utils installed?)")
        })?
    } else {
        device_name.to_string()
    };
    let monitor = format!("{pulse_sink}.monitor");
    let mut child = Command::new("parec")
        .args(["-d", &monitor, "--format=float32le", "--rate=16000", "--channels=1", "--latency-msec=50"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("start parec for system audio (install pulseaudio-utils)")?;

    let sink = Sink::new(1, TARGET_SR, cap_secs, fill, origin);
    let mut stdout = child.stdout.take().ok_or_else(|| anyhow!("no parec stdout"))?;
    let reader_sink = sink.clone();
    let reader = std::thread::Builder::new()
        .name("audio-capture-parec".into())
        .spawn(move || {
            let mut chunk = [0u8; 4096];
            let mut carry: Vec<u8> = Vec::new();
            loop {
                let n = match stdout.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                carry.extend_from_slice(&chunk[..n]);
                let whole = carry.len() / 4 * 4;
                let samples: Vec<f32> = carry[..whole]
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect();
                reader_sink.push(&samples, |v| v);
                carry.drain(..whole);
            }
        })?;

    // parec exits immediately if the monitor source doesn't exist.
    std::thread::sleep(Duration::from_millis(150));
    if let Ok(Some(status)) = child.try_wait() {
        return Err(anyhow!("parec couldn't record '{monitor}' ({status})"));
    }

    Ok((Capture::Process(child, Some(reader)), sink, TARGET_SR))
}

/// System-audio capture: opens an OUTPUT device and reads it as input.
#[cfg(not(target_os = "linux"))]
fn open_loopback_stream(device_name: &str, fill: GapFill, cap_secs: u64, origin: Instant) -> Result<(Capture, Sink, u32)> {
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
    build_capture(&device, config, fill, cap_secs, origin, "loopback")
}

/// Incremental sinc resampler: feed it arbitrary-sized pieces, get output in
/// fixed internal chunks; `flush` handles the final partial chunk.
struct StreamResampler {
    inner: Option<rubato::SincFixedIn<f32>>,
    pending: Vec<f32>,
    ratio: f64,
}

impl StreamResampler {
    fn new(from: u32, to: u32) -> Result<Self> {
        use rubato::{SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
        if from == to {
            return Ok(Self { inner: None, pending: Vec::new(), ratio: 1.0 });
        }
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };
        let ratio = to as f64 / from as f64;
        let inner = SincFixedIn::<f32>::new(ratio, 2.0, params, RESAMPLE_CHUNK, 1).context("build resampler")?;
        Ok(Self { inner: Some(inner), pending: Vec::new(), ratio })
    }

    fn push(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<()> {
        use rubato::Resampler;
        let Some(r) = self.inner.as_mut() else {
            out.extend_from_slice(input);
            return Ok(());
        };
        self.pending.extend_from_slice(input);
        let mut pos = 0;
        while pos + RESAMPLE_CHUNK <= self.pending.len() {
            let frames = r.process(&[&self.pending[pos..pos + RESAMPLE_CHUNK]], None)?;
            out.extend_from_slice(&frames[0]);
            pos += RESAMPLE_CHUNK;
        }
        self.pending.drain(..pos);
        Ok(())
    }

    fn flush(&mut self, out: &mut Vec<f32>) -> Result<()> {
        use rubato::Resampler;
        let Some(r) = self.inner.as_mut() else { return Ok(()) };
        if self.pending.is_empty() {
            return Ok(());
        }
        let rem = self.pending.len();
        let mut last = vec![0.0f32; RESAMPLE_CHUNK];
        last[..rem].copy_from_slice(&self.pending);
        let frames = r.process(&[&last], None)?;
        let keep = (rem as f64 * self.ratio).round() as usize;
        out.extend_from_slice(&frames[0][..keep.min(frames[0].len())]);
        self.pending.clear();
        Ok(())
    }
}

fn resample(input: &[f32], from: u32, to: u32) -> Result<Vec<f32>> {
    let mut r = StreamResampler::new(from, to)?;
    let mut out = Vec::with_capacity((input.len() as f64 * to as f64 / from as f64) as usize + RESAMPLE_CHUNK);
    r.push(input, &mut out)?;
    r.flush(&mut out)?;
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
/// plus a headset). On Linux these are PulseAudio/PipeWire sink names (what
/// `open_loopback_stream` records the `.monitor` of), not ALSA devices.
#[cfg(target_os = "linux")]
pub fn list_output_devices() -> Vec<String> {
    crate::linux::list_sinks()
}

#[cfg(not(target_os = "linux"))]
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
    fn streaming_resample_matches_one_shot() {
        let src: Vec<f32> = (0..30_011).map(|i| (i as f32 * 0.003).sin() * 0.5).collect();
        let whole = resample(&src, 44_100, 16_000).unwrap();
        let mut r = StreamResampler::new(44_100, 16_000).unwrap();
        let mut pieces = Vec::new();
        for part in src.chunks(777) {
            r.push(part, &mut pieces).unwrap();
        }
        r.flush(&mut pieces).unwrap();
        assert_eq!(whole, pieces);
    }

    #[test]
    fn sink_downmixes_stereo_without_padding() {
        let sink = Sink::new(2, 16_000, 10, GapFill::None, Instant::now());
        sink.push(&[1.0f32, -1.0, 0.5, 0.5], |v| v);
        let q = sink.queue.lock();
        assert_eq!(q.samples, vec![0.0, 0.5]);
        assert_eq!(q.frames, 2);
    }

    #[test]
    fn deficit_ignores_jitter_but_fills_real_gaps() {
        // 2 s elapsed at 1 kHz, 1.9 s delivered: inside a 0.2 s tolerance.
        assert_eq!(silence_deficit(2.0, 1000.0, 1900, 0.2), 0);
        // 5 s elapsed, only 2 s delivered: 3 s of silence missing.
        assert_eq!(silence_deficit(5.0, 1000.0, 2000, 0.2), 3000);
        // Ahead of the clock never pads.
        assert_eq!(silence_deficit(1.0, 1000.0, 1500, 0.0), 0);
    }

    #[test]
    fn loopback_sink_pads_time_when_nothing_was_playing() {
        let origin = Instant::now() - Duration::from_secs(1);
        let sink = Sink::new(1, 1000, 10, GapFill::Full, origin);
        sink.push(&[0.25f32; 100], |v| v);
        let q = sink.queue.lock();
        // ~1 s since the origin: ~900 frames of silence, then the 100 delivered.
        assert!((990..=1100).contains(&q.frames), "frames = {}", q.frames);
        assert_eq!(q.samples[0], 0.0);
        assert_eq!(*q.samples.last().unwrap(), 0.25);
    }
}
