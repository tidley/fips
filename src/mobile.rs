//! Mobile-facing embedded FIPS facade.
//!
//! This is the Rust boundary intended for Android/Flutter wrappers. It owns a
//! node in-process, keeps TUN/DNS/control disabled by default, and exposes the
//! small set of operations a mobile app needs for the first Dropbox-style PoC.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use crate::config::{PeerConfig, TransportInstances, UdpConfig};
use crate::dropbox::{DROPBOX_SERVICE_PORT, DropboxMessage, DropboxResult, encode_b64, sha256_hex};
use crate::{
    Config, EmbeddedNodeCommand, EmbeddedNodeStatus, Node, NodeError, PeerIdentity,
    ServiceOutbound, ServicePacket,
};

/// Default mobile reply port for app-owned FSP service traffic.
pub const MOBILE_RESPONSE_PORT: u16 = 49_152;

/// Mobile runtime configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FipsMobileConfig {
    pub config: Config,
    #[serde(default = "default_response_port")]
    pub response_port: u16,
    #[serde(default = "default_queue_depth")]
    pub queue_depth: usize,
}

impl FipsMobileConfig {
    /// Create a mobile config from a normal FIPS config.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            response_port: default_response_port(),
            queue_depth: default_queue_depth(),
        }
    }
}

/// Embedded mobile client handle.
pub struct FipsMobileClient {
    command_tx: mpsc::Sender<EmbeddedNodeCommand>,
    inbound_rx: mpsc::Receiver<ServicePacket>,
    join: tokio::task::JoinHandle<Result<(), NodeError>>,
    response_port: u16,
}

