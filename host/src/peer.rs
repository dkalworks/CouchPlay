use crate::input::{self, PadState};
use crate::media::Media;
use crate::pad::VirtualPad;
use anyhow::{Context, Result};
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::media_engine::MIME_TYPE_H264;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RtpCodecKind,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
use webrtc::media_stream::Track;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
    RTCConfigurationBuilder, RTCIceCandidateInit, RTCIceServer, RTCPeerConnectionIceEvent,
    RTCPeerConnectionState, RTCSessionDescription, Registry, register_default_interceptors,
};

const H264_FMTP: &str = "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f";

/// State every peer shares with the signaling loop.
pub struct Shared {
    /// Viewer id currently allowed to drive the pad; `None` when the host is playing.
    pub controller: Mutex<Option<String>>,
    pub pad: Mutex<VirtualPad>,
    pub accepted: std::sync::atomic::AtomicU64,
    pub dropped: std::sync::atomic::AtomicU64,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            controller: Mutex::new(None),
            pad: Mutex::new(VirtualPad::new()),
            accepted: Default::default(),
            dropped: Default::default(),
        })
    }
}

pub struct PeerHandle {
    pub name: String,
    pub signals: mpsc::UnboundedSender<Value>,
    /// Highest input sequence applied; the unordered channel may reorder packets.
    pub last_seq: Arc<AtomicI64>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for PeerHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

enum Event {
    Connected,
    Failed,
}

struct Handler {
    id: String,
    out: mpsc::UnboundedSender<Value>,
    events: mpsc::UnboundedSender<Event>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        match event.candidate.to_json() {
            Ok(init) => {
                let _ = self.out.send(
                    json!({ "t": "signal", "to": self.id, "data": { "candidate": init } }),
                );
            }
            Err(e) => warn!("candidate to_json: {e}"),
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        info!(peer = %self.id, "connection state: {state}");
        match state {
            RTCPeerConnectionState::Connected => {
                let _ = self.events.send(Event::Connected);
            }
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                let _ = self.events.send(Event::Failed);
            }
            _ => {}
        }
    }
}

pub fn h264_codec() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_H264.to_owned(),
            clock_rate: 90000,
            channels: 0,
            sdp_fmtp_line: H264_FMTP.to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: 102,
        ..Default::default()
    }
}

pub struct PeerConfig {
    pub ice_servers: Vec<RTCIceServer>,
}

pub fn spawn(
    id: String,
    name: String,
    out: mpsc::UnboundedSender<Value>,
    shared: Arc<Shared>,
    media: Media,
    cfg: Arc<PeerConfig>,
) -> PeerHandle {
    let (signals, signals_rx) = mpsc::unbounded_channel();
    let last_seq = Arc::new(AtomicI64::new(-1));
    let task = tokio::spawn({
        let (id, last_seq) = (id.clone(), last_seq.clone());
        async move {
            if let Err(e) = run(id.clone(), out, signals_rx, shared, media, cfg, last_seq).await {
                warn!(peer = %id, "peer ended: {e:#}");
            }
        }
    });
    PeerHandle { name, signals, last_seq, task }
}

async fn run(
    id: String,
    out: mpsc::UnboundedSender<Value>,
    mut signals: mpsc::UnboundedReceiver<Value>,
    shared: Arc<Shared>,
    media: Media,
    cfg: Arc<PeerConfig>,
    last_seq: Arc<AtomicI64>,
) -> Result<()> {
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let handler = Arc::new(Handler { id: id.clone(), out: out.clone(), events: events_tx });

    let video_codec = h264_codec();
    let mut media_engine = MediaEngine::default();
    media_engine.register_codec(video_codec.clone(), RtpCodecKind::Video)?;
    let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;

    let config = RTCConfigurationBuilder::new().with_ice_servers(cfg.ice_servers.clone()).build();
    let pc = PeerConnectionBuilder::new()
        .with_configuration(config)
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .with_handler(handler)
        .with_runtime(webrtc::runtime::default_runtime().context("no async runtime")?)
        .with_udp_addrs(vec!["0.0.0.0:0".to_string()])
        .build()
        .await?;
    let pc: Arc<dyn PeerConnection> = Arc::new(pc);

    let track = Arc::new(TrackLocalStaticSample::new(
        Instant::now(),
        MediaStreamTrack::new(
            "couchplay".into(),
            "couchplay-video".into(),
            "couchplay-video".into(),
            RtpCodecKind::Video,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(rand_ssrc()),
                    ..Default::default()
                },
                codec: video_codec.rtp_codec.clone(),
                ..Default::default()
            }],
        ),
    )?);
    let sender = pc.add_track(track.clone() as Arc<dyn TrackLocal>).await?;

    // Unordered + no retransmits: a late input packet is worse than a lost one.
    let input_dc = pc
        .create_data_channel(
            "input",
            Some(RTCDataChannelInit {
                ordered: false,
                max_retransmits: Some(0),
                ..Default::default()
            }),
        )
        .await?;
    let ctrl_dc = pc.create_data_channel("ctrl", None).await?;

    tokio::spawn(input_loop(id.clone(), input_dc, shared, last_seq));
    tokio::spawn(ctrl_loop(ctrl_dc));

    let offer = pc.create_offer(None).await?;
    pc.set_local_description(offer).await?;
    if let Some(desc) = pc.local_description().await {
        let _ = out.send(json!({ "t": "signal", "to": id, "data": { "sdp": desc } }));
    }

    let mut video_task: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        tokio::select! {
            Some(data) = signals.recv() => {
                if let Some(sdp) = data.get("sdp") {
                    let desc: RTCSessionDescription = serde_json::from_value(sdp.clone())?;
                    pc.set_remote_description(desc).await?;
                } else if let Some(c) = data.get("candidate") {
                    if c.is_null() { continue; }
                    let init: RTCIceCandidateInit = serde_json::from_value(c.clone())?;
                    if let Err(e) = pc.add_ice_candidate(init).await {
                        debug!(peer = %id, "add_ice_candidate: {e}");
                    }
                }
            }
            Some(event) = events.recv() => match event {
                Event::Connected if video_task.is_none() => {
                    let frames = media.frames.subscribe();
                    media.want_keyframe.store(true, Ordering::Relaxed);
                    video_task = Some(tokio::spawn(video_loop(
                        id.clone(), track.clone(), sender.clone(), frames, media.clone(),
                    )));
                }
                Event::Connected => {}
                Event::Failed => break,
            },
            else => break,
        }
    }
    if let Some(t) = video_task {
        t.abort();
    }
    let _ = pc.close().await;
    Ok(())
}

