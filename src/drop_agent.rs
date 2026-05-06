//! Product-facing FIPS Drop receiver agent.
//!
//! The agent runs an embedded FIPS node, binds local FSP service port 4242,
//! stores incoming files under a configured directory, and sends ACK/ERROR
//! replies over the same encrypted FIPS service path.

use std::ffi::OsStr;
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
    default_storage_root_for_program_name(std::env::args_os().next().as_deref())
}

fn default_storage_root_for_program_name(program_name: Option<&OsStr>) -> PathBuf {
    let is_legacy_binary = program_name
        .and_then(|arg| PathBuf::from(arg).file_stem().map(|stem| stem.to_owned()))
        .and_then(|stem| stem.into_string().ok())
        .is_some_and(|name| name == "fips-dropbox-agent");

    if is_legacy_binary {
        LEGACY_DROPBOX_STORAGE_ROOT.into()
    } else {
        DEFAULT_STORAGE_ROOT.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Identity, ServicePacket};
    use tokio::time::{Duration, timeout};

    #[test]
    fn args_parse_receiver_defaults() {
        let args = Args::try_parse_from(["fips-drop-agent", "--config", "/tmp/fips.yaml"])
            .expect("args parse");

        assert_eq!(args.config, PathBuf::from("/tmp/fips.yaml"));
        assert_eq!(args.storage_root, None);
        assert_eq!(args.port, DROPBOX_SERVICE_PORT);
        assert_eq!(args.queue_depth, 512);
    }

    #[test]
    fn args_parse_receiver_overrides() {
        let args = Args::try_parse_from([
            "fips-drop-agent",
            "--config",
            "/tmp/fips.yaml",
            "--storage-root",
            "/tmp/drop",
            "--port",
            "5000",
            "--queue-depth",
            "7",
        ])
        .expect("args parse");

        assert_eq!(args.storage_root, Some(PathBuf::from("/tmp/drop")));
        assert_eq!(args.port, 5000);
        assert_eq!(args.queue_depth, 7);
    }

    #[test]
    fn default_storage_root_preserves_legacy_binary_name() {
        assert_eq!(
            default_storage_root_for_program_name(Some(OsStr::new("/usr/bin/fips-drop-agent"))),
            PathBuf::from(DEFAULT_STORAGE_ROOT)
        );
        assert_eq!(
            default_storage_root_for_program_name(Some(OsStr::new("/usr/bin/fips-dropbox-agent"))),
            PathBuf::from(LEGACY_DROPBOX_STORAGE_ROOT)
        );
        assert_eq!(
            default_storage_root_for_program_name(None),
            PathBuf::from(DEFAULT_STORAGE_ROOT)
        );
    }

    #[test]
    fn prepare_agent_config_disables_host_services() {
        let mut config = Config::default();
        config.tun.enabled = true;
        config.dns.enabled = true;
        config.node.control.enabled = true;

        let config = prepare_agent_config(config);

        assert!(!config.tun.enabled);
        assert!(!config.dns.enabled);
        assert!(!config.node.control.enabled);
    }

    #[tokio::test]
    async fn receiver_task_queues_protocol_reply() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (service_tx, service_rx) = tokio::sync::mpsc::channel(1);
        let (command_tx, mut command_rx) = tokio::sync::mpsc::channel(1);
        let src_addr = *Identity::generate().node_addr();

        let task = tokio::spawn(run_receiver(
            service_rx,
            command_tx,
            dir.path().to_path_buf(),
            DROPBOX_SERVICE_PORT,
        ));

        service_tx
            .send(ServicePacket {
                src_addr,
                src_port: 6000,
                dst_port: DROPBOX_SERVICE_PORT,
                payload: DropboxMessage::Hello {
                    id: "0102030405060708".to_string(),
                    client: Some("test".to_string()),
                }
                .to_payload()
                .expect("payload"),
            })
            .await
            .expect("service send");

        let command = timeout(Duration::from_secs(1), command_rx.recv())
            .await
            .expect("command timeout")
            .expect("command");
        match command {
            EmbeddedNodeCommand::SendServiceData {
                outbound,
                respond_to,
            } => {
                assert_eq!(outbound.dest_addr, src_addr);
                assert_eq!(outbound.src_port, DROPBOX_SERVICE_PORT);
                assert_eq!(outbound.dst_port, 6000);
                assert_eq!(
                    DropboxMessage::from_payload(&outbound.payload).expect("reply"),
                    DropboxMessage::Ack {
                        id: "0102030405060708".to_string(),
                        status: "hello".to_string(),
                        sha256: None,
                        size: None,
                        path: None,
                    }
                );
                respond_to
                    .expect("respond_to")
                    .send(Ok(()))
                    .expect("response send");
            }
            _ => panic!("unexpected embedded command"),
        }

        drop(service_tx);
        timeout(Duration::from_secs(1), task)
            .await
            .expect("receiver task timeout")
            .expect("receiver task join");
    }

    #[tokio::test]
    async fn receiver_task_queues_error_reply_for_bad_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (service_tx, service_rx) = tokio::sync::mpsc::channel(1);
        let (command_tx, mut command_rx) = tokio::sync::mpsc::channel(1);
        let src_addr = *Identity::generate().node_addr();

        let task = tokio::spawn(run_receiver(
            service_rx,
            command_tx,
            dir.path().to_path_buf(),
            DROPBOX_SERVICE_PORT,
        ));

        service_tx
            .send(ServicePacket {
                src_addr,
                src_port: 6001,
                dst_port: DROPBOX_SERVICE_PORT,
                payload: vec![0xff, 0x00, 0x01],
            })
            .await
            .expect("service send");

        let command = timeout(Duration::from_secs(1), command_rx.recv())
            .await
            .expect("command timeout")
            .expect("command");
        match command {
            EmbeddedNodeCommand::SendServiceData {
                outbound,
                respond_to,
            } => {
                assert_eq!(outbound.dest_addr, src_addr);
                assert_eq!(outbound.src_port, DROPBOX_SERVICE_PORT);
                assert_eq!(outbound.dst_port, 6001);
                match DropboxMessage::from_payload(&outbound.payload).expect("reply") {
                    DropboxMessage::Error { id, reason } => {
                        assert_eq!(id, None);
                        assert!(reason.contains("payload"));
                    }
                    other => panic!("unexpected reply: {other:?}"),
                }
                respond_to
                    .expect("respond_to")
                    .send(Ok(()))
                    .expect("response send");
            }
            _ => panic!("unexpected embedded command"),
        }

        drop(service_tx);
        timeout(Duration::from_secs(1), task)
            .await
            .expect("receiver task timeout")
            .expect("receiver task join");
    }
}
