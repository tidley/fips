use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_stream::stream;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use fips::AppCommand;
use fips_nostr_rendezvous::client_runtime::{
    ClientRuntimeCore, ClientRuntimeParams, EventEnvelope,
};
use fips_nostr_rendezvous::common::{
    default_advert_relays, default_dm_relays, default_stun_servers, log_traversal_observation,
    now_ms, parse_csv_env_list,
};
use fips_nostr_rendezvous::decode_session_frame;
use fips_nostr_rendezvous::fips_handoff::handoff_established_app_runtime;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::{oneshot, Mutex, RwLock};
use tokio::time::sleep;

const VIDEO_APP_PORT: u16 = 4100;
const VIDEO_FRAME_CHUNK_SIZE: usize = 900;
const VIDEO_FRAME_KIND: u8 = 1;

#[derive(Debug, Parser)]
#[command(
    name = "fips-video-daemon",
    about = "Rust video daemon runtime for FIPS-over-Nostr rendezvous"
)]
struct Args {
    #[arg(long, default_value = "")]
    nsec: String,

    #[arg(long, default_value_t = 8788)]
    http_port: u16,

    #[arg(long, default_value_t = 0)]
    udp_port: u16,

    #[arg(long, value_delimiter = ',', num_args = 1.., default_values_t = default_advert_relays())]
    advert_relays: Vec<String>,

    #[arg(long, value_delimiter = ',', num_args = 1.., default_values_t = default_dm_relays())]
    dm_relays: Vec<String>,

    #[arg(long, value_delimiter = ',', num_args = 0.., default_values_t = default_stun_servers())]
    stun_servers: Vec<String>,

    #[arg(long)]
    public_host: Option<String>,

    #[arg(long, default_value_t = false)]
    no_discover: bool,
}

#[derive(Debug, Clone)]
struct LatestFrame {
    frame_id: u32,
    jpeg: Vec<u8>,
    received_at_ms: u64,
}

#[derive(Debug)]
struct FrameAssembly {
    total_chunks: u16,
    chunks: Vec<Option<Vec<u8>>>,
    received_chunks: usize,
}

#[derive(Debug, Clone)]
struct VideoRuntimeHandle {
    peer_npub: String,
    command_tx: tokio::sync::mpsc::Sender<AppCommand>,
}

struct AppState {
    core: Arc<ClientRuntimeCore>,
    video_runtime: Mutex<Option<VideoRuntimeHandle>>,
    latest_frame: RwLock<Option<LatestFrame>>,
    pending_frames: Mutex<HashMap<(String, u32), FrameAssembly>>,
}

impl AppState {
    async fn set_video_runtime(&self, runtime: VideoRuntimeHandle) {
        *self.video_runtime.lock().await = Some(runtime);
    }

    async fn send_video_frame(&self, jpeg: Vec<u8>) -> Result<u32> {
        let runtime = self
            .video_runtime
            .lock()
            .await
            .clone()
            .context("FIPS video runtime not connected")?;
        let frame_id = rand::random::<u32>();
        let total_chunks = jpeg.len().div_ceil(VIDEO_FRAME_CHUNK_SIZE) as u16;

        for (index, chunk) in jpeg.chunks(VIDEO_FRAME_CHUNK_SIZE).enumerate() {
            let mut packet = Vec::with_capacity(9 + chunk.len());
            packet.push(VIDEO_FRAME_KIND);
            packet.extend_from_slice(&frame_id.to_be_bytes());
            packet.extend_from_slice(&total_chunks.to_be_bytes());
            packet.extend_from_slice(&(index as u16).to_be_bytes());
            packet.extend_from_slice(chunk);

            let (tx, rx) = oneshot::channel();
            runtime
                .command_tx
                .send(AppCommand::SendDatagram {
                    peer_npub: runtime.peer_npub.clone(),
                    src_port: VIDEO_APP_PORT,
                    dst_port: VIDEO_APP_PORT,
                    payload: packet,
                    response: tx,
                })
                .await
                .context("video command channel closed")?;
            rx.await
                .context("video command response dropped")?
                .map_err(anyhow::Error::from)?;
        }

        Ok(frame_id)
    }

