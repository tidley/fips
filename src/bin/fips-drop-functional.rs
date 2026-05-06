//! Real-world FIPS Drop functional harness.
//!
//! This binary intentionally uses public Nostr relays and public STUN servers.
//! It starts an in-process receiver and an in-process mobile-style sender,
//! establishes the normal Nostr/STUN/FIPS path, transfers bytes through service
//! port 4242, and verifies the stored file hash.

use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Parser;
use fips::config::{NostrDiscoveryPolicy, PeerConfig, TransportInstances};
use fips::dropbox::{DROPBOX_SERVICE_PORT, DropboxMessage, DropboxReceiver, sha256_hex};
use fips::mobile::{FipsMobileClient, FipsMobileConfig, MOBILE_RESPONSE_PORT};
use fips::{
    Config, EmbeddedNodeCommand, EmbeddedNodeStatus, Identity, Node, ServiceOutbound, ServiceRx,
    UdpConfig, encode_nsec,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};
use tracing_subscriber::{EnvFilter, fmt};

type HarnessResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

const DEFAULT_RELAYS: &str = "wss://relay.damus.io,wss://nos.lol,wss://offchain.pub";
const DEFAULT_STUN_SERVERS: &str =
    "stun:stun.l.google.com:19302,stun:stun.cloudflare.com:3478,stun:global.stun.twilio.com:3478";

#[derive(Debug, Parser)]
#[command(
    version = fips::version::short_version(),
    long_version = fips::version::long_version(),
    about = "Run a real-relay FIPS Drop functional transfer"
)]
struct Args {
    /// Number of transfers to run on the same session.
    #[arg(long, default_value_t = 1)]
    runs: usize,

    /// Deterministic payload size per transfer.
    #[arg(long, default_value_t = 192 * 1024)]
    payload_bytes: usize,

    /// End-to-end session wait timeout.
    #[arg(long, default_value_t = 90_000)]
    connect_timeout_ms: u64,

    /// Per-transfer timeout enforced by the mobile transfer protocol.
    #[arg(long, default_value_t = 120_000)]
    transfer_timeout_ms: u64,

    /// Comma-separated relay URLs used for adverts and NIP-59 signaling.
    #[arg(long, value_delimiter = ',', default_value = DEFAULT_RELAYS)]
    relays: Vec<String>,

    /// Comma-separated STUN server URLs.
    #[arg(long, value_delimiter = ',', default_value = DEFAULT_STUN_SERVERS)]
    stun_servers: Vec<String>,

    /// Override the Nostr discovery application namespace.
    #[arg(long)]
    app: Option<String>,

    /// Directory for receiver artifacts. A temp directory is used by default.
    #[arg(long)]
    storage_root: Option<PathBuf>,

    /// Keep the default temp storage directory after a successful run.
    #[arg(long)]
    keep_artifacts: bool,

    /// Do not advertise RFC1918/ULA local candidates in traversal offers.
    #[arg(long)]
    no_local_candidates: bool,

    /// Emit a compact machine-readable result line at the end.
    #[arg(long)]
    json: bool,
}

struct ReceiverHarness {
    npub: String,
    node_addr: String,
    command_tx: mpsc::Sender<EmbeddedNodeCommand>,
    node_task: tokio::task::JoinHandle<Result<(), fips::NodeError>>,
    receiver_task: tokio::task::JoinHandle<()>,
}

impl ReceiverHarness {
    async fn status(&self) -> HarnessResult<EmbeddedNodeStatus> {
        let (respond_to, response_rx) = oneshot::channel();
        self.command_tx
            .send(EmbeddedNodeCommand::Status { respond_to })
            .await?;
        Ok(response_rx.await?)
    }

    async fn stop(self) -> HarnessResult<()> {
        let _ = self.command_tx.send(EmbeddedNodeCommand::Stop).await;
        self.receiver_task.abort();
        self.node_task.await??;
        Ok(())
    }
}

