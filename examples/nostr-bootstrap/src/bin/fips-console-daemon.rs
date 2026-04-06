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
use fips::AppCommand;
use fips_nostr_rendezvous::client_runtime::{
    ClientRuntimeCore, ClientRuntimeParams, EventEnvelope,
};
use fips_nostr_rendezvous::common::{
    default_advert_relays, default_dm_relays, default_stun_servers, log_traversal_observation,
    now_ms, parse_csv_env_list,
};
use fips_nostr_rendezvous::fips_handoff::handoff_established_app_runtime;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::{oneshot, Mutex};
use tokio::time::sleep;

const CONSOLE_APP_PORT: u16 = 4200;

#[derive(Debug, Parser)]
#[command(
    name = "fips-console-daemon",
    about = "Rust console daemon runtime for FIPS-over-Nostr rendezvous"
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
struct ConsoleRuntimeHandle {
    peer_npub: String,
    command_tx: tokio::sync::mpsc::Sender<AppCommand>,
}

struct AppState {
    core: Arc<ClientRuntimeCore>,
    console_runtime: Mutex<Option<ConsoleRuntimeHandle>>,
}

impl AppState {
    async fn send_console_message(&self, text: String) -> Result<()> {
        let runtime = self
            .console_runtime
            .lock()
            .await
            .clone()
            .context("FIPS console runtime not connected")?;
        let (tx, rx) = oneshot::channel();
        runtime
            .command_tx
            .send(AppCommand::SendDatagram {
                peer_npub: runtime.peer_npub.clone(),
                src_port: CONSOLE_APP_PORT,
                dst_port: CONSOLE_APP_PORT,
                payload: text.into_bytes(),
                response: tx,
            })
            .await
            .context("console command channel closed")?;
        rx.await
            .context("console command response dropped")?
            .map_err(anyhow::Error::from)?;
        Ok(())
    }

    async fn accept_console_datagram(&self, peer_npub: String, payload: Vec<u8>) -> Result<()> {
        let text = String::from_utf8(payload).context("console payload must be UTF-8")?;
        self.core
            .emit(
                "message",
                json!({"from": peer_npub, "text": text, "at": now_ms()}),
            )
            .await;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct ConnectRequest {
    npub: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SendRequest {
    text: String,
}

async fn api_meta(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::to_value(state.core.meta_response()).unwrap_or_else(|_| json!({"ok": false})))
}

async fn api_discover(State(state): State<Arc<AppState>>) -> Json<Value> {
    match state.core.list_advertised_peers(10).await {
        Ok(peers) => Json(json!({"ok": true, "peers": peers})),
        Err(err) => Json(json!({"ok": false, "error": err.to_string(), "peers": []})),
    }
}

async fn api_send(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SendRequest>,
) -> Json<Value> {
    match state.send_console_message(body.text).await {
        Ok(()) => Json(json!({"ok": true})),
        Err(err) => Json(json!({"ok": false, "error": err.to_string()})),
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
            .connect_via_rendezvous(body.npub, "console-daemon")
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
                CONSOLE_APP_PORT,
            )
            .await?;
            let (handoff, command_tx, app_rx) = runtime.into_parts();
            *state.console_runtime.lock().await = Some(ConsoleRuntimeHandle {
                peer_npub: outcome.target_npub.clone(),
                command_tx,
            });
            let message_state = state.clone();
            let runtime_handle = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                while let Ok(datagram) = app_rx.recv() {
                    let message_state = message_state.clone();
                    let peer_npub = datagram.peer_npub;
                    let payload = datagram.payload;
                    runtime_handle.block_on(async move {
                        if let Err(err) = message_state
                            .accept_console_datagram(peer_npub, payload)
                            .await
                        {
                            eprintln!("[console-runtime] message-accept-error {err}");
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
            response["runtimeMode"] = json!("fips-console");
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
                "[console-daemon] connect success {}",
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
                "[console-daemon] connect failure {}",
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
        console_runtime: Mutex::new(None),
    });

    let udp_task = core.clone().spawn_udp_loop(|_, _| async { Ok(false) });
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
        .route("/api/send", post(api_send))
        .route("/api/events", get(api_events))
        .with_state(state);

    let listener = TcpListener::bind(("127.0.0.1", args.http_port)).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "app": "fips-console-daemon-rs",
            "http": format!("http://127.0.0.1:{}", listener.local_addr()?.port()),
            "npub": core.npub,
            "udpPort": core.udp_socket.local_addr()?.port(),
            "advertRelays": core.advert_relays,
            "dmRelays": core.dm_relays,
            "relaySource": "embedded-defaults",
            "discoveryEnabled": core.discovery_enabled,
            "handoffFips": core.handoff_fips,
            "appPort": CONSOLE_APP_PORT,
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
