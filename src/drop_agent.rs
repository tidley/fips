//! Product-facing FIPS Drop receiver agent.
//!
//! The agent runs an embedded FIPS node, binds local FSP service port 4242,
//! stores incoming files under a configured directory, and sends ACK/ERROR
//! replies over the same encrypted FIPS service path.

use std::path::PathBuf;

use clap::Parser;
use tracing::{debug, error, info, warn};
use tracing_subscriber::{EnvFilter, fmt};

use crate::config::{IdentitySource, resolve_identity};
use crate::dropbox::{DROPBOX_SERVICE_PORT, DropboxMessage, DropboxReceiver};
use crate::version;
use crate::{Config, EmbeddedNodeCommand, Node, ServiceOutbound};

pub const DEFAULT_STORAGE_ROOT: &str = "/var/lib/fips-drop";
pub const LEGACY_DROPBOX_STORAGE_ROOT: &str = "/var/lib/fips-dropbox";

#[derive(Parser, Debug)]
#[command(
    version = version::short_version(),
    long_version = version::long_version(),
    about = "Receive FIPS Drop files over an embedded FIPS service port"
)]
struct Args {
    /// Path to configuration file
    #[arg(short, long, value_name = "FILE")]
    config: PathBuf,

    /// Directory where received files are written
    #[arg(long, value_name = "DIR")]
    storage_root: Option<PathBuf>,

    /// Local FIPS service port to bind
    #[arg(long, default_value_t = DROPBOX_SERVICE_PORT)]
    port: u16,

    /// In-process service receive queue depth
    #[arg(long, default_value_t = 512)]
    queue_depth: usize,
}

pub async fn run_from_env() {
    let args = Args::parse();
    run(args).await;
}

async fn run(args: Args) {
    let storage_root = args
        .storage_root
        .unwrap_or_else(default_storage_root_for_binary);

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
    info!("FIPS Drop agent {} starting", version::short_version());
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

    info!("FIPS Drop agent node created:");
    info!("      npub: {}", node.npub());
    info!("   node_addr: {}", hex::encode(node.node_addr().as_bytes()));
    info!("   address: {}", node.identity().address());
    info!("    service: {}", args.port);
    info!("    storage: {}", storage_root.display());

    if let Err(e) = node.start().await {
        error!("Failed to start node: {}", e);
        std::process::exit(1);
    }

    let (command_tx, command_rx) = tokio::sync::mpsc::channel::<EmbeddedNodeCommand>(64);
    let receiver_task = tokio::spawn(run_receiver(
        service_rx,
        command_tx.clone(),
        storage_root,
        args.port,
    ));

    let stop_tx = command_tx.clone();
    tokio::spawn(async move {
        foreground_shutdown_signal().await;
        let _ = stop_tx.send(EmbeddedNodeCommand::Stop).await;
    });

    info!("FIPS Drop agent running");
    let loop_result = node.run_embedded_loop(command_rx).await;

    info!("FIPS Drop agent shutting down");
    receiver_task.abort();
    if let Err(e) = node.stop().await {
        warn!("Error during shutdown: {}", e);
    }

    if let Err(e) = loop_result {
        error!("Embedded RX loop error: {}", e);
        std::process::exit(1);
    }

    info!("FIPS Drop agent shutdown complete");
}

async fn run_receiver(
    mut service_rx: crate::ServiceRx,
    command_tx: tokio::sync::mpsc::Sender<EmbeddedNodeCommand>,
    storage_root: PathBuf,
    service_port: u16,
) {
    let mut receiver = DropboxReceiver::new(storage_root);
    while let Some(packet) = service_rx.recv().await {
        debug!(
            src = %packet.src_addr,
            src_port = packet.src_port,
            dst_port = packet.dst_port,
            len = packet.payload.len(),
            "FIPS Drop service packet received"
        );
        match receiver.handle_service_packet(&packet) {
            Ok(replies) => {
                for reply in replies {
                    send_outbound(&command_tx, reply.into_service_outbound()).await;
                }
            }
            Err(error) => {
                warn!(error = %error, "Failed to handle FIPS Drop service packet");
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
    let dest_addr = outbound.dest_addr;
    let src_port = outbound.src_port;
    let dst_port = outbound.dst_port;
    let len = outbound.payload.len();
    let (respond_to, response_rx) = tokio::sync::oneshot::channel();
    if command_tx
        .send(EmbeddedNodeCommand::SendServiceData {
            outbound,
            respond_to: Some(respond_to),
        })
        .await
        .is_err()
    {
        debug!("FIPS Drop reply dropped because embedded command loop is closed");
        return;
    }

    match response_rx.await {
        Ok(Ok(())) => {
            debug!(
                dest = %dest_addr,
                src_port,
                dst_port,
                len,
                "FIPS Drop reply queued for FIPS send"
            );
        }
        Ok(Err(error)) => {
            warn!(
                dest = %dest_addr,
                src_port,
                dst_port,
                len,
                error = %error,
                "Failed to queue FIPS Drop reply for FIPS send"
            );
        }
        Err(_) => {
            debug!(
                dest = %dest_addr,
                src_port,
                dst_port,
                len,
                "FIPS Drop reply result dropped because embedded command loop closed"
            );
        }
    }
}

trait IntoServiceOutbound {
    fn into_service_outbound(self) -> ServiceOutbound;
}

impl IntoServiceOutbound for crate::dropbox::DropboxOutbound {
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

    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(false)
        .with_thread_names(false)
        .init();
}

#[cfg(unix)]
async fn foreground_shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
}

#[cfg(not(unix))]
async fn foreground_shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

fn default_storage_root_for_binary() -> PathBuf {
    let is_legacy_binary = std::env::args_os()
        .next()
        .and_then(|arg| PathBuf::from(arg).file_stem().map(|stem| stem.to_owned()))
        .and_then(|stem| stem.into_string().ok())
        .is_some_and(|name| name == "fips-dropbox-agent");

    if is_legacy_binary {
        LEGACY_DROPBOX_STORAGE_ROOT.into()
    } else {
        DEFAULT_STORAGE_ROOT.into()
    }
}