    async fn accept_video_datagram(&self, peer_npub: String, payload: Vec<u8>) -> Result<()> {
        if payload.len() < 9 || payload[0] != VIDEO_FRAME_KIND {
            return Ok(());
        }
        let frame_id = u32::from_be_bytes(payload[1..5].try_into()?);
        let total_chunks = u16::from_be_bytes(payload[5..7].try_into()?);
        let chunk_index = u16::from_be_bytes(payload[7..9].try_into()?);
        let chunk_payload = payload[9..].to_vec();
        let key = (peer_npub, frame_id);

        let mut completed = None;
        {
            let mut pending = self.pending_frames.lock().await;
            let assembly = pending.entry(key.clone()).or_insert_with(|| FrameAssembly {
                total_chunks,
                chunks: vec![None; total_chunks as usize],
                received_chunks: 0,
            });
            if assembly.total_chunks != total_chunks {
                *assembly = FrameAssembly {
                    total_chunks,
                    chunks: vec![None; total_chunks as usize],
                    received_chunks: 0,
                };
            }
            let index = chunk_index as usize;
            if index >= assembly.chunks.len() {
                return Ok(());
            }
            if assembly.chunks[index].is_none() {
                assembly.received_chunks += 1;
                assembly.chunks[index] = Some(chunk_payload);
            }
            if assembly.received_chunks == assembly.total_chunks as usize {
                let mut jpeg = Vec::new();
                for chunk in assembly.chunks.iter_mut() {
                    if let Some(bytes) = chunk.take() {
                        jpeg.extend_from_slice(&bytes);
                    }
                }
                completed = Some(jpeg);
                pending.remove(&key);
            }
        }

        if let Some(jpeg) = completed {
            *self.latest_frame.write().await = Some(LatestFrame {
                frame_id,
                jpeg,
                received_at_ms: now_ms(),
            });
            self.core
                .emit(
                    "frame",
                    json!({"frameId": frame_id, "receivedAt": now_ms()}),
                )
                .await;
        }
        Ok(())
    }

    async fn handle_udp_packet(&self, packet: Vec<u8>, remote: SocketAddr) -> Result<bool> {
        let frame = match decode_session_frame(&packet) {
            Ok(frame) => frame,
            Err(_) => return Ok(false),
        };
        if self
            .core
            .active_session_matches(&frame.session_id, remote)
            .await
            .is_none()
        {
            return Ok(false);
        }
        if frame.channel.as_deref() == Some("shell_result") {
            self.core.emit("result", frame.payload).await;
            return Ok(true);
        }
        Ok(false)
    }
}

#[derive(Debug, Deserialize)]
struct ConnectRequest {
    npub: Option<String>,
}

async fn api_meta(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(serde_json::to_value(state.core.meta_response()).unwrap_or_else(|_| json!({"ok": false})))
}

async fn api_discover(State(state): State<Arc<AppState>>) -> Json<Value> {
    match state.core.list_advertised_peers(10).await {
        Ok(peers) => Json(json!({"ok": true, "peers": peers})),
        Err(err) => Json(json!({"ok": false, "error": err.to_string(), "peers": []})),
    }
}

async fn api_frame(State(state): State<Arc<AppState>>, body: Bytes) -> Json<Value> {
    match state.send_video_frame(body.to_vec()).await {
        Ok(frame_id) => Json(json!({"ok": true, "frameId": frame_id, "bytes": body.len()})),
        Err(err) => Json(json!({"ok": false, "error": err.to_string()})),
    }
}

async fn api_remote_frame(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if let Some(frame) = state.latest_frame.read().await.clone() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("image/jpeg"));
        headers.insert(
            "cache-control",
            HeaderValue::from_static("no-store, max-age=0"),
        );
        headers.insert(
            "x-frame-id",
            HeaderValue::from_str(&frame.frame_id.to_string())
                .unwrap_or_else(|_| HeaderValue::from_static("0")),
        );
        headers.insert(
            "x-received-at",
            HeaderValue::from_str(&frame.received_at_ms.to_string())
                .unwrap_or_else(|_| HeaderValue::from_static("0")),
        );
        (StatusCode::OK, headers, frame.jpeg).into_response()
    } else {
        StatusCode::NO_CONTENT.into_response()
    }
}

async fn api_connect(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ConnectRequest>,
) -> Json<Value> {
    let started_at = now_ms();
    let result: Result<Value> = async {
        let outcome = state
            .core
            .connect_via_rendezvous(body.npub, "video-daemon")
            .await?;
        let mut response = json!({
            "ok": true,
            "sessionId": outcome.session_id,
            "remote": outcome.established_remote,
            "discovered": outcome.discovered_advert.is_some(),
            "discoveredAdvert": outcome.discovered_advert,
        });

        if state.core.handoff_fips {
            state.core.shutdown_udp().await;
            sleep(Duration::from_millis(50)).await;
            let handoff_socket = state.core.take_handoff_socket().await?;
            let remote_addr = SocketAddr::new(
                outcome
                    .established_remote
                    .host
                    .parse()
                    .context("invalid established remote host")?,
                outcome.established_remote.port,
            );
            let runtime = handoff_established_app_runtime(
                &state.core.resolved_nsec,
                outcome.session_id.clone(),
                outcome.target_npub.clone(),
                handoff_socket,
                remote_addr,
                VIDEO_APP_PORT,
            )
            .await?;
            let (handoff, command_tx, app_rx) = runtime.into_parts();
            state
                .set_video_runtime(VideoRuntimeHandle {
                    peer_npub: outcome.target_npub.clone(),
                    command_tx,
                })
                .await;
            let frame_state = state.clone();
            let runtime_handle = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                while let Ok(datagram) = app_rx.recv() {
                    let frame_state = frame_state.clone();
                    let peer_npub = datagram.peer_npub;
                    let payload = datagram.payload;
                    runtime_handle.block_on(async move {
                        if let Err(err) =
                            frame_state.accept_video_datagram(peer_npub, payload).await
                        {
                            eprintln!("[video-runtime] frame-accept-error {err}");
                        }
                    });
                }
            });
            state
                .core
                .set_active_session(
                    outcome.session_id.clone(),
                    outcome.established_remote.clone(),
                )
                .await;
            response["handoff"] = serde_json::to_value(handoff)?;
            response["runtimeMode"] = json!("fips-video");
            return Ok(response);
        }

        state
            .core
            .set_active_session(
                outcome.session_id.clone(),
                outcome.established_remote.clone(),
            )
            .await;
        Ok(response)
    }
    .await;

    match result {
        Ok(value) => {
            println!(
                "[video-daemon] connect success {}",
                serde_json::to_string(&json!({
                    "sessionId": value["sessionId"],
                    "remote": value["remote"],
                    "elapsedMs": now_ms().saturating_sub(started_at),
                }))
                .unwrap_or_default()
            );
            Json(value)
        }
        Err(err) => {
            println!(
                "[video-daemon] connect failure {}",
                serde_json::to_string(&json!({
                    "error": err.to_string(),
                    "elapsedMs": now_ms().saturating_sub(started_at),
                }))
                .unwrap_or_default()
            );
            Json(json!({"ok": false, "error": err.to_string()}))
        }
    }
}

