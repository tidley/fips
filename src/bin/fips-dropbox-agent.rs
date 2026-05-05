//! FIPS Dropbox-style receiver agent.
//!
//! Runs an embedded FIPS node, binds local service port 4242, stores incoming
//! blobs under a directory, and sends ACK/ERROR replies over the same FIPS
//! service-port path.

use std::path::PathBuf;

use clap::Parser;
use fips::config::{IdentitySource, resolve_identity};
use fips::dropbox::{DROPBOX_SERVICE_PORT, DropboxMessage, DropboxReceiver};
use fips::version;
use fips::{Config, EmbeddedNodeCommand, Node, ServiceOutbound};
use tracing::{debug, error, info, warn};
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Parser, Debug)]
#[command(
    name = "fips-dropbox-agent",
    version = version::short_version(),
    long_version = version::long_version(),
    about = "Receive Dropbox-style blobs over an embedded FIPS service port"
)]
struct Args {
    /// Path to configuration file
    #[arg(short, long, value_name = "FILE")]
    config: PathBuf,

    /// Directory where received blobs are written
    #[arg(long, value_name = "DIR", default_value = "/var/lib/fips-dropbox")]
    storage_root: PathBuf,

    /// Local FIPS service port to bind
    #[arg(long, default_value_t = DROPBOX_SERVICE_PORT)]
    port: u16,

    /// In-process service receive queue depth
    #[arg(long, default_value_t = 128)]
    queue_depth: usize,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args = Args::parse();

    let config = match Config::load_file(&args.config) {
        Ok(config) => config,
        Err(e) => {
            eprintln!(
                "Failed to load configuration from {}: {}",
                args.config.display(),
                e
            );
            std::process::exit(1);
        }
    };

    init_logging(&config);
    info!("FIPS Dropbox agent {} starting", version::short_version());
    info!(path = %args.config.display(), "Loaded config file");

    let resolved = match resolve_identity(&config, std::slice::from_ref(&args.config)) {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to resolve identity: {}", e);
            std::process::exit(1);
        }
    };
    match &resolved.source {
        IdentitySource::Config => info!("Using identity from configuration"),
        IdentitySource::KeyFile(p) => {
            info!(path = %p.display(), "Loaded persistent identity from key file")
        }
        IdentitySource::Generated(p) => {
            info!(path = %p.display(), "Generated persistent identity, saved to key file")
        }
        IdentitySource::Ephemeral => info!("Using ephemeral identity (new keypair each start)"),
    }

    let mut config = prepare_agent_config(config);
    config.node.identity.nsec = Some(resolved.nsec);

    let mut node = match Node::new(config) {
        Ok(node) => node,
        Err(e) => {
            error!("Failed to create node: {}", e);
            std::process::exit(1);
        }
    };

    let service_rx = match node.register_service_port(args.port, args.queue_depth) {
        Ok(rx) => rx,
        Err(e) => {
            error!("Failed to register service port {}: {}", args.port, e);
            std::process::exit(1);
        }
    };

    info!("Dropbox agent node created:");
    info!("      npub: {}", node.npub());
    info!("   node_addr: {}", hex::encode(node.node_addr().as_bytes()));
    info!("   address: {}", node.identity().address());
    info!("    service: {}", args.port);
    info!("    storage: {}", args.storage_root.display());

    if let Err(e) = node.start().await {
        error!("Failed to start node: {}", e);
        std::process::exit(1);
    }

    let (command_tx, command_rx) = tokio::sync::mpsc::channel::<EmbeddedNodeCommand>(64);
    let receiver_task = tokio::spawn(run_receiver(
        service_rx,
        command_tx.clone(),
        args.storage_root,
        args.port,
    ));

    let stop_tx = command_tx.clone();
    tokio::spawn(async move {
        foreground_shutdown_signal().await;
        let _ = stop_tx.send(EmbeddedNodeCommand::Stop).await;
    });

    info!("FIPS Dropbox agent running");
    let loop_result = node.run_embedded_loop(command_rx).await;

    info!("FIPS Dropbox agent shutting down");
    receiver_task.abort();
    if let Err(e) = node.stop().await {
        warn!("Error during shutdown: {}", e);
    }

    if let Err(e) = loop_result {
        error!("Embedded RX loop error: {}", e);
        std::process::exit(1);
    }

    info!("FIPS Dropbox agent shutdown complete");
}

async fn run_receiver(
    mut service_rx: fips::ServiceRx,
    command_tx: tokio::sync::mpsc::Sender<EmbeddedNodeCommand>,
    storage_root: PathBuf,
    service_port: u16,
) {
    let mut receiver = DropboxReceiver::new(storage_root);
    while let Some(packet) = service_rx.recv().await {
        match receiver.handle_service_packet(&packet) {
            Ok(replies) => {
                for reply in replies {
                    send_outbound(&command_tx, reply.into_service_outbound()).await;
                }
            }
            Err(error) => {
                warn!(error = %error, "Failed to handle Dropbox service packet");
                if let Ok(payload) = (DropboxMessage::Error {
                    id: None,
                    reason: error.to_string(),
                })
                .to_payload()
                {
                    send_outbound(
                        &command_tx,
                        ServiceOutbound {
                            dest_addr: packet.src_addr,
                            src_port: service_port,
                            dst_port: packet.src_port,
                            payload,
                        },
                    )
                    .await;
                }
            }
        }
    }
}

async fn send_outbound(
    command_tx: &tokio::sync::mpsc::Sender<EmbeddedNodeCommand>,
    outbound: ServiceOutbound,
) {
    if command_tx
        .send(EmbeddedNodeCommand::SendServiceData {
            outbound,
            respond_to: None,
        })
        .await
        .is_err()
    {
        debug!("Dropbox reply dropped because embedded command loop is closed");
    }
}

trait IntoServiceOutbound {
    fn into_service_outbound(self) -> ServiceOutbound;
}

impl IntoServiceOutbound for fips::dropbox::DropboxOutbound {
    fn into_service_outbound(self) -> ServiceOutbound {
        ServiceOutbound {
            dest_addr: self.dest_addr,
            src_port: self.src_port,
            dst_port: self.dst_port,
            payload: self.payload,
        }
    }
}

fn prepare_agent_config(mut config: Config) -> Config {
    config.tun.enabled = false;
    config.dns.enabled = false;
    config.node.control.enabled = false;
    config
}

fn init_logging(config: &Config) {
    let log_level = config.node.log_level();
    let nostr_directive = if log_level == tracing::Level::TRACE {
        "trace"
    } else {
        "info"
    };
    let default_directive = format!(
        "{log_level},nostr_relay_pool={nostr_directive},nostr_sdk={nostr_directive},nostr={nostr_directive}"
    );
    let filter = EnvFilter::builder()
        .with_default_directive(log_level.into())
        .parse_lossy(default_directive);
    let filter = match std::env::var("RUST_LOG") {
        Ok(env) if !env.is_empty() => EnvFilter::builder()
            .with_default_directive(log_level.into())
            .parse_lossy(env),
        _ => filter,
    };

    fmt().with_env_filter(filter).with_target(true).init();
}

async fn foreground_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