struct RunOutcome {
    run: usize,
    file_name: String,
    bytes: usize,
    sha256: String,
    path: PathBuf,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    init_logging();
    if let Err(error) = run().await {
        error!(%error, "real-world FIPS Drop functional harness failed");
        eprintln!("FAIL: {error}");
        std::process::exit(1);
    }
}

async fn run() -> HarnessResult<()> {
    let args = Args::parse();
    if args.runs == 0 {
        return Err("--runs must be greater than zero".into());
    }
    if args.payload_bytes == 0 {
        return Err("--payload-bytes must be greater than zero".into());
    }

    let started_ms = now_millis();
    let app = args
        .app
        .clone()
        .unwrap_or_else(|| format!("fips-drop-functional-{started_ms}"));
    let storage_root = args
        .storage_root
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("fips-drop-functional-{started_ms}")));

    std::fs::create_dir_all(&storage_root)?;

    let receiver_identity = Identity::generate();
    let sender_identity = Identity::generate();
    let receiver_npub = receiver_identity.npub();
    let sender_npub = sender_identity.npub();

    let share_local_candidates = !args.no_local_candidates;

    info!(app, relays = ?args.relays, stun = ?args.stun_servers, share_local_candidates, "starting real-world harness");
    println!("Real-world FIPS Drop harness");
    println!("  app:      {app}");
    println!("  relays:   {}", args.relays.join(", "));
    println!("  stun:     {}", args.stun_servers.join(", "));
    println!("  local candidates: {share_local_candidates}");
    println!("  storage:  {}", storage_root.display());
    println!("  receiver: {receiver_npub}");
    println!("  sender:   {sender_npub}");

    let receiver_config = harness_config(
        &receiver_identity,
        None,
        &args.relays,
        &args.stun_servers,
        &app,
        share_local_candidates,
    );
    let sender_config = harness_config(
        &sender_identity,
        Some(receiver_npub.as_str()),
        &args.relays,
        &args.stun_servers,
        &app,
        share_local_candidates,
    );

    let receiver = start_receiver(
        receiver_config,
        storage_root.clone(),
        DROPBOX_SERVICE_PORT,
        1024,
    )
    .await?;
    let mut sender = FipsMobileClient::start(FipsMobileConfig {
        config: sender_config,
        response_port: MOBILE_RESPONSE_PORT,
        queue_depth: 1024,
    })
    .await?;

    println!("  receiver node_addr: {}", receiver.node_addr);
    println!("Connecting through public relay/STUN path...");

    let connect_timeout = Duration::from_millis(args.connect_timeout_ms);
    if let Err(error) = sender
        .wait_for_session_npub(&receiver.npub, connect_timeout)
        .await
    {
        dump_statuses(&sender, &receiver).await;
        let _ = sender.stop().await;
        let _ = receiver.stop().await;
        return Err(format!("session setup failed: {error}").into());
    }

    println!("Session established; sending {} run(s).", args.runs);
    let mut outcomes = Vec::new();
    for run_index in 0..args.runs {
        let transfer_result = run_transfer(
            &mut sender,
            &receiver.npub,
            &storage_root,
            started_ms,
            run_index,
            args.payload_bytes,
            Duration::from_millis(args.transfer_timeout_ms),
        )
        .await;
        let outcome = match transfer_result {
            Ok(outcome) => outcome,
            Err(error) => {
                dump_statuses(&sender, &receiver).await;
                let _ = sender.stop().await;
                let _ = receiver.stop().await;
                return Err(format!("transfer {} failed: {error}", run_index + 1).into());
            }
        };
        println!(
            "PASS run={} bytes={} sha256={} path={}",
            outcome.run,
            outcome.bytes,
            outcome.sha256,
            outcome.path.display()
        );
        outcomes.push(outcome);
    }

    let sender_status = sender.status().await?;
    let receiver_status = receiver.status().await?;
    println!(
        "Final status: sender peers={} links={} sessions={}; receiver peers={} links={} sessions={}",
        sender_status.peer_count,
        sender_status.link_count,
        sender_status.session_count,
        receiver_status.peer_count,
        receiver_status.link_count,
        receiver_status.session_count
    );

    if args.json {
        println!(
            "{}",
            json_summary(&app, &receiver.npub, &sender_npub, &outcomes)
        );
    }

    sender.stop().await?;
    receiver.stop().await?;

    if args.storage_root.is_none() && !args.keep_artifacts {
        let _ = std::fs::remove_dir_all(&storage_root);
    }

    Ok(())
}

