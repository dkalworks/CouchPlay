//! System audio: WASAPI loopback of the default output -> 48 kHz stereo -> 20 ms Opus packets.

use crate::media::{AudioFrame, Media};
use crate::proc_loopback;
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};

const RATE: u32 = 48_000;
const FRAME: usize = 960; // 20 ms
const MAX_BACKLOG_FRAMES: usize = FRAME * 3;

pub fn spawn(media: Media, kbps: u32, exclude: Vec<String>) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new().name("audio".into()).spawn(move || {
        loop {
            // Capture ends when the output device changes, the excluded app starts or exits,
            // or the device goes away; set it up again.
            if let Err(e) = run(&media, kbps, &exclude) {
                warn!("audio capture stopped: {e:#}");
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SampleFormat {
    F32,
    I32,
    I16,
}

/// `subtype` is the first dword of a WAVE_FORMAT_EXTENSIBLE SubFormat GUID (1 = PCM, 3 = float).
fn sample_format(tag: u16, bits: u16, subtype: u32) -> Option<SampleFormat> {
    let kind = if tag == 0xFFFE { subtype } else { tag as u32 };
    match (kind, bits) {
        (3, 32) => Some(SampleFormat::F32),
        (1, 32) => Some(SampleFormat::I32),
        (1, 16) => Some(SampleFormat::I16),
        _ => None,
    }
}

/// Reads `frames` interleaved frames from a WASAPI buffer as stereo f32.
unsafe fn read_stereo(data: *const u8, frames: usize, channels: usize, fmt: SampleFormat, out: &mut Vec<[f32; 2]>) {
    let pick = |f: &[f32]| if channels == 1 { [f[0], f[0]] } else { [f[0], f[1]] };
    let mut frame = vec![0f32; channels];
    for i in 0..frames {
        for (c, v) in frame.iter_mut().enumerate() {
            let idx = i * channels + c;
            *v = match fmt {
                SampleFormat::F32 => unsafe { (data as *const f32).add(idx).read_unaligned() },
                SampleFormat::I32 => (unsafe { (data as *const i32).add(idx).read_unaligned() }) as f32 / 2_147_483_648.0,
                SampleFormat::I16 => (unsafe { (data as *const i16).add(idx).read_unaligned() }) as f32 / 32_768.0,
            };
        }
        out.push(pick(&frame));
    }
}

/// Loopback is captured after Windows' volume stage, so a quiet system volume would give viewers
/// a quiet stream. Undo it: they get the game's own level and set their own volume.
fn undo_master_volume(db: f32) -> f32 {
    10f32.powf(-db / 20.0).clamp(1.0, 1000.0)
}

/// Linear-interpolation resampler for interleaved stereo.
struct Resampler {
    step: f64, // input frames consumed per output frame
    pos: f64,  // position in the current input block, relative to its first frame
    prev: [f32; 2],
}

impl Resampler {
    fn new(in_rate: u32) -> Self {
        Self { step: in_rate as f64 / RATE as f64, pos: 0.0, prev: [0.0; 2] }
    }

    fn process(&mut self, input: &[[f32; 2]], out: &mut VecDeque<f32>) {
        if input.is_empty() {
            return;
        }
        if (self.step - 1.0).abs() < 1e-9 {
            for f in input {
                out.extend(f);
            }
            return;
        }
        // Frame -1 is the last frame of the previous block.
        let at = |i: i64| if i < 0 { self.prev } else { input[i as usize] };
        while self.pos < input.len() as f64 - 1.0 {
            let i = self.pos.floor() as i64;
            let t = (self.pos - i as f64) as f32;
            let (a, b) = (at(i), at(i + 1));
            out.push_back(a[0] + (b[0] - a[0]) * t);
            out.push_back(a[1] + (b[1] - a[1]) * t);
            self.pos += self.step;
        }
        self.pos -= input.len() as f64;
        self.prev = input[input.len() - 1];
    }
}

struct Source {
    client: IAudioClient,
    rate: u32,
    channels: usize,
    format: SampleFormat,
    /// Present only for plain device loopback, which is captured after the volume stage.
    volume: Option<IAudioEndpointVolume>,
}

/// Everything except `excluded` (a process tree), as 48 kHz float stereo.
unsafe fn open_excluding(excluded: u32) -> Result<Source> {
    let client = proc_loopback::client_excluding(excluded)?;
    let fmt = WAVEFORMATEX {
        wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
        nChannels: 2,
        nSamplesPerSec: RATE,
        nAvgBytesPerSec: RATE * 8,
        nBlockAlign: 8,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            2_000_000,
            0,
            &fmt,
            None,
        )?;
    }
    Ok(Source { client, rate: RATE, channels: 2, format: SampleFormat::F32, volume: None })
}

/// Loopback of the default output device (includes every app).
unsafe fn open_default_output() -> Result<Source> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).context("no default audio output")?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;

        let fmt = client.GetMixFormat()?;
        let (rate, channels, bits, tag) = (
            std::ptr::addr_of!((*fmt).nSamplesPerSec).read_unaligned(),
            std::ptr::addr_of!((*fmt).nChannels).read_unaligned() as usize,
            std::ptr::addr_of!((*fmt).wBitsPerSample).read_unaligned(),
            std::ptr::addr_of!((*fmt).wFormatTag).read_unaligned(),
        );
        // WAVE_FORMAT_EXTENSIBLE carries the real sample type in its SubFormat GUID (offset 24).
        let subtype = if tag == 0xFFFE { (fmt as *const u8).add(24).cast::<u32>().read_unaligned() } else { 0 };
        let Some(format) = sample_format(tag, bits, subtype) else {
            CoTaskMemFree(Some(fmt as *const _));
            bail!("unsupported mix format: {bits}-bit, tag {tag:#x}, subtype {subtype:#x}");
        };
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 2_000_000, 0, fmt, None);
        CoTaskMemFree(Some(fmt as *const _));
        init?;
        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
        Ok(Source { client, rate, channels, format, volume: Some(volume) })
    }
}

