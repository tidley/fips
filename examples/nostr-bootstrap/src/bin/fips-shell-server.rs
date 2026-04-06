use std::env;

use anyhow::{Context, Result};
use clap::Parser;
use fips_nostr_rendezvous::common::{
    default_advert_relays, default_dm_relays, default_stun_servers, log_traversal_observation,
    parse_csv_env_list,
};
use fips_nostr_rendezvous::fips_handoff::handoff_established_traversal;
use fips_nostr_rendezvous::server_runtime::{ServerRuntimeCore, ServerRuntimeParams};
use serde_json::json;
use tokio::signal;

#[derive(Debug, Parser)]
#[command(
    name = "fips-shell-server-rs",
    about = "Rust FIPS shell server over Nostr rendezvous"
)]
struct Args {
    #[arg(long)]
    nsec: String,

    #[arg(long, default_value_t = 9999)]
    udp_port: u16,

    #[arg(long, value_delimiter = ',', num_args = 1.., default_values_t = default_advert_relays())]
    advert_relays: Vec<String>,

    #[arg(long, value_delimiter = ',', num_args = 1.., default_values_t = default_dm_relays())]
    dm_relays: Vec<String>,

    #[arg(long, value_delimiter = ',', num_args = 0.., default_values_t = default_stun_servers())]
    stun_servers: Vec<String>,

    #[arg(long, value_delimiter = ',', default_value = "")]
    trusted_npubs: Vec<String>,

    #[arg(long)]
    public_host: Option<String>,

    #[arg(long, default_value_t = false)]
    handoff_fips: bool,
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

    let core = ServerRuntimeCore::create(ServerRuntimeParams {
        nsec: args.nsec,
        udp_port: args.udp_port,
        advert_relays: args.advert_relays,
        dm_relays: args.dm_relays,
        stun_servers: args.stun_servers,
        trusted_npubs: args.trusted_npubs,
        public_host: args.public_host,
        handoff_fips: args.handoff_fips,
    })
    .await?;

    let handoff_core = core.clone();
    let udp_task = core
        .clone()
        .spawn_udp_loop(move |session_id, peer_npub, remote| {
            let handoff_core = handoff_core.clone();
            async move {
                let handoff_socket = handoff_core.take_handoff_socket().await?;
                let status = handoff_established_traversal(
                    &handoff_core.resolved_nsec,
                    session_id,
                    peer_npub,
                    handoff_socket,
                    remote,
                )
                .await?;
                println!(
                    "[fips-handoff] {}",
                    serde_json::to_string(&json!({
                        "sessionId": status.session_id,
                        "peerNpub": status.peer_npub,
                        "transportId": status.transport_id,
                        "localAddr": status.local_addr,
                        "remoteAddr": status.remote_addr,
                    }))
                    .unwrap_or_else(|_| "{\"kind\":\"log-error\"}".to_owned())
                );
                Ok(())
            }
        });

    let observation = core.refresh_traversal_observation(true).await?;
    log_traversal_observation("server", observation.as_ref());
    core.publish_inbox_relays().await?;
    core.publish_advert().await?;
    let _advertise_task = core.clone().spawn_advertise_loop();
    core.subscribe_dm().await?;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "app": "fips-shell-server-rs",
            "npub": core.npub.clone(),
            "udpPort": core.udp_socket.local_addr()?.port(),
            "advertRelays": core.advert_relays.clone(),
            "dmRelays": core.dm_relays.clone(),
            "relaySource": "embedded-defaults",
            "trustedCount": core.trusted_npubs.len(),
            "handoffFips": core.handoff_fips,
        }))?
    );

    let notify_task = core.clone().spawn_notify_loop();
    tokio::select! {
        res = notify_task => { res??; }
        res = udp_task => { res??; }
        _ = signal::ctrl_c() => {}
    }

    Ok(())
}
