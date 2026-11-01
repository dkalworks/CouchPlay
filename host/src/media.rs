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

/// Fan-out from the single encoder to every connected viewer.
#[derive(Clone)]
pub struct Media {
    pub frames: broadcast::Sender<Arc<VideoFrame>>,
    /// Set when a viewer needs an IDR frame to start decoding; the encoder clears it.
    pub want_keyframe: Arc<AtomicBool>,
}

impl Media {
    pub fn new() -> Self {
        Self {
            frames: broadcast::channel(8).0,
            want_keyframe: Arc::new(AtomicBool::new(false)),
        }
    }
}
