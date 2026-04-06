use std::env;
use std::io::BufRead;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use fips::AppCommand;
use fips_nostr_rendezvous::client_runtime::{ClientRuntimeCore, ClientRuntimeParams};
use fips_nostr_rendezvous::common::{
    default_advert_relays, default_dm_relays, default_stun_servers, log_traversal_observation,
    parse_csv_env_list,
};
use fips_nostr_rendezvous::fips_handoff::{handoff_established_app_runtime, FipsAppRuntime};
use serde_json::json;
use tokio::sync::oneshot;
use tokio::time::sleep;

const CONSOLE_APP_PORT: u16 = 4200;

#[derive(Debug, Parser)]
#[command(
    name = "fips-console-client",
    about = "Rust console client over FIPS and Nostr rendezvous"
)]
struct Args {
    #[arg(long, default_value = "")]
    nsec: String,

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

    #[arg(long)]
    npub: Option<String>,
}

async fn run_console_runtime(peer_npub: String, runtime: FipsAppRuntime) -> Result<()> {
    let (status, command_tx, app_rx) = runtime.into_parts();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "app": "fips-console-client",
            "sessionId": status.session_id,
            "peerNpub": status.peer_npub,
            "transportId": status.transport_id,
            "localAddr": status.local_addr,
            "remoteAddr": status.remote_addr,
        }))?
    );
    println!("Connected. Type lines and press Enter. Ctrl-C exits.");

    std::thread::spawn(move || {
        while let Ok(datagram) = app_rx.recv() {
            let text = String::from_utf8_lossy(&datagram.payload);
            println!("[{}] {}", datagram.peer_npub, text);
        }
    });

    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<String>(32);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(line) => {
                    if line_tx.blocking_send(line).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    eprintln!("[console-input] read-error {err}");
                    break;
                }
            }
        }
    });

    while let Some(line) = line_rx.recv().await {
        let text = line.trim_end().to_owned();
        if text.is_empty() {
            continue;
        }
        let (tx, rx) = oneshot::channel();
        command_tx
            .send(AppCommand::SendDatagram {
                peer_npub: peer_npub.clone(),
                src_port: CONSOLE_APP_PORT,
                dst_port: CONSOLE_APP_PORT,
                payload: text.clone().into_bytes(),
                response: tx,
            })
            .await
            .context("console command channel closed")?;
        rx.await
            .context("console command response dropped")?
            .map_err(anyhow::Error::from)?;
        println!("[me] {}", text);
    }

    Ok(())
}

async fn connect_cli(core: Arc<ClientRuntimeCore>, npub: Option<String>) -> Result<()> {
    let outcome = core.connect_via_rendezvous(npub, "console-client").await?;
    core.shutdown_udp().await;
    sleep(Duration::from_millis(50)).await;
    let handoff_socket = core.take_handoff_socket().await?;
    let remote_addr = SocketAddr::new(
        outcome
            .established_remote
            .host
            .parse()
            .context("invalid established remote host")?,
        outcome.established_remote.port,
    );
    let runtime = handoff_established_app_runtime(
        &core.resolved_nsec,
        outcome.session_id,
        outcome.target_npub.clone(),
        handoff_socket,
        remote_addr,
        CONSOLE_APP_PORT,
    )
    .await?;

    run_console_runtime(outcome.target_npub, runtime).await
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

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "app": "fips-console-client-rs",
            "npub": core.npub.clone(),
            "udpPort": core.udp_socket.local_addr()?.port(),
            "advertRelays": core.advert_relays.clone(),
            "dmRelays": core.dm_relays.clone(),
            "relaySource": "embedded-defaults",
            "discoveryEnabled": core.discovery_enabled,
            "handoffFips": core.handoff_fips,
            "appPort": CONSOLE_APP_PORT,
            "target": args.npub,
        }))?
    );

    let result = connect_cli(core.clone(), args.npub.clone()).await;
    notify_task.abort();
    udp_task.abort();
    result
}