fn rand_ssrc() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1);
    n ^ std::process::id().rotate_left(16)
}

async fn video_loop(
    id: String,
    track: Arc<TrackLocalStaticSample>,
    sender: Arc<dyn webrtc::rtp_transceiver::RtpSender>,
    mut frames: broadcast::Receiver<Arc<crate::media::VideoFrame>>,
    media: Media,
) {
    let setup = async {
        let ssrc = *track.ssrcs().await.first().context("track has no ssrc")?;
        let pt = sender
            .get_parameters()
            .await?
            .rtp_parameters
            .codecs
            .first()
            .map(|c| c.payload_type)
            .context("no negotiated video codec")?;
        anyhow::Ok((ssrc, pt))
    };
    let (ssrc, pt) = match setup.await {
        Ok(v) => v,
        Err(e) => return warn!(peer = %id, "video setup: {e:#}"),
    };
    // Frames are only decodable from a keyframe on; after a lag, wait for the next one.
    let mut need_key = true;
    loop {
        match frames.recv().await {
            Ok(frame) => {
                if need_key && !frame.keyframe {
                    continue;
                }
                need_key = false;
                let sample = Sample {
                    data: frame.data.clone(),
                    duration: frame.duration,
                    ..Sample::new(Instant::now())
                };
                if let Err(e) = track.sample_writer(ssrc, pt).write_sample(&sample).await {
                    return debug!(peer = %id, "write_sample: {e}");
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                debug!(peer = %id, "video lagged by {n} frames");
                need_key = true;
                media.want_keyframe.store(true, Ordering::Relaxed);
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn input_loop(
    id: String,
    dc: Arc<dyn DataChannel>,
    shared: Arc<Shared>,
    last_seq: Arc<AtomicI64>,
) {
    while let Some(event) = dc.poll().await {
        match event {
            DataChannelEvent::OnMessage(msg) => {
                let allowed = shared.controller.lock().unwrap().as_deref() == Some(id.as_str());
                if !allowed {
                    shared.dropped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let Some(packet) = input::unpack(&msg.data) else { continue };
                // Unordered channel: ignore anything older than what we already applied.
                if i64::from(packet.seq) <= last_seq.load(Ordering::Relaxed) {
                    continue;
                }
                last_seq.store(i64::from(packet.seq), Ordering::Relaxed);
                shared.accepted.fetch_add(1, Ordering::Relaxed);
                let state: PadState = packet.state;
                shared.pad.lock().unwrap().update(&state);
            }
            DataChannelEvent::OnClose => break,
            _ => {}
        }
    }
}

async fn ctrl_loop(dc: Arc<dyn DataChannel>) {
    while let Some(event) = dc.poll().await {
        match event {
            DataChannelEvent::OnMessage(msg) => {
                let Ok(m) = serde_json::from_slice::<Value>(&msg.data) else { continue };
                if m["t"] == "ping" {
                    let pong = json!({ "t": "pong", "ts": m["ts"] }).to_string();
                    let _ = dc.send_text(&pong).await;
                }
            }
            DataChannelEvent::OnClose => break,
            _ => {}
        }
    }
}