async fn api_events(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    let mut rx = state.core.subscribe_events();
    let initial = state.core.current_status_value().await;
    Sse::new(stream! {
        yield Ok::<SseEvent, std::convert::Infallible>(SseEvent::default().event("status").data(initial.to_string()));
        while let Ok(EventEnvelope { event, data }) = rx.recv().await {
            yield Ok::<SseEvent, std::convert::Infallible>(SseEvent::default().event(event).data(data.to_string()));
        }
    })
    .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("keepalive"))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .init();

    let mut args = Args::parse();
    if args.nsec.is_empty() {
        args.nsec = env::var("NOSTR_NSEC").context("missing --nsec or NOSTR_NSEC")?;
    }
    if let Some(stun_servers) = parse_csv_env_list("FIPS_STUN_SERVERS") {
        args.stun_servers = stun_servers;
    }
    if args.public_host.is_none() {
        args.public_host = env::var("FIPS_UDP_PUBLIC_HOST")
            .ok()
            .filter(|value| !value.is_empty());
    }
    args.advert_relays.retain(|value| !value.trim().is_empty());
    args.dm_relays.retain(|value| !value.trim().is_empty());
    args.stun_servers.retain(|value| !value.trim().is_empty());

    let core = ClientRuntimeCore::create(ClientRuntimeParams {
        nsec: args.nsec,
        udp_port: args.udp_port,
        advert_relays: args.advert_relays.clone(),
        dm_relays: args.dm_relays.clone(),
        stun_servers: args.stun_servers.clone(),
        public_host: args.public_host.clone(),
        discovery_enabled: !args.no_discover,
        handoff_fips: true,
    })
    .await?;
    let state = Arc::new(AppState {
        core: core.clone(),
        video_runtime: Mutex::new(None),
        latest_frame: RwLock::new(None),
        pending_frames: Mutex::new(HashMap::new()),
    });

    let udp_state = state.clone();
    let udp_task = core.clone().spawn_udp_loop(move |packet, remote| {
        let udp_state = udp_state.clone();
        async move { udp_state.handle_udp_packet(packet, remote).await }
    });
    let observation = core
        .refresh_traversal_observation(true)
        .await
        .ok()
        .flatten();
    log_traversal_observation("client", observation.as_ref());
    core.publish_inbox_relays().await.ok();
    let notify_task = core.clone().spawn_notify_loop();
    let _subscriptions = core.clone().spawn_subscriptions();

    let app = Router::new()
        .route("/api/meta", get(api_meta))
        .route("/api/discover", get(api_discover))
        .route("/api/connect", post(api_connect))
        .route("/api/frame", post(api_frame))
        .route("/api/remote-frame", get(api_remote_frame))
        .route("/api/events", get(api_events))
        .with_state(state);

    let listener = TcpListener::bind(("127.0.0.1", args.http_port)).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "app": "fips-video-daemon-rs",
            "http": format!("http://127.0.0.1:{}", listener.local_addr()?.port()),
            "npub": core.npub,
            "udpPort": core.udp_socket.local_addr()?.port(),
            "advertRelays": core.advert_relays,
            "dmRelays": core.dm_relays,
            "relaySource": "embedded-defaults",
            "discoveryEnabled": core.discovery_enabled,
            "handoffFips": core.handoff_fips,
            "appPort": VIDEO_APP_PORT,
        }))?
    );

    tokio::select! {
        res = axum::serve(listener, app) => { res?; }
        res = notify_task => { res??; }
        res = udp_task => { res??; }
        _ = signal::ctrl_c() => {}
    }

    Ok(())
}
