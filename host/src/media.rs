use bytes::Bytes;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::sync::broadcast;

/// One encoded access unit in Annex-B form (start codes), as produced by the encoder.
pub struct VideoFrame {
    pub data: Bytes,
    pub duration: Duration,
    pub keyframe: bool,
}

/// One 20 ms Opus packet.
pub struct AudioFrame {
    pub data: Bytes,
    pub duration: Duration,
}

/// Fan-out from the single encoder to every connected viewer.
#[derive(Clone)]
pub struct Media {
    pub frames: broadcast::Sender<Arc<VideoFrame>>,
    pub audio: broadcast::Sender<Arc<AudioFrame>>,
    /// Set when a viewer needs an IDR frame to start decoding; the encoder clears it.
    pub want_keyframe: Arc<AtomicBool>,
}

impl Media {
    pub fn new() -> Self {
        Self {
            frames: broadcast::channel(8).0,
            audio: broadcast::channel(16).0,
            want_keyframe: Arc::new(AtomicBool::new(false)),
        }
    }
}
