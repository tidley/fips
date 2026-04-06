use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_stream::stream;
use axum::extract::State;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use fips_nostr_rendezvous::client_runtime::{
    ClientRuntimeCore, ClientRuntimeParams, EventEnvelope,
};
use fips_nostr_rendezvous::common::{
    default_advert_relays, default_dm_relays, default_stun_servers, log_traversal_observation,
    now_ms, parse_csv_env_list,
};
use fips_nostr_rendezvous::decode_session_frame;
use fips_nostr_rendezvous::fips_handoff::handoff_established_traversal;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::signal;
use tokio::time::sleep;

#[derive(Debug, Parser)]
#[command(
    name = "fips-web-daemon",
    about = "Rust web/daemon runtime for the FIPS web console"
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

    #[arg(long, default_value_t = false)]
    handoff_fips: bool,
}

struct AppState {
    core: Arc<ClientRuntimeCore>,
}

impl AppState {
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

#[derive(Debug, Deserialize)]
struct CmdRequest {
    cmd: String,
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

async fn api_connect(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ConnectRequest>,
) -> Json<Value> {
    let started_at = now_ms();
    let result: Result<Value> = async {
        let outcome = state
            .core
            .connect_via_rendezvous(body.npub, "web-daemon")
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
            let handoff = handoff_established_traversal(
                &state.core.resolved_nsec,
                outcome.session_id.clone(),
                outcome.target_npub.clone(),
                handoff_socket,
                remote_addr,
            )
            .await?;
            state
                .core
                .set_active_session(
                    outcome.session_id.clone(),
                    outcome.established_remote.clone(),
                )
                .await;
            response["handoff"] = serde_json::to_value(handoff)?;
            response["runtimeMode"] = json!("fips-handoff");
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
                "[web-daemon] connect success {}",
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
                "[web-daemon] connect failure {}",
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

async fn api_cmd(State(state): State<Arc<AppState>>, Json(body): Json<CmdRequest>) -> Json<Value> {
    if state.core.handoff_fips {
        return Json(json!({
            "ok": false,
            "error": "shell command channel unavailable after FIPS handoff"
        }));
    }
    let id = fips_nostr_rendezvous::common::nonce();
    match state
        .core
        .send_session_frame("shell", json!({"id": id, "cmd": body.cmd}), "request")
        .await
    {
        Ok(()) => Json(json!({"ok": true, "id": id})),
        Err(err) => Json(json!({"ok": false, "error": err.to_string()})),
    }
}

async fn api_ctrlc(State(state): State<Arc<AppState>>) -> Json<Value> {
    if state.core.handoff_fips {
        return Json(json!({
            "ok": false,
            "error": "shell interrupt unavailable after FIPS handoff"
        }));
    }
    match state
        .core
        .send_session_frame("shell_interrupt", json!({"ts": now_ms()}), "request")
        .await
    {
        Ok(()) => Json(json!({"ok": true})),
        Err(err) => Json(json!({"ok": false, "error": err.to_string()})),
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
        handoff_fips: args.handoff_fips,
    })
    .await?;
    let state = Arc::new(AppState { core: core.clone() });

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
        .route("/api/cmd", post(api_cmd))
        .route("/api/ctrlc", post(api_ctrlc))
        .route("/api/events", get(api_events))
        .with_state(state);

    let listener = TcpListener::bind(("127.0.0.1", args.http_port)).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "app": "fips-web-daemon-rs",
            "http": format!("http://127.0.0.1:{}", listener.local_addr()?.port()),
            "npub": core.npub,
            "udpPort": core.udp_socket.local_addr()?.port(),
            "advertRelays": core.advert_relays,
            "dmRelays": core.dm_relays,
            "relaySource": "embedded-defaults",
            "discoveryEnabled": core.discovery_enabled,
            "handoffFips": core.handoff_fips,
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