async fn run_transfer(
    sender: &mut FipsMobileClient,
    receiver_npub: &str,
    storage_root: &std::path::Path,
    run_started_ms: u128,
    run_index: usize,
    payload_bytes: usize,
    transfer_timeout: Duration,
) -> HarnessResult<RunOutcome> {
    let data = deterministic_payload(payload_bytes, run_index as u8);
    let sha256 = sha256_hex(&data);
    let file_name = format!("functional-{run_started_ms}-{run_index}.bin");
    let path = storage_root.join(&file_name);

    let transfer = sender.send_dropbox_blob_to_npub(
        receiver_npub,
        &file_name,
        Some("application/octet-stream".to_string()),
        &data,
    );
    tokio::time::timeout(transfer_timeout, transfer).await??;

    let stored = std::fs::read(&path)?;
    let stored_sha256 = sha256_hex(&stored);
    if stored != data {
        return Err(
            format!("stored file hash mismatch: expected {sha256}, got {stored_sha256}").into(),
        );
    }

    Ok(RunOutcome {
        run: run_index + 1,
        file_name,
        bytes: data.len(),
        sha256,
        path,
    })
}

async fn start_receiver(
    mut config: Config,
    storage_root: PathBuf,
    service_port: u16,
    queue_depth: usize,
) -> HarnessResult<ReceiverHarness> {
    config.tun.enabled = false;
    config.dns.enabled = false;
    config.node.control.enabled = false;

    let mut node = Node::new(config)?;
    let service_rx = node.register_service_port(service_port, queue_depth)?;
    let npub = node.npub();
    let node_addr = node.node_addr().to_string();
    node.start().await?;

    let (command_tx, command_rx) = mpsc::channel::<EmbeddedNodeCommand>(queue_depth.max(64));
    let receiver_task = tokio::spawn(run_receiver_service(
        service_rx,
        command_tx.clone(),
        storage_root,
        service_port,
    ));
    let node_task = tokio::spawn(async move {
        let loop_result = node.run_embedded_loop(command_rx).await;
        let stop_result = node.stop().await;
        match (loop_result, stop_result) {
            (Err(loop_error), _) => Err(loop_error),
            (Ok(()), Err(stop_error)) => Err(stop_error),
            (Ok(()), Ok(())) => Ok(()),
        }
    });

    Ok(ReceiverHarness {
        npub,
        node_addr,
        command_tx,
        node_task,
        receiver_task,
    })
}