fn run(media: &Media, kbps: u32, exclude: &[String]) -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let excluded = if exclude.is_empty() { None } else { proc_loopback::find_root_pid(exclude) };
        let (source, mode) = match excluded.map(|pid| open_excluding(pid)) {
            Some(Ok(s)) => (s, format!("everything except process tree {}", excluded.unwrap())),
            other => {
                if let Some(Err(e)) = other {
                    warn!("could not exclude the chat app from audio capture ({e:#}); capturing all system audio");
                }
                (open_default_output()?, "all system audio".to_string())
            }
        };
        let Source { client, rate, channels, format: sample_fmt, volume } = source;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        info!("capturing {mode}: {rate} Hz, {channels} ch, {sample_fmt:?} -> Opus {kbps} kbps");

        let mut opus = opus::Encoder::new(RATE, opus::Channels::Stereo, opus::Application::LowDelay)?;
        opus.set_bitrate(opus::Bitrate::Bits(kbps as i32 * 1000))?;

        let mut gain = 1.0f32;
        let mut gain_checked = Instant::now() - Duration::from_secs(1);
        let mut exclusion_checked = Instant::now();

        let mut resampler = Resampler::new(rate);
        let mut queue: VecDeque<f32> = VecDeque::new();
        let mut block: Vec<[f32; 2]> = Vec::new();
        let mut pcm = vec![0f32; FRAME * 2];
        let mut packet = vec![0u8; 4000];
        let (mut peak, mut captured, mut peak_since) = (0f32, 0u64, Instant::now());

        let step = Duration::from_millis(20);
        let mut next = Instant::now() + step;
        loop {
            // Drain everything WASAPI has for us.
            while capture.GetNextPacketSize()? > 0 {
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                block.clear();
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    block.resize(frames as usize, [0.0; 2]);
                } else {
                    read_stereo(data, frames as usize, channels, sample_fmt, &mut block);
                }
                capture.ReleaseBuffer(frames)?;
                if let Some(volume) = &volume {
                    if gain_checked.elapsed() >= Duration::from_millis(500) {
                        gain = volume.GetMasterVolumeLevel().map_or(1.0, undo_master_volume);
                        gain_checked = Instant::now();
                    }
                    if gain != 1.0 {
                        for f in block.iter_mut().flatten() {
                            *f = (*f * gain).clamp(-1.0, 1.0);
                        }
                    }
                }
                captured += frames as u64;
                peak = block.iter().flatten().fold(peak, |m, s| m.max(s.abs()));
                resampler.process(&block, &mut queue);
            }
            let cap = MAX_BACKLOG_FRAMES * 2;
            if queue.len() > cap {
                queue.drain(..queue.len() - cap);
            }

            if peak_since.elapsed() >= Duration::from_secs(2) {
                tracing::debug!("audio: {captured} frames captured, peak level {peak:.3}");
                (peak, captured, peak_since) = (0.0, 0, Instant::now());
            }
            if !exclude.is_empty() && exclusion_checked.elapsed() >= Duration::from_secs(5) {
                exclusion_checked = Instant::now();
                if proc_loopback::find_root_pid(exclude) != excluded {
                    info!("chat app started, stopped or restarted; reconfiguring audio capture");
                    let _ = client.Stop();
                    return Ok(());
                }
            }

            let now = Instant::now();
            if now < next {
                std::thread::sleep((next - now).min(Duration::from_millis(4)));
                continue;
            }
            next += step;
            if now > next + step * 4 {
                next = now + step;
            }

            if media.audio.receiver_count() == 0 {
                queue.clear();
                continue;
            }
            // Nothing playing means WASAPI sends no packets; pad with silence to keep the clock.
            let have = queue.len().min(FRAME * 2);
            for (dst, src) in pcm.iter_mut().zip(queue.drain(..have)) {
                *dst = src;
            }
            pcm[have..].fill(0.0);
            let n = opus.encode_float(&pcm, &mut packet)?;
            let _ = media.audio.send(Arc::new(AudioFrame {
                data: Bytes::copy_from_slice(&packet[..n]),
                duration: step,
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undoes_volume_attenuation() {
        assert!((undo_master_volume(-20.0) - 10.0).abs() < 1e-4);
        assert_eq!(undo_master_volume(0.0), 1.0);
        assert_eq!(undo_master_volume(-200.0), 1000.0);
        assert_eq!(undo_master_volume(6.0), 1.0);
    }

    #[test]
    fn detects_sample_formats() {
        assert_eq!(sample_format(3, 32, 0), Some(SampleFormat::F32));
        assert_eq!(sample_format(0xFFFE, 32, 3), Some(SampleFormat::F32));
        assert_eq!(sample_format(0xFFFE, 32, 1), Some(SampleFormat::I32));
        assert_eq!(sample_format(1, 16, 0), Some(SampleFormat::I16));
        assert_eq!(sample_format(0xFFFE, 24, 1), None);
    }

    #[test]
    fn converts_integer_samples() {
        let data: [i32; 4] = [i32::MAX, i32::MIN, 0, 1 << 30];
        let mut out = Vec::new();
        unsafe { read_stereo(data.as_ptr() as *const u8, 2, 2, SampleFormat::I32, &mut out) };
        assert!((out[0][0] - 1.0).abs() < 1e-6 && (out[0][1] + 1.0).abs() < 1e-6);
        assert_eq!(out[1], [0.0, 0.5]);
    }

    #[test]
    fn passthrough_at_48k() {
        let mut r = Resampler::new(48_000);
        let mut out = VecDeque::new();
        r.process(&[[0.5, -0.5]; 4], &mut out);
        assert_eq!(out.len(), 8);
    }

    #[test]
    fn resamples_44k1_to_48k_length() {
        let mut r = Resampler::new(44_100);
        let mut out = VecDeque::new();
        let block = vec![[0.25f32, 0.25]; 441];
        for _ in 0..100 {
            r.process(&block, &mut out);
        }
        let frames = out.len() / 2;
        // 100 blocks of 10 ms -> about one second of output.
        assert!((47_900..=48_100).contains(&frames), "{frames}");
        assert!(out.iter().all(|s| (*s - 0.25).abs() < 1e-6));
    }
}

#[cfg(test)]
mod diagnostics {
    use super::*;
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::System::Com::STGM_READ;

    fn name_of(d: &IMMDevice) -> String {
        unsafe {
            let store = d.OpenPropertyStore(STGM_READ).unwrap();
            let v = store.GetValue(&PKEY_Device_FriendlyName).unwrap();
            v.to_string()
        }
    }

    /// Run while something is playing: `cargo test list_endpoints -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn list_endpoints() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap();
            let default = en.GetDefaultAudioEndpoint(eRender, eConsole).unwrap();
            let default_comm = en.GetDefaultAudioEndpoint(eRender, eCommunications).unwrap();
            println!("default (console): {}", name_of(&default));
            println!("default (communications): {}", name_of(&default_comm));
            let all = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE).unwrap();
            let meters: Vec<_> = (0..all.GetCount().unwrap())
                .map(|i| {
                    let d = all.Item(i).unwrap();
                    let m: windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation =
                        d.Activate(CLSCTX_ALL, None).unwrap();
                    (name_of(&d), m)
                })
                .collect();
            let mut max = vec![0f32; meters.len()];
            for _ in 0..60 {
                for (i, (_, m)) in meters.iter().enumerate() {
                    max[i] = max[i].max(m.GetPeakValue().unwrap_or(0.0));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            for ((name, _), peak) in meters.iter().zip(max) {
                println!("endpoint {name:50} peak {peak:.3}");
            }
        }
    }
}
