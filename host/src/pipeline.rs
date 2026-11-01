//! Capture + encode thread: samples the latest screen frame at a fixed rate and publishes
//! encoded H.264 to every viewer through `Media`.

use crate::capture::{Capture, Source};
use crate::encoder::Encoder;
use crate::gpu;
use crate::media::{Media, VideoFrame};
use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tracing::{info, warn};

pub struct VideoSettings {
    pub fps: u32,
    pub kbps: u32,
    /// Stream height; width follows the monitor's aspect ratio. 0 keeps the native size.
    pub height: u32,
    pub source: Source,
}

pub fn spawn(media: Media, settings: VideoSettings) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new().name("video".into()).spawn(move || {
        if let Err(e) = run(media, settings) {
            warn!("video pipeline stopped: {e:?}");
        }
    })
}

fn even(v: u32) -> u32 {
    v & !1
}

/// Keeps trying until the source exists, so the host can be started before the game.
fn wait_for_capture(gpu: &gpu::Gpu, source: &Source) -> Capture {
    let mut announced = false;
    loop {
        match Capture::start(gpu, source) {
            Ok(c) => return c,
            Err(e) => {
                if !announced {
                    warn!("waiting for {}: {e:#}", source.describe());
                    announced = true;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn run(media: Media, s: VideoSettings) -> Result<()> {
    let gpu = gpu::create()?;
    let mut capture = wait_for_capture(&gpu, &s.source);

    let (cw, ch) = capture.size;
    let (out_w, out_h) = if s.height == 0 || s.height >= ch {
        (even(cw), even(ch))
    } else {
        (even(cw * s.height / ch), even(s.height))
    };
    let mut encoder = Encoder::new(&gpu, (out_w, out_h), s.fps, s.kbps)?;
    info!(
        "capturing {} ({cw}x{ch}), streaming {out_w}x{out_h}@{} at {} kbps via {}",
        s.source.describe(), s.fps, s.kbps, encoder.name
    );

    let step = Duration::from_nanos(1_000_000_000 / s.fps as u64);
    let mut next = Instant::now();
    let (mut sent, mut bytes, mut since) = (0u32, 0usize, Instant::now());
    loop {
        next += step;
        let now = Instant::now();
        if next > now {
            let wait = next - now;
            if wait > Duration::from_millis(2) {
                std::thread::sleep(wait - Duration::from_millis(1));
            }
            while Instant::now() < next {
                std::hint::spin_loop();
            }
        } else if now - next > step * 4 {
            next = now; // fell far behind; don't try to catch up
        }

        if capture.is_closed() {
            warn!("{} went away; waiting for it to come back", s.source.describe());
            drop(capture);
            capture = wait_for_capture(&gpu, &s.source);
            info!("capturing {} again", s.source.describe());
            media.want_keyframe.store(true, Ordering::Relaxed);
            next = Instant::now();
            continue;
        }
        if media.frames.receiver_count() == 0 {
            continue;
        }
        if media.want_keyframe.swap(false, Ordering::Relaxed) {
            encoder.request_keyframe();
        }

        let texture = match capture.latest.lock().unwrap().as_ref() {
            Some(l) => l.texture.clone(),
            None => continue,
        };
        for e in encoder.encode(&texture)? {
            sent += 1;
            bytes += e.data.len();
            let _ = media.frames.send(Arc::new(VideoFrame {
                data: e.data,
                duration: step,
                keyframe: e.keyframe,
            }));
        }

        if since.elapsed() >= Duration::from_secs(2) {
            let secs = since.elapsed().as_secs_f64();
            info!("video: {:.0} fps, {:.1} Mbit/s", sent as f64 / secs, bytes as f64 * 8.0 / secs / 1e6);
            (sent, bytes, since) = (0, 0, Instant::now());
        }
    }
}