async fn run_receiver_service(
    mut service_rx: ServiceRx,
    command_tx: mpsc::Sender<EmbeddedNodeCommand>,
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
            "functional receiver packet received"
        );

        match receiver.handle_service_packet(&packet) {
            Ok(replies) => {
                for reply in replies {
                    send_outbound(
                        &command_tx,
                        ServiceOutbound {
                            dest_addr: reply.dest_addr,
                            src_port: reply.src_port,
                            dst_port: reply.dst_port,
                            payload: reply.payload,
                        },
                    )
                    .await;
                }
            }
            Err(error) => {
                warn!(%error, "functional receiver failed to handle packet");
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

async fn send_outbound(command_tx: &mpsc::Sender<EmbeddedNodeCommand>, outbound: ServiceOutbound) {
    let (respond_to, response_rx) = oneshot::channel();
    if command_tx
        .send(EmbeddedNodeCommand::SendServiceData {
            outbound,
            respond_to: Some(respond_to),
        })
        .await
        .is_err()
    {
        debug!("functional receiver reply dropped because command loop is closed");
        return;
    }

    match response_rx.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!(%error, "functional receiver reply could not be sent"),
        Err(_) => debug!("functional receiver reply result was dropped"),
    }
}

fn harness_config(
    identity: &Identity,
    peer_npub: Option<&str>,
    relays: &[String],
    stun_servers: &[String],
    app: &str,
    share_local_candidates: bool,
) -> Config {
    let mut config = Config::default();
    config.node.identity.nsec = Some(encode_nsec(&identity.keypair().secret_key()));
    config.node.identity.persistent = false;
    config.node.log_level = Some("debug".to_string());
    config.node.tick_interval_secs = 1;
    config.tun.enabled = false;
    config.dns.enabled = false;
    config.node.control.enabled = false;
    config.node.discovery.nostr.enabled = true;
    config.node.discovery.nostr.advertise = true;
    config.node.discovery.nostr.advert_relays = relays.to_vec();
    config.node.discovery.nostr.dm_relays = relays.to_vec();
    config.node.discovery.nostr.stun_servers = stun_servers.to_vec();
    config.node.discovery.nostr.share_local_candidates = share_local_candidates;
    config.node.discovery.nostr.app = app.to_string();
    config.node.discovery.nostr.signal_ttl_secs = 120;
    config.node.discovery.nostr.attempt_timeout_secs = 15;
    config.node.discovery.nostr.policy = NostrDiscoveryPolicy::ConfiguredOnly;
    config.transports.udp = TransportInstances::Single(UdpConfig {
        bind_addr: Some("0.0.0.0:0".to_string()),
        advertise_on_nostr: Some(true),
        public: Some(false),
        ..UdpConfig::default()
    });

    if let Some(peer_npub) = peer_npub {
        config.peers.push(PeerConfig {
            npub: peer_npub.to_string(),
            via_nostr: true,
            ..PeerConfig::default()
        });
    }

    config
}

async fn dump_statuses(sender: &FipsMobileClient, receiver: &ReceiverHarness) {
    match sender.status().await {
        Ok(status) => eprintln!("sender status after failure: {status:?}"),
        Err(error) => eprintln!("sender status unavailable after failure: {error}"),
    }
    match receiver.status().await {
        Ok(status) => eprintln!("receiver status after failure: {status:?}"),
        Err(error) => eprintln!("receiver status unavailable after failure: {error}"),
    }
}

fn deterministic_payload(len: usize, salt: u8) -> Vec<u8> {
    (0..len)
        .map(|index| {
            let mixed = index
                .wrapping_mul(31)
                .wrapping_add((salt as usize).wrapping_mul(17))
                .wrapping_add(11);
            (mixed & 0xff) as u8
        })
        .collect()
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn init_logging() {
    let default_filter =
        "info,fips=debug,fips_drop_functional=debug,nostr_relay_pool=info,nostr_sdk=info";
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| default_filter.to_string());
    fmt()
        .with_env_filter(EnvFilter::builder().parse_lossy(filter))
        .with_target(true)
        .with_thread_ids(false)
        .with_thread_names(false)
        .init();
}

fn json_summary(
    app: &str,
    receiver_npub: &str,
    sender_npub: &str,
    outcomes: &[RunOutcome],
) -> String {
    let runs: Vec<_> = outcomes
        .iter()
        .map(|outcome| {
            serde_json::json!({
                "run": outcome.run,
                "file_name": outcome.file_name,
                "bytes": outcome.bytes,
                "sha256": outcome.sha256,
                "path": outcome.path,
            })
        })
        .collect();
    serde_json::json!({
        "ok": true,
        "app": app,
        "receiver_npub": receiver_npub,
        "sender_npub": sender_npub,
        "runs": runs,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn args_parse_defaults_and_lists() {
        let args = Args::try_parse_from(["fips-drop-functional"]).expect("args parse");

        assert_eq!(args.runs, 1);
        assert_eq!(args.payload_bytes, 192 * 1024);
        assert_eq!(args.connect_timeout_ms, 90_000);
        assert_eq!(args.transfer_timeout_ms, 120_000);
        assert_eq!(args.relays.len(), 3);
        assert_eq!(args.stun_servers.len(), 3);
        assert!(args.storage_root.is_none());
        assert!(!args.keep_artifacts);
        assert!(!args.no_local_candidates);
        assert!(!args.json);
    }

    #[test]
    fn args_parse_overrides() {
        let args = Args::try_parse_from([
            "fips-drop-functional",
            "--runs",
            "2",
            "--payload-bytes",
            "64",
            "--connect-timeout-ms",
            "1000",
            "--transfer-timeout-ms",
            "2000",
            "--relays",
            "wss://a.example,wss://b.example",
            "--stun-servers",
            "stun:a.example:3478,stun:b.example:3478",
            "--app",
            "test-app",
            "--storage-root",
            "/tmp/fips-drop-functional",
            "--keep-artifacts",
            "--no-local-candidates",
            "--json",
        ])
        .expect("args parse");

        assert_eq!(args.runs, 2);
        assert_eq!(args.payload_bytes, 64);
        assert_eq!(args.connect_timeout_ms, 1000);
        assert_eq!(args.transfer_timeout_ms, 2000);
        assert_eq!(args.relays, ["wss://a.example", "wss://b.example"]);
        assert_eq!(
            args.stun_servers,
            ["stun:a.example:3478", "stun:b.example:3478"]
        );
        assert_eq!(args.app.as_deref(), Some("test-app"));
        assert_eq!(
            args.storage_root.as_deref(),
            Some(std::path::Path::new("/tmp/fips-drop-functional"))
        );
        assert!(args.keep_artifacts);
        assert!(args.no_local_candidates);
        assert!(args.json);
    }

    #[test]
    fn deterministic_payload_is_stable_and_salted() {
        assert_eq!(deterministic_payload(0, 0), Vec::<u8>::new());
        assert_eq!(
            deterministic_payload(8, 0),
            vec![11, 42, 73, 104, 135, 166, 197, 228]
        );
        assert_eq!(
            deterministic_payload(8, 1),
            vec![28, 59, 90, 121, 152, 183, 214, 245]
        );
    }

    #[test]
    fn harness_config_is_mobile_safe_and_nostr_enabled() {
        let identity = Identity::generate();
        let peer = Identity::generate().npub();
        let relays = vec!["wss://relay.example".to_string()];
        let stun = vec!["stun:stun.example:3478".to_string()];

        let config = harness_config(&identity, Some(&peer), &relays, &stun, "test-app", false);

        assert!(!config.tun.enabled);
        assert!(!config.dns.enabled);
        assert!(!config.node.control.enabled);
        assert_eq!(config.node.discovery.nostr.app, "test-app");
        assert_eq!(config.node.discovery.nostr.advert_relays, relays);
        assert_eq!(config.node.discovery.nostr.dm_relays, relays);
        assert_eq!(config.node.discovery.nostr.stun_servers, stun);
        assert!(!config.node.discovery.nostr.share_local_candidates);
        assert_eq!(config.peers.len(), 1);
        assert_eq!(config.peers[0].npub, peer);
        assert!(config.peers[0].via_nostr);
    }

    #[test]
    fn json_summary_contains_transfer_outcomes() {
        let outcomes = vec![RunOutcome {
            run: 1,
            file_name: "file.bin".to_string(),
            bytes: 3,
            sha256: "abc123".to_string(),
            path: PathBuf::from("/tmp/file.bin"),
        }];

        let value: serde_json::Value =
            serde_json::from_str(&json_summary("app", "receiver", "sender", &outcomes))
                .expect("json");

        assert_eq!(value["ok"], true);
        assert_eq!(value["app"], "app");
        assert_eq!(value["receiver_npub"], "receiver");
        assert_eq!(value["sender_npub"], "sender");
        assert_eq!(value["runs"][0]["run"], 1);
        assert_eq!(value["runs"][0]["file_name"], "file.bin");
        assert_eq!(value["runs"][0]["bytes"], 3);
        assert_eq!(value["runs"][0]["sha256"], "abc123");
        assert_eq!(value["runs"][0]["path"], "/tmp/file.bin");
    }
}
