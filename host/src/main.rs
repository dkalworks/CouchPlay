mod audio;
mod capture;
mod encoder;
mod gpu;
mod input;
mod media;
mod pad;
mod peer;
mod pipeline;
mod proc_loopback;

use anyhow::{Context, Result};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};
use webrtc::peer_connection::RTCIceServer;

#[derive(Parser)]
#[command(about = "CouchPlay native host")]
struct Args {
    /// CouchPlay server (ws:// or wss://).
    #[arg(long, env = "COUCHPLAY_SERVER", default_value = "ws://localhost:3001")]
    server: String,
    /// STUN/TURN server URLs, comma separated.
    #[arg(long, env = "COUCHPLAY_ICE", default_value = "stun:stun.l.google.com:19302")]
    ice: String,
    /// Target frame rate.
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// Video bitrate in kbit/s.
    #[arg(long, default_value_t = 12000)]
    kbps: u32,
    /// Audio bitrate in kbit/s.
    #[arg(long, default_value_t = 128)]
    audio_kbps: u32,
    /// Apps whose audio is left out of the stream (comma separated; empty = capture everything).
    /// Keeps Discord voice chat out so friends do not hear themselves.
    #[arg(long, env = "COUCHPLAY_AUDIO_EXCLUDE", default_value = "Discord.exe,DiscordPTB.exe,DiscordCanary.exe")]
    audio_exclude: String,
    /// Don't stream system audio.
    #[arg(long)]
    no_audio: bool,
    /// Capture this monitor (0 = primary). See --list-sources.
    #[arg(long, default_value_t = 0)]
    monitor: usize,
    /// Capture the largest visible window whose title contains this text instead of a monitor.
    #[arg(long)]
    window: Option<String>,
    /// Print the monitors and windows that can be captured, then exit.
    #[arg(long)]
    list_sources: bool,
    /// Stream height in pixels (0 = native resolution).
    #[arg(long, default_value_t = 1080)]
    height: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "couchplay_host=info,warn".into()),
        )
        .init();
    let args = Args::parse();
    if args.list_sources {
        return capture::print_sources();
    }

    let shared = peer::Shared::new();
    let media = media::Media::new();
    pipeline::spawn(
        media.clone(),
        pipeline::VideoSettings {
            fps: args.fps,
            kbps: args.kbps,
            height: args.height,
            source: match args.window {
                Some(title) => capture::Source::Window(title),
                None => capture::Source::Monitor(args.monitor),
            },
        },
    )?;
    if !args.no_audio {
        let exclude = args
            .audio_exclude
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        audio::spawn(media.clone(), args.audio_kbps, exclude)?;
    }
    let cfg = Arc::new(peer::PeerConfig {
        ice_servers: vec![RTCIceServer {
            urls: args.ice.split(',').map(|s| s.trim().to_string()).collect(),
            ..Default::default()
        }],
    });

    let url = format!("{}/api/ws", args.server.trim_end_matches('/'));
    let (ws, _) = connect_async(&url).await.with_context(|| format!("connecting to {url}"))?;
    let (mut ws_tx, mut ws_rx) = ws.split();

    let (out, mut out_rx) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if ws_tx.send(Message::text(msg.to_string())).await.is_err() {
                break;
            }
        }
    });
    out.send(json!({ "t": "host" }))?;

    tokio::spawn({
        let shared = shared.clone();
        async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                let accepted = shared.accepted.swap(0, Ordering::Relaxed);
                let dropped = shared.dropped.swap(0, Ordering::Relaxed);
                if accepted + dropped > 0 {
                    info!("input: {accepted} accepted, {dropped} dropped (not controller)");
                }
            }
        }
    });

    let web_base = args.server.replacen("ws", "http", 1);
    let mut peers: HashMap<String, peer::PeerHandle> = HashMap::new();

    while let Some(msg) = ws_rx.next().await {
        let Message::Text(text) = msg? else { continue };
        let Ok(m) = serde_json::from_str::<Value>(&text) else { continue };
        let str_of = |k: &str| m[k].as_str().map(str::to_owned);

        match m["t"].as_str().unwrap_or_default() {
            "room" => {
                let code = str_of("room").unwrap_or_default();
                info!("party started. Code {code}  ->  {}/?room={code}", web_base.trim_end_matches('/'));
            }
            "state" => {
                let new = str_of("controller");
                let changed = *shared.controller.lock().unwrap() != new;
                if changed {
                    // Hand-off: release everything so no button stays held across owners.
                    shared.pad.lock().unwrap().reset();
                    if let Some(p) = new.as_ref().and_then(|id| peers.get(id)) {
                        p.last_seq.store(-1, Ordering::Relaxed);
                    }
                    info!("controller is now {}", new.as_deref().unwrap_or("the host"));
                }
                *shared.controller.lock().unwrap() = new;
            }
            "peer-joined" => {
                let (Some(id), name) = (str_of("id"), str_of("name").unwrap_or_default()) else {
                    continue;
                };
                info!("{name} joined");
                let handle = peer::spawn(
                    id.clone(),
                    name,
                    out.clone(),
                    shared.clone(),
                    media.clone(),
                    cfg.clone(),
                );
                peers.insert(id, handle);
            }
            "peer-left" => {
                if let Some(p) = str_of("id").and_then(|id| peers.remove(&id)) {
                    info!("{} left", p.name);
                }
            }
            "signal" => {
                if let Some(p) = str_of("from").and_then(|id| peers.get(&id)) {
                    let _ = p.signals.send(m["data"].clone());
                }
            }
            "error" => warn!("server: {}", m["error"]),
            _ => {}
        }
    }
    info!("signaling closed");
    Ok(())
}