#[derive(Debug, Error)]
pub enum FipsMobileError {
    #[error("failed to parse mobile config yaml: {0}")]
    ConfigYaml(#[from] serde_yaml::Error),
    #[error("invalid peer npub: {0}")]
    InvalidPeerNpub(#[from] crate::IdentityError),
    #[error("node error: {0}")]
    Node(#[from] NodeError),
    #[error("embedded node command loop is closed")]
    CommandLoopClosed,
    #[error("embedded node response was dropped")]
    ResponseDropped,
    #[error("embedded command failed: {0}")]
    Command(String),
    #[error("dropbox payload error: {0}")]
    Dropbox(#[from] crate::dropbox::DropboxError),
    #[error("mobile node task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("service session not ready for {npub} after {timeout_ms}ms")]
    ServiceSessionTimeout { npub: String, timeout_ms: u64 },
}

impl FipsMobileClient {
    /// Start an in-process FIPS node for a mobile runtime.
    pub async fn start(config: FipsMobileConfig) -> Result<Self, FipsMobileError> {
        let node_config = prepare_mobile_config(config.config);
        let response_port = config.response_port;
        let queue_depth = config.queue_depth;

        let mut node = Node::new(node_config)?;
        let inbound_rx = node.register_service_port(response_port, queue_depth)?;
        node.start().await?;

        let (command_tx, command_rx) = mpsc::channel(queue_depth.max(8));
        let join = tokio::spawn(async move {
            let loop_result = node.run_embedded_loop(command_rx).await;
            let stop_result = node.stop().await;
            match (loop_result, stop_result) {
                (Err(loop_error), _) => Err(loop_error),
                (Ok(()), Err(stop_error)) => Err(stop_error),
                (Ok(()), Ok(())) => Ok(()),
            }
        });

        Ok(Self {
            command_tx,
            inbound_rx,
            join,
            response_port,
        })
    }

    /// Start from a YAML config string.
    pub async fn start_from_yaml(
        config_yaml: &str,
        response_port: u16,
    ) -> Result<Self, FipsMobileError> {
        let config = serde_yaml::from_str(config_yaml)?;
        Self::start(FipsMobileConfig {
            config,
            response_port,
            queue_depth: default_queue_depth(),
        })
        .await
    }

    /// Request Nostr-based traversal to a known npub.
    pub async fn connect_npub(&self, npub: impl Into<String>) -> Result<(), FipsMobileError> {
        let peer_config = PeerConfig {
            npub: npub.into(),
            via_nostr: true,
            ..PeerConfig::default()
        };
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(EmbeddedNodeCommand::RequestNostrBootstrap {
                peer_config,
                respond_to: Some(tx),
            })
            .await
            .map_err(|_| FipsMobileError::CommandLoopClosed)?;
        rx.await
            .map_err(|_| FipsMobileError::ResponseDropped)?
            .map_err(FipsMobileError::Command)
    }

    /// Queue a Nostr traversal request without waiting for an immediate result.
    pub async fn queue_connect_npub(&self, npub: impl Into<String>) -> Result<(), FipsMobileError> {
        let peer_config = PeerConfig {
            npub: npub.into(),
            via_nostr: true,
            ..PeerConfig::default()
        };
        self.command_tx
            .send(EmbeddedNodeCommand::RequestNostrBootstrap {
                peer_config,
                respond_to: None,
            })
            .await
            .map_err(|_| FipsMobileError::CommandLoopClosed)
    }

    /// Initiate an end-to-end FSP session to a known npub.
    pub async fn ensure_session_npub(&self, npub: &str) -> Result<(), FipsMobileError> {
        let peer_identity = PeerIdentity::from_npub(npub)?;
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(EmbeddedNodeCommand::EnsureServiceSession {
                peer_identity,
                respond_to: Some(tx),
            })
            .await
            .map_err(|_| FipsMobileError::CommandLoopClosed)?;
        rx.await
            .map_err(|_| FipsMobileError::ResponseDropped)?
            .map_err(FipsMobileError::Command)
    }

    /// Wait until the FSP service session to a known npub is established.
    pub async fn wait_for_session_npub(
        &self,
        npub: &str,
        timeout: Duration,
    ) -> Result<(), FipsMobileError> {
        let peer_identity = PeerIdentity::from_npub(npub)?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.has_service_session(*peer_identity.node_addr()).await? {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(FipsMobileError::ServiceSessionTimeout {
                    npub: npub.to_string(),
                    timeout_ms: timeout.as_millis() as u64,
                });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Send one Dropbox-style blob to a remote node's port 4242 receiver.
    pub async fn send_dropbox_blob_to_npub(
        &self,
        npub: &str,
        name: &str,
        mime: Option<String>,
        data: &[u8],
    ) -> Result<(), FipsMobileError> {
        let peer_identity = PeerIdentity::from_npub(npub)?;
        let message = build_dropbox_put_message(name, mime, data)?;
        let outbound = ServiceOutbound {
            dest_addr: *peer_identity.node_addr(),
            src_port: self.response_port,
            dst_port: DROPBOX_SERVICE_PORT,
            payload: message.to_payload()?,
        };
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(EmbeddedNodeCommand::SendServiceData {
                outbound,
                respond_to: Some(tx),
            })
            .await
            .map_err(|_| FipsMobileError::CommandLoopClosed)?;
        rx.await
            .map_err(|_| FipsMobileError::ResponseDropped)?
            .map_err(FipsMobileError::Command)
    }

    /// Receive the next app service packet.
    pub async fn recv_service_packet(&mut self) -> Option<ServicePacket> {
        self.inbound_rx.recv().await
    }

    /// Return a compact status snapshot.
    pub async fn status(&self) -> Result<EmbeddedNodeStatus, FipsMobileError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(EmbeddedNodeCommand::Status { respond_to: tx })
            .await
            .map_err(|_| FipsMobileError::CommandLoopClosed)?;
        rx.await.map_err(|_| FipsMobileError::ResponseDropped)
    }

    /// Stop the embedded node.
    pub async fn stop(self) -> Result<(), FipsMobileError> {
        let _ = self.command_tx.send(EmbeddedNodeCommand::Stop).await;
        self.join.await??;
        Ok(())
    }

    async fn has_service_session(
        &self,
        dest_addr: crate::NodeAddr,
    ) -> Result<bool, FipsMobileError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(EmbeddedNodeCommand::HasServiceSession {
                dest_addr,
                respond_to: tx,
            })
            .await
            .map_err(|_| FipsMobileError::CommandLoopClosed)?;
        rx.await.map_err(|_| FipsMobileError::ResponseDropped)
    }
}

/// Build the single-payload Dropbox message used by the Android PoC.
pub fn build_dropbox_put_message(
    name: &str,
    mime: Option<String>,
    data: &[u8],
) -> DropboxResult<DropboxMessage> {
    Ok(DropboxMessage::Put {
        id: format!("mobile-{}", now_millis()),
        name: name.to_string(),
        mime,
        sha256: Some(sha256_hex(data)),
        size: Some(data.len() as u64),
        data_b64: encode_b64(data),
    })
}

fn prepare_mobile_config(mut config: Config) -> Config {
    config.tun.enabled = false;
    config.dns.enabled = false;
    config.node.control.enabled = false;

    if config.transports.udp.is_empty() {
        let advertise_on_nostr = config.node.discovery.nostr.enabled;
        config.transports.udp = TransportInstances::Single(UdpConfig {
            bind_addr: Some("0.0.0.0:0".to_string()),
            advertise_on_nostr: Some(advertise_on_nostr),
            public: Some(false),
            ..UdpConfig::default()
        });
    }

    config
}

fn default_response_port() -> u16 {
    MOBILE_RESPONSE_PORT
}

fn default_queue_depth() -> usize {
    128
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;
    use crate::dropbox::{DropboxMessage, sha256_hex};

    fn peer_npub() -> String {
        Identity::generate().npub()
    }

    #[test]
    fn build_dropbox_put_message_sets_hash_size_and_payload() {
        let data = b"android test blob";
        let message =
            build_dropbox_put_message("photo.jpg", Some("image/jpeg".to_string()), data).unwrap();

        if let DropboxMessage::Put {
            name,
            mime,
            sha256,
            size,
            data_b64,
            ..
        } = message
        {
            assert_eq!(name, "photo.jpg");
            assert_eq!(mime, Some("image/jpeg".to_string()));
            assert_eq!(sha256, Some(sha256_hex(data)));
            assert_eq!(size, Some(data.len() as u64));
            assert_eq!(data_b64, encode_b64(data));
        }
    }

    #[tokio::test]
    async fn mobile_client_starts_reports_status_and_stops() {
        let config = FipsMobileConfig::new(Config::default());
        let client = FipsMobileClient::start(config).await.unwrap();

        let status = client.status().await.unwrap();
        assert_eq!(status.state, "running");
        assert_eq!(status.tun_state, "disabled");

        client.stop().await.unwrap();
    }

    #[tokio::test]
    async fn start_from_yaml_rejects_bad_yaml() {
        let result = FipsMobileClient::start_from_yaml("node: [", MOBILE_RESPONSE_PORT).await;

        assert!(matches!(result, Err(FipsMobileError::ConfigYaml(_))));
    }

    #[tokio::test]
    async fn start_from_yaml_runs_minimal_config() {
        let client = FipsMobileClient::start_from_yaml(
            r#"
node:
  identity:
    persistent: false
tun:
  enabled: true
dns:
  enabled: true
"#,
            MOBILE_RESPONSE_PORT + 1,
        )
        .await
        .unwrap();

        let status = client.status().await.unwrap();
        assert_eq!(status.state, "running");
        assert_eq!(status.tun_state, "disabled");

        client.stop().await.unwrap();
    }

    #[tokio::test]
    async fn connect_npub_reports_missing_runtime() {
        let client = FipsMobileClient::start(FipsMobileConfig::new(Config::default()))
            .await
            .unwrap();

        let err = client.connect_npub(peer_npub()).await.unwrap_err();

        assert!(
            matches!(err, FipsMobileError::Command(reason) if reason.contains("runtime is not running") || reason.contains("compiled without nostr-discovery"))
        );
        client.stop().await.unwrap();
    }

    #[tokio::test]
    async fn queue_connect_npub_only_requires_open_command_loop() {
        let client = FipsMobileClient::start(FipsMobileConfig::new(Config::default()))
            .await
            .unwrap();

        client.queue_connect_npub(peer_npub()).await.unwrap();
        client.stop().await.unwrap();
    }

    #[tokio::test]
    async fn ensure_session_rejects_invalid_npub() {
        let client = FipsMobileClient::start(FipsMobileConfig::new(Config::default()))
            .await
            .unwrap();

        let err = client.ensure_session_npub("not-an-npub").await.unwrap_err();

        assert!(matches!(err, FipsMobileError::InvalidPeerNpub(_)));
        client.stop().await.unwrap();
    }

    #[tokio::test]
    async fn ensure_session_reports_no_route_for_unknown_peer() {
        let client = FipsMobileClient::start(FipsMobileConfig::new(Config::default()))
            .await
            .unwrap();

        let err = client.ensure_session_npub(&peer_npub()).await.unwrap_err();

        assert!(
            matches!(err, FipsMobileError::Command(reason) if reason.contains("no route") || reason.contains("no transport"))
        );
        client.stop().await.unwrap();
    }

    #[tokio::test]
    async fn wait_for_session_times_out_when_not_established() {
        let client = FipsMobileClient::start(FipsMobileConfig::new(Config::default()))
            .await
            .unwrap();
        let npub = peer_npub();

        let err = client
            .wait_for_session_npub(&npub, Duration::from_millis(1))
            .await
            .unwrap_err();

        assert!(matches!(err, FipsMobileError::ServiceSessionTimeout { .. }));
        client.stop().await.unwrap();
    }

    #[tokio::test]
    async fn send_dropbox_blob_rejects_invalid_npub_before_payload_send() {
        let client = FipsMobileClient::start(FipsMobileConfig::new(Config::default()))
            .await
            .unwrap();

        let err = client
            .send_dropbox_blob_to_npub("not-an-npub", "note.txt", None, b"hello")
            .await
            .unwrap_err();

        assert!(matches!(err, FipsMobileError::InvalidPeerNpub(_)));
        client.stop().await.unwrap();
    }

    #[tokio::test]
    async fn send_dropbox_blob_reports_missing_session() {
        let client = FipsMobileClient::start(FipsMobileConfig::new(Config::default()))
            .await
            .unwrap();

        let err = client
            .send_dropbox_blob_to_npub(&peer_npub(), "note.txt", None, b"hello")
            .await
            .unwrap_err();

        assert!(matches!(err, FipsMobileError::Command(reason) if reason.contains("no session")));
        client.stop().await.unwrap();
    }

    #[tokio::test]
    async fn recv_service_packet_waits_for_inbound_data() {
        let mut client = FipsMobileClient::start(FipsMobileConfig::new(Config::default()))
            .await
            .unwrap();

        let result =
            tokio::time::timeout(Duration::from_millis(1), client.recv_service_packet()).await;

        assert!(result.is_err());
        client.stop().await.unwrap();
    }
}
