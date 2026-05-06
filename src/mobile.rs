//! Mobile-facing embedded FIPS facade.
//!
//! This is the Rust boundary intended for Android/Flutter wrappers. It owns a
//! node in-process, keeps TUN/DNS/control disabled by default, and exposes the
//! small set of operations a mobile app needs for the first FIPS Drop PoC.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use crate::config::{PeerConfig, TransportInstances, UdpConfig};
use crate::dropbox::{
    DROPBOX_BLOB_CHUNK_DATA_BYTES, DROPBOX_SERVICE_PORT, DropboxMessage, DropboxResult, encode_b64,
    sha256_hex,
};
use crate::{
    Config, EmbeddedNodeCommand, EmbeddedNodeStatus, Node, NodeAddr, NodeError, PeerIdentity,
    ServiceOutbound, ServicePacket,
};

/// Default mobile reply port for app-owned FSP service traffic.
pub const MOBILE_RESPONSE_PORT: u16 = 49_152;

/// Maximum serialized Dropbox payload to send as a single service packet.
pub const DROPBOX_INLINE_PAYLOAD_BYTES: usize = 1_024;

/// Raw file bytes per binary blob chunk. Keeps each FIPS service payload well under MTU.
pub const DROPBOX_CHUNK_DATA_BYTES: usize = DROPBOX_BLOB_CHUNK_DATA_BYTES;

/// Android PoC maximum file size.
pub const DROPBOX_MAX_FILE_BYTES: usize = 10 * 1024 * 1024;

/// Number of FIPS Drop chunks sent before waiting for receiver ACKs.
pub const DROPBOX_CHUNK_WINDOW_SIZE: usize = 16;

/// Number of binary blob chunks sent before asking the receiver for a sparse report.
pub const DROPBOX_BLOB_WINDOW_SIZE: usize = 32;

const DROPBOX_BLOB_MIN_WINDOW_SIZE: usize = 8;
const DROPBOX_BLOB_MAX_WINDOW_SIZE: usize = 64;

/// Number of missing binary blob chunks to repair before asking for a fresh report.
pub const DROPBOX_BLOB_REPAIR_BATCH_SIZE: usize = 8;

const DROPBOX_BLOB_MIN_REPAIR_BATCH_SIZE: usize = 4;
const DROPBOX_BLOB_MAX_REPAIR_BATCH_SIZE: usize = 16;
const DROPBOX_BLOB_DEFAULT_CHUNK_SPACING_MS: u64 = 6;
const DROPBOX_BLOB_MIN_CHUNK_SPACING_MS: u64 = 3;
const DROPBOX_BLOB_MAX_CHUNK_SPACING_MS: u64 = 20;
const DROPBOX_BLOB_DEFAULT_WINDOW_SPACING_MS: u64 = 50;
const DROPBOX_BLOB_CLEAN_WINDOWS_BEFORE_GROW: u8 = 3;

/// Number of times to retry missing FIPS Drop chunks.
pub const DROPBOX_MAX_SEND_ATTEMPTS: usize = 10;

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
    #[error(
        "FIPS route to {npub} ({node_addr}) is not ready: {reason}. Connect first and wait for a peer/session before sending."
    )]
    RouteUnavailable {
        npub: String,
        node_addr: NodeAddr,
        reason: String,
    },
    #[error("file-transfer payload error: {0}")]
    Dropbox(#[from] crate::dropbox::DropboxError),
    #[error("file-transfer file too large: {size} bytes > max {max} bytes")]
    DropboxFileTooLarge { size: usize, max: usize },
    #[error("file-transfer receiver closed before ack")]
    DropboxAckChannelClosed,
    #[error("file-transfer ack timeout for transfer {id}: missing {missing:?}")]
    DropboxAckTimeout { id: String, missing: Vec<String> },
    #[error("file receiver error for transfer {id:?}: {reason}")]
    DropboxRemoteError { id: Option<String>, reason: String },
    #[error("mobile node task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("FIPS connection to {npub} was not ready after {timeout_ms}ms. {detail}")]
    ServiceSessionTimeout {
        npub: String,
        timeout_ms: u64,
        detail: String,
    },
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
        let mut next_session_attempt = tokio::time::Instant::now();
        let mut last_route_error = None;
        self.queue_connect_npub(npub.to_string()).await?;

        loop {
            if self.has_service_session(*peer_identity.node_addr()).await? {
                return Ok(());
            }

            let now = tokio::time::Instant::now();
            if now >= deadline {
                let detail = match self.status().await {
                    Ok(status) => {
                        let last_error = status
                            .last_connect_error
                            .as_deref()
                            .or(last_route_error.as_deref())
                            .unwrap_or(
                                "no lower-level connect error was reported before the wait expired",
                            );
                        format!(
                            "Current node state: peers={}, links={}, pending_connections={}, sessions={}. Last connect/session detail: {last_error}",
                            status.peer_count,
                            status.link_count,
                            status.connection_count,
                            status.session_count,
                        )
                    }
                    Err(status_err) => {
                        let last_error = last_route_error.as_deref().unwrap_or(
                            "no lower-level connect error was reported before the wait expired",
                        );
                        format!(
                            "Could not read node status after timeout ({status_err}). Last session detail: {last_error}"
                        )
                    }
                };
                return Err(FipsMobileError::ServiceSessionTimeout {
                    npub: npub.to_string(),
                    timeout_ms: timeout.as_millis() as u64,
                    detail,
                });
            }

            if now >= next_session_attempt {
                match self.ensure_session_npub(npub).await {
                    Ok(()) => {}
                    Err(err) if err.is_pending_route_error() => {
                        last_route_error = Some(err.to_string());
                    }
                    Err(err) => return Err(err),
                }
                next_session_attempt = now + Duration::from_millis(1_000);
            }

            tokio::time::sleep(Duration::from_millis(100).min(deadline - now)).await;
        }
    }

    /// Send one FIPS Drop blob to a remote node's port 4242 receiver.
    pub async fn send_dropbox_blob_to_npub(
        &mut self,
        npub: &str,
        name: &str,
        mime: Option<String>,
        data: &[u8],
    ) -> Result<(), FipsMobileError> {
        validate_dropbox_file_size(data.len())?;
        let peer_identity = PeerIdentity::from_npub(npub)?;
        let messages = build_dropbox_messages_for_blob(name, mime, data)?;
        let dest_addr = *peer_identity.node_addr();
        if !self.has_service_session(dest_addr).await? {
            return Err(FipsMobileError::RouteUnavailable {
                npub: npub.to_string(),
                node_addr: dest_addr,
                reason: "no active FIPS service session to target".to_string(),
            });
        }
        self.send_dropbox_messages_to_addr(dest_addr, messages)
            .await
            .map_err(|err| err.with_route_context(npub, dest_addr))
    }

    async fn send_dropbox_messages_to_addr(
        &mut self,
        dest_addr: NodeAddr,
        messages: Vec<DropboxMessage>,
    ) -> Result<(), FipsMobileError> {
        match messages.as_slice() {
            [message @ DropboxMessage::Put { id, .. }] => {
                self.send_dropbox_message(dest_addr, message.clone())
                    .await?;
                self.wait_for_dropbox_ack_or_timeout(
                    dest_addr,
                    id,
                    [DROPBOX_ACK_STORED.to_string()],
                    dropbox_ack_timeout(),
                )
                .await
            }
            [DropboxMessage::BlobStart { id, .. }, ..] => {
                self.send_dropbox_blob_transfer(dest_addr, id, &messages)
                    .await
            }
            _ => {
                let Some(id) = messages.first().and_then(dropbox_message_id) else {
                    return Ok(());
                };
                let chunks = messages
                    .iter()
                    .filter_map(|message| match message {
                        DropboxMessage::PutChunk { .. } => Some(message.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let done = messages
                    .iter()
                    .find(|message| matches!(message, DropboxMessage::PutDone { .. }))
                    .cloned();

                self.send_dropbox_chunks_with_acks(dest_addr, id, &chunks)
                    .await?;
                if let Some(done) = done {
                    self.send_dropbox_message(dest_addr, done).await?;
                    self.wait_for_dropbox_ack_or_timeout(
                        dest_addr,
                        id,
                        [DROPBOX_ACK_STORED.to_string()],
                        dropbox_ack_timeout(),
                    )
                    .await?;
                }
                Ok(())
            }
        }
    }

    async fn send_dropbox_message(
        &self,
        dest_addr: crate::NodeAddr,
        message: DropboxMessage,
    ) -> Result<(), FipsMobileError> {
        let outbound = ServiceOutbound {
            dest_addr,
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

    async fn send_dropbox_blob_transfer(
        &mut self,
        dest_addr: NodeAddr,
        id: &str,
        messages: &[DropboxMessage],
    ) -> Result<(), FipsMobileError> {
        let Some(start) = messages
            .iter()
            .find(|message| matches!(message, DropboxMessage::BlobStart { .. }))
            .cloned()
        else {
            return Err(FipsMobileError::DropboxAckTimeout {
                id: id.to_string(),
                missing: vec!["blob:start".to_string()],
            });
        };
        let chunks = messages
            .iter()
            .filter_map(|message| match message {
                DropboxMessage::BlobChunk { chunk_index, .. } => {
                    Some((*chunk_index, message.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let Some(done) = messages
            .iter()
            .find(|message| matches!(message, DropboxMessage::BlobDone { .. }))
            .cloned()
        else {
            return Err(FipsMobileError::DropboxAckTimeout {
                id: id.to_string(),
                missing: vec!["blob:done".to_string()],
            });
        };

        self.send_dropbox_start_until_ready(dest_addr, id, start)
            .await?;

        let mut sent_through = None;
        let mut offset = 0;
        let mut tuning = DropboxBlobTransferTuning::default();
        while offset < chunks.len() {
            let end = (offset + tuning.window_size()).min(chunks.len());
            let window = &chunks[offset..end];

            self.send_dropbox_blob_chunk_window(dest_addr, window, tuning.chunk_spacing())
                .await?;
            let Some(window_highest) = window.last().map(|(chunk_index, _)| *chunk_index) else {
                offset = end;
                continue;
            };
            sent_through = Some(window_highest);

            match self
                .request_dropbox_blob_report(dest_addr, id, &done)
                .await?
            {
                DropboxBlobReport::Stored => return Ok(()),
                DropboxBlobReport::Missing(missing) => {
                    let missing_sent = missing_chunks_at_or_below(&missing, window_highest);
                    tuning.record_window_result(window.len(), missing_sent.len());
                    if !missing_sent.is_empty() {
                        match self
                            .repair_dropbox_blob_chunks(
                                dest_addr,
                                id,
                                &chunks,
                                &done,
                                window_highest,
                                missing_sent,
                                &tuning,
                            )
                            .await?
                        {
                            DropboxBlobReport::Stored => return Ok(()),
                            DropboxBlobReport::Missing(missing)
                                if missing_chunks_at_or_below(&missing, window_highest)
                                    .is_empty() => {}
                            DropboxBlobReport::Missing(missing) => {
                                return Err(FipsMobileError::DropboxAckTimeout {
                                    id: id.to_string(),
                                    missing: missing
                                        .into_iter()
                                        .map(|chunk_index| format!("chunk:{chunk_index}"))
                                        .collect(),
                                });
                            }
                            DropboxBlobReport::Timeout => {
                                return Err(FipsMobileError::DropboxAckTimeout {
                                    id: id.to_string(),
                                    missing: vec![format!("chunk:{window_highest}")],
                                });
                            }
                        }
                    }
                }
                DropboxBlobReport::Timeout => {
                    tuning.record_timeout();
                    return Err(FipsMobileError::DropboxAckTimeout {
                        id: id.to_string(),
                        missing: vec![format!("chunk:{window_highest}")],
                    });
                }
            }

            offset = end;
            tokio::time::sleep(tuning.window_spacing()).await;
        }

        match self
            .request_dropbox_blob_report(dest_addr, id, &done)
            .await?
        {
            DropboxBlobReport::Stored => Ok(()),
            DropboxBlobReport::Missing(missing) => {
                let final_chunk = sent_through.unwrap_or_default();
                match self
                    .repair_dropbox_blob_chunks(
                        dest_addr,
                        id,
                        &chunks,
                        &done,
                        final_chunk,
                        missing,
                        &tuning,
                    )
                    .await?
                {
                    DropboxBlobReport::Stored => Ok(()),
                    DropboxBlobReport::Missing(missing) if missing.is_empty() => {
                        match self
                            .request_dropbox_blob_report(dest_addr, id, &done)
                            .await?
                        {
                            DropboxBlobReport::Stored => Ok(()),
                            DropboxBlobReport::Missing(missing) => {
                                Err(FipsMobileError::DropboxAckTimeout {
                                    id: id.to_string(),
                                    missing: missing
                                        .into_iter()
                                        .map(|chunk_index| format!("chunk:{chunk_index}"))
                                        .collect(),
                                })
                            }
                            DropboxBlobReport::Timeout => Err(FipsMobileError::DropboxAckTimeout {
                                id: id.to_string(),
                                missing: vec!["blob:stored_ack".to_string()],
                            }),
                        }
                    }
                    DropboxBlobReport::Missing(missing) => {
                        Err(FipsMobileError::DropboxAckTimeout {
                            id: id.to_string(),
                            missing: missing
                                .into_iter()
                                .map(|chunk_index| format!("chunk:{chunk_index}"))
                                .collect(),
                        })
                    }
                    DropboxBlobReport::Timeout => Err(FipsMobileError::DropboxAckTimeout {
                        id: id.to_string(),
                        missing: vec!["blob:stored_ack".to_string()],
                    }),
                }
            }
            DropboxBlobReport::Timeout => Err(FipsMobileError::DropboxAckTimeout {
                id: id.to_string(),
                missing: vec!["blob:stored_ack".to_string()],
            }),
        }
    }

    async fn send_dropbox_blob_chunk_window(
        &self,
        dest_addr: NodeAddr,
        window: &[(u32, DropboxMessage)],
        spacing: Duration,
    ) -> Result<(), FipsMobileError> {
        for (_, message) in window {
            self.send_dropbox_message(dest_addr, message.clone())
                .await?;
            tokio::time::sleep(spacing).await;
        }
        Ok(())
    }

    async fn request_dropbox_blob_report(
        &mut self,
        dest_addr: NodeAddr,
        id: &str,
        done: &DropboxMessage,
    ) -> Result<DropboxBlobReport, FipsMobileError> {
        for attempt in 0..DROPBOX_MAX_SEND_ATTEMPTS {
            self.send_dropbox_message(dest_addr, done.clone()).await?;
            match self
                .wait_for_dropbox_blob_report(dest_addr, id, dropbox_ack_timeout())
                .await?
            {
                DropboxBlobReport::Timeout if attempt + 1 < DROPBOX_MAX_SEND_ATTEMPTS => {}
                report => return Ok(report),
            }
        }
        Ok(DropboxBlobReport::Timeout)
    }

    async fn repair_dropbox_blob_chunks(
        &mut self,
        dest_addr: NodeAddr,
        id: &str,
        chunks: &[(u32, DropboxMessage)],
        done: &DropboxMessage,
        sent_through: u32,
        mut missing: Vec<u32>,
        tuning: &DropboxBlobTransferTuning,
    ) -> Result<DropboxBlobReport, FipsMobileError> {
        missing = missing_chunks_at_or_below(&missing, sent_through);
        for _ in 0..DROPBOX_MAX_SEND_ATTEMPTS {
            if missing.is_empty() {
                return Ok(DropboxBlobReport::Missing(Vec::new()));
            }

            let repair_batch = missing
                .iter()
                .copied()
                .take(tuning.repair_batch_size())
                .collect::<Vec<_>>();
            for missing_index in &repair_batch {
                if let Some((_, message)) = chunks
                    .iter()
                    .find(|(chunk_index, _)| chunk_index == missing_index)
                {
                    self.send_dropbox_message(dest_addr, message.clone())
                        .await?;
                    tokio::time::sleep(tuning.chunk_spacing()).await;
                }
            }

            match self
                .request_dropbox_blob_report(dest_addr, id, done)
                .await?
            {
                DropboxBlobReport::Stored => return Ok(DropboxBlobReport::Stored),
                DropboxBlobReport::Missing(next_missing) => {
                    missing = missing_chunks_at_or_below(&next_missing, sent_through);
                }
                DropboxBlobReport::Timeout => {}
            }
        }

        Ok(DropboxBlobReport::Missing(missing))
    }

    async fn send_dropbox_start_until_ready(
        &mut self,
        dest_addr: NodeAddr,
        id: &str,
        start: DropboxMessage,
    ) -> Result<(), FipsMobileError> {
        for attempt in 0..DROPBOX_MAX_SEND_ATTEMPTS {
            self.send_dropbox_message(dest_addr, start.clone()).await?;
            match self
                .wait_for_dropbox_blob_report(dest_addr, id, dropbox_ack_timeout())
                .await?
            {
                DropboxBlobReport::Stored | DropboxBlobReport::Missing(_) => return Ok(()),
                DropboxBlobReport::Timeout if attempt + 1 < DROPBOX_MAX_SEND_ATTEMPTS => {}
                DropboxBlobReport::Timeout => break,
            }
        }

        Err(FipsMobileError::DropboxAckTimeout {
            id: id.to_string(),
            missing: vec!["blob:start".to_string()],
        })
    }

    async fn wait_for_dropbox_blob_report(
        &mut self,
        dest_addr: NodeAddr,
        id: &str,
        timeout: Duration,
    ) -> Result<DropboxBlobReport, FipsMobileError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(DropboxBlobReport::Timeout);
            }
            match tokio::time::timeout(deadline - now, self.inbound_rx.recv()).await {
                Ok(Some(packet)) => {
                    if let Some(report) = parse_dropbox_blob_report_packet(
                        &packet,
                        dest_addr,
                        self.response_port,
                        id,
                    )? {
                        return Ok(report);
                    }
                }
                Ok(None) => return Err(FipsMobileError::DropboxAckChannelClosed),
                Err(_) => return Ok(DropboxBlobReport::Timeout),
            }
        }
    }

    async fn send_dropbox_chunks_with_acks(
        &mut self,
        dest_addr: NodeAddr,
        id: &str,
        chunks: &[DropboxMessage],
    ) -> Result<(), FipsMobileError> {
        let final_chunk_index = chunks.len().saturating_sub(1);
        let (regular_chunks, final_chunks) = chunks.split_at(final_chunk_index);

        for window in regular_chunks.chunks(DROPBOX_CHUNK_WINDOW_SIZE) {
            self.send_dropbox_chunk_window_with_acks(dest_addr, id, window)
                .await?;
        }

        if let Some(final_chunk) = final_chunks.first() {
            self.send_dropbox_chunk_window_with_acks(
                dest_addr,
                id,
                std::slice::from_ref(final_chunk),
            )
            .await?;
        }

        Ok(())
    }

    async fn send_dropbox_chunk_window_with_acks(
        &mut self,
        dest_addr: NodeAddr,
        id: &str,
        window: &[DropboxMessage],
    ) -> Result<(), FipsMobileError> {
        if !window.is_empty() {
            let mut missing = chunk_statuses(window);
            let mut attempt = 0;
            while !missing.is_empty() && attempt < DROPBOX_MAX_SEND_ATTEMPTS {
                attempt += 1;

                let messages_to_send: Vec<_> = window
                    .iter()
                    .filter(|message| {
                        dropbox_expected_ack_status(message)
                            .is_some_and(|status| missing.contains(&status))
                    })
                    .cloned()
                    .collect();

                let last_index = messages_to_send.len().saturating_sub(1);
                for (index, message) in messages_to_send.into_iter().enumerate() {
                    self.send_dropbox_message(dest_addr, message).await?;
                    if index < last_index {
                        tokio::time::sleep(dropbox_chunk_send_spacing()).await;
                    }
                }

                missing = self
                    .wait_for_dropbox_statuses(dest_addr, id, missing, dropbox_ack_timeout())
                    .await?;
            }

            if !missing.is_empty() {
                return Err(FipsMobileError::DropboxAckTimeout {
                    id: id.to_string(),
                    missing: missing.into_iter().collect(),
                });
            }
        }
        Ok(())
    }

    async fn wait_for_dropbox_ack_or_timeout(
        &mut self,
        dest_addr: NodeAddr,
        id: &str,
        statuses: impl IntoIterator<Item = String>,
        timeout: Duration,
    ) -> Result<(), FipsMobileError> {
        let missing = self
            .wait_for_dropbox_statuses(dest_addr, id, statuses.into_iter().collect(), timeout)
            .await?;
        if missing.is_empty() {
            Ok(())
        } else {
            Err(FipsMobileError::DropboxAckTimeout {
                id: id.to_string(),
                missing: missing.into_iter().collect(),
            })
        }
    }

    async fn wait_for_dropbox_statuses(
        &mut self,
        dest_addr: NodeAddr,
        id: &str,
        mut missing: BTreeSet<String>,
        timeout: Duration,
    ) -> Result<BTreeSet<String>, FipsMobileError> {
        let deadline = tokio::time::Instant::now() + timeout;
        while !missing.is_empty() {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            match tokio::time::timeout(deadline - now, self.inbound_rx.recv()).await {
                Ok(Some(packet)) => {
                    apply_dropbox_ack_packet(
                        &packet,
                        dest_addr,
                        self.response_port,
                        id,
                        &mut missing,
                    )?;
                }
                Ok(None) => return Err(FipsMobileError::DropboxAckChannelClosed),
                Err(_) => break,
            }
        }
        Ok(missing)
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

impl FipsMobileError {
    fn is_pending_route_error(&self) -> bool {
        match self {
            FipsMobileError::Command(reason) => {
                reason.contains("no route")
                    || reason.contains("no transport")
                    || reason.contains("no operational transport")
            }
            _ => false,
        }
    }

    fn is_send_route_error(&self) -> bool {
        match self {
            FipsMobileError::Command(reason) => {
                reason.contains("no route")
                    || reason.contains("no session")
                    || reason.contains("no transport")
                    || reason.contains("no operational transport")
            }
            _ => false,
        }
    }

    fn with_route_context(self, npub: &str, node_addr: NodeAddr) -> Self {
        if self.is_send_route_error() {
            return FipsMobileError::RouteUnavailable {
                npub: npub.to_string(),
                node_addr,
                reason: self.to_string(),
            };
        }
        self
    }
}

/// Build the single-payload Dropbox message used by the Android PoC.
pub fn build_dropbox_put_message(
    name: &str,
    mime: Option<String>,
    data: &[u8],
) -> DropboxResult<DropboxMessage> {
    build_dropbox_put_message_with_id(&new_dropbox_transfer_id(), name, mime, data)
}

fn build_dropbox_messages_for_blob(
    name: &str,
    mime: Option<String>,
    data: &[u8],
) -> DropboxResult<Vec<DropboxMessage>> {
    let id = new_dropbox_transfer_id();
    if data.len() <= DROPBOX_INLINE_PAYLOAD_BYTES {
        let put = build_dropbox_put_message_with_id(&id, name, mime.clone(), data)?;
        if put.to_payload()?.len() <= DROPBOX_INLINE_PAYLOAD_BYTES {
            return Ok(vec![put]);
        }
    }

    Ok(build_dropbox_chunk_messages(&id, name, mime, data))
}

fn build_dropbox_put_message_with_id(
    id: &str,
    name: &str,
    mime: Option<String>,
    data: &[u8],
) -> DropboxResult<DropboxMessage> {
    Ok(DropboxMessage::Put {
        id: id.to_string(),
        name: name.to_string(),
        mime,
        sha256: Some(sha256_hex(data)),
        size: Some(data.len() as u64),
        data_b64: encode_b64(data),
    })
}

fn build_dropbox_chunk_messages(
    id: &str,
    name: &str,
    mime: Option<String>,
    data: &[u8],
) -> Vec<DropboxMessage> {
    let chunk_count = u32::try_from(data.chunks(DROPBOX_CHUNK_DATA_BYTES).count())
        .expect("dropbox file size limit keeps chunk count within u32 range");
    let sha256 = sha256_hex(data);
    let mut messages = Vec::with_capacity(chunk_count as usize + 2);

    messages.push(DropboxMessage::BlobStart {
        id: id.to_string(),
        name: name.to_string(),
        mime: mime.clone(),
        sha256,
        size: data.len() as u64,
        chunk_size: DROPBOX_CHUNK_DATA_BYTES as u16,
        chunk_count,
    });

    for (index, chunk) in data.chunks(DROPBOX_CHUNK_DATA_BYTES).enumerate() {
        messages.push(DropboxMessage::BlobChunk {
            id: id.to_string(),
            chunk_index: index as u32,
            data: chunk.to_vec(),
        });
    }
    messages.push(DropboxMessage::BlobDone { id: id.to_string() });

    messages
}

const DROPBOX_ACK_STORED: &str = "stored";

fn dropbox_ack_timeout() -> Duration {
    Duration::from_secs(8)
}

#[derive(Clone, Debug)]
struct DropboxBlobTransferTuning {
    window_size: usize,
    repair_batch_size: usize,
    chunk_spacing_ms: u64,
    clean_windows: u8,
}

impl Default for DropboxBlobTransferTuning {
    fn default() -> Self {
        Self {
            window_size: DROPBOX_BLOB_WINDOW_SIZE,
            repair_batch_size: DROPBOX_BLOB_REPAIR_BATCH_SIZE,
            chunk_spacing_ms: DROPBOX_BLOB_DEFAULT_CHUNK_SPACING_MS,
            clean_windows: 0,
        }
    }
}

impl DropboxBlobTransferTuning {
    fn window_size(&self) -> usize {
        self.window_size
    }

    fn repair_batch_size(&self) -> usize {
        self.repair_batch_size
    }

    fn chunk_spacing(&self) -> Duration {
        Duration::from_millis(self.chunk_spacing_ms)
    }

    fn window_spacing(&self) -> Duration {
        Duration::from_millis(DROPBOX_BLOB_DEFAULT_WINDOW_SPACING_MS)
    }

    fn record_window_result(&mut self, sent_chunks: usize, missing_chunks: usize) {
        if sent_chunks == 0 {
            return;
        }

        if missing_chunks > 0 {
            self.clean_windows = 0;
            self.window_size = (self.window_size / 2).max(DROPBOX_BLOB_MIN_WINDOW_SIZE);
            self.repair_batch_size =
                (self.repair_batch_size / 2).max(DROPBOX_BLOB_MIN_REPAIR_BATCH_SIZE);
            self.chunk_spacing_ms =
                (self.chunk_spacing_ms + 2).min(DROPBOX_BLOB_MAX_CHUNK_SPACING_MS);
            return;
        }

        self.clean_windows = self.clean_windows.saturating_add(1);
        if self.clean_windows >= DROPBOX_BLOB_CLEAN_WINDOWS_BEFORE_GROW {
            self.clean_windows = 0;
            self.window_size = (self.window_size + 8).min(DROPBOX_BLOB_MAX_WINDOW_SIZE);
            self.repair_batch_size =
                (self.repair_batch_size + 2).min(DROPBOX_BLOB_MAX_REPAIR_BATCH_SIZE);
            self.chunk_spacing_ms = self
                .chunk_spacing_ms
                .saturating_sub(1)
                .max(DROPBOX_BLOB_MIN_CHUNK_SPACING_MS);
        }
    }

    fn record_timeout(&mut self) {
        self.clean_windows = 0;
        self.window_size = DROPBOX_BLOB_MIN_WINDOW_SIZE;
        self.repair_batch_size = DROPBOX_BLOB_MIN_REPAIR_BATCH_SIZE;
        self.chunk_spacing_ms = DROPBOX_BLOB_MAX_CHUNK_SPACING_MS;
    }
}

fn dropbox_chunk_send_spacing() -> Duration {
    Duration::from_millis(6)
}

fn missing_chunks_at_or_below(missing: &[u32], sent_through: u32) -> Vec<u32> {
    missing
        .iter()
        .copied()
        .filter(|chunk_index| *chunk_index <= sent_through)
        .collect()
}

fn dropbox_message_id(message: &DropboxMessage) -> Option<&str> {
    match message {
        DropboxMessage::Put { id, .. }
        | DropboxMessage::PutChunk { id, .. }
        | DropboxMessage::PutDone { id }
        | DropboxMessage::BlobStart { id, .. }
        | DropboxMessage::BlobChunk { id, .. }
        | DropboxMessage::BlobDone { id } => Some(id),
        DropboxMessage::Hello { .. }
        | DropboxMessage::BlobAck { .. }
        | DropboxMessage::Ack { .. }
        | DropboxMessage::Error { .. } => None,
    }
}

fn dropbox_expected_ack_status(message: &DropboxMessage) -> Option<String> {
    match message {
        DropboxMessage::Put { .. } | DropboxMessage::PutDone { .. } => {
            Some(DROPBOX_ACK_STORED.to_string())
        }
        DropboxMessage::PutChunk {
            chunk_index,
            chunk_count,
            ..
        } if chunk_index.saturating_add(1) == *chunk_count => Some(DROPBOX_ACK_STORED.to_string()),
        DropboxMessage::PutChunk { chunk_index, .. } => Some(format!("chunk:{chunk_index}")),
        DropboxMessage::Hello { .. }
        | DropboxMessage::BlobStart { .. }
        | DropboxMessage::BlobChunk { .. }
        | DropboxMessage::BlobDone { .. }
        | DropboxMessage::BlobAck { .. }
        | DropboxMessage::Ack { .. }
        | DropboxMessage::Error { .. } => None,
    }
}

fn chunk_statuses(messages: &[DropboxMessage]) -> BTreeSet<String> {
    messages
        .iter()
        .filter_map(dropbox_expected_ack_status)
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DropboxBlobReport {
    Stored,
    Missing(Vec<u32>),
    Timeout,
}

fn parse_dropbox_blob_report_packet(
    packet: &ServicePacket,
    expected_src_addr: NodeAddr,
    response_port: u16,
    transfer_id: &str,
) -> Result<Option<DropboxBlobReport>, FipsMobileError> {
    if packet.src_addr != expected_src_addr
        || packet.src_port != DROPBOX_SERVICE_PORT
        || packet.dst_port != response_port
    {
        return Ok(None);
    }

    match DropboxMessage::from_payload(&packet.payload)? {
        DropboxMessage::Ack { id, status, .. }
            if id == transfer_id && status == DROPBOX_ACK_STORED =>
        {
            Ok(Some(DropboxBlobReport::Stored))
        }
        DropboxMessage::BlobAck {
            id, missing_chunks, ..
        } if id == transfer_id => Ok(Some(DropboxBlobReport::Missing(missing_chunks))),
        DropboxMessage::Error { id, reason }
            if id.as_deref().is_none() || id.as_deref() == Some(transfer_id) =>
        {
            Err(FipsMobileError::DropboxRemoteError { id, reason })
        }
        _ => Ok(None),
    }
}

fn apply_dropbox_ack_packet(
    packet: &ServicePacket,
    expected_src_addr: NodeAddr,
    response_port: u16,
    transfer_id: &str,
    missing: &mut BTreeSet<String>,
) -> Result<(), FipsMobileError> {
    if packet.src_addr != expected_src_addr
        || packet.src_port != DROPBOX_SERVICE_PORT
        || packet.dst_port != response_port
    {
        return Ok(());
    }

    match DropboxMessage::from_payload(&packet.payload)? {
        DropboxMessage::Ack { id, status, .. } if id == transfer_id => {
            missing.remove(&status);
            Ok(())
        }
        DropboxMessage::Error { id, reason }
            if id.as_deref().is_none() || id.as_deref() == Some(transfer_id) =>
        {
            Err(FipsMobileError::DropboxRemoteError { id, reason })
        }
        _ => Ok(()),
    }
}

fn validate_dropbox_file_size(size: usize) -> Result<(), FipsMobileError> {
    if size > DROPBOX_MAX_FILE_BYTES {
        return Err(FipsMobileError::DropboxFileTooLarge {
            size,
            max: DROPBOX_MAX_FILE_BYTES,
        });
    }
    Ok(())
}

fn new_dropbox_transfer_id() -> String {
    format!("{:016x}", now_millis() as u64)
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
    use crate::dropbox::{DropboxMessage, DropboxReceiver, sha256_hex};

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

    #[test]
    fn build_dropbox_messages_uses_single_put_for_small_payload() {
        let messages = build_dropbox_messages_for_blob("note.txt", None, b"small").unwrap();

        assert_eq!(messages.len(), 1);
        assert!(matches!(messages[0], DropboxMessage::Put { .. }));
    }

    #[test]
    fn build_dropbox_messages_chunks_large_payload_and_finishes() {
        let data = vec![7u8; DROPBOX_CHUNK_DATA_BYTES * 2 + 13];
        let messages =
            build_dropbox_messages_for_blob("movie.bin", Some("video/mp4".to_string()), &data)
                .unwrap();

        assert_eq!(messages.len(), 5);
        let DropboxMessage::BlobStart {
            id,
            name,
            mime,
            sha256,
            size,
            chunk_size,
            chunk_count,
        } = &messages[0]
        else {
            panic!("expected blob start");
        };
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(name, "movie.bin");
        assert_eq!(mime, &Some("video/mp4".to_string()));
        assert_eq!(sha256, &sha256_hex(&data));
        assert_eq!(size, &(data.len() as u64));
        assert_eq!(*chunk_size as usize, DROPBOX_CHUNK_DATA_BYTES);
        assert_eq!(*chunk_count, 3);

        for (index, message) in messages.iter().skip(1).take(3).enumerate() {
            let DropboxMessage::BlobChunk {
                id,
                chunk_index,
                data: chunk,
            } = message
            else {
                panic!("expected blob chunk at index {index}");
            };
            assert_eq!(id.len(), 16);
            assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
            assert_eq!(*chunk_index, index as u32);
            assert_eq!(
                chunk.as_slice(),
                &data[index * DROPBOX_CHUNK_DATA_BYTES
                    ..((index + 1) * DROPBOX_CHUNK_DATA_BYTES).min(data.len())]
            );
            assert!(message.to_payload().unwrap().len() < 1_280);
        }

        assert!(matches!(
            messages.last(),
            Some(DropboxMessage::BlobDone { .. })
        ));
    }

    #[test]
    fn build_dropbox_messages_chunks_multimegabyte_payload_without_inline_encode() {
        let data = vec![3u8; 3 * 1024 * 1024];
        let messages = build_dropbox_messages_for_blob(
            "VID-20260505-WA0003.mp4",
            Some("video/mp4".into()),
            &data,
        )
        .unwrap();

        assert!(messages.len() > 1);
        assert!(matches!(messages[0], DropboxMessage::BlobStart { .. }));
        assert!(
            messages
                .iter()
                .all(|message| message.to_payload().unwrap().len() < 1_280)
        );
    }

    #[test]
    fn mobile_chunked_messages_match_dropbox_receiver_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = DropboxReceiver::new(dir.path());
        let data = vec![9u8; DROPBOX_CHUNK_DATA_BYTES + 5];
        let messages = build_dropbox_chunk_messages("transfer-1", "archive.bin", None, &data);

        let mut replies = Vec::new();
        for message in messages {
            replies = receiver.handle_message(message).unwrap();
        }

        assert_eq!(std::fs::read(dir.path().join("archive.bin")).unwrap(), data);
        assert_eq!(
            replies,
            vec![DropboxMessage::Ack {
                id: "transfer-1".to_string(),
                status: "stored".to_string(),
                sha256: Some(sha256_hex(&data)),
                size: Some(data.len() as u64),
                path: Some("archive.bin".to_string()),
            }]
        );
    }

    #[test]
    fn dropbox_ack_packet_removes_matching_status() {
        let transfer_id = "0102030405060708";
        let src_addr = NodeAddr::from_bytes([1u8; 16]);
        let mut missing = BTreeSet::from(["chunk:291".to_string(), "chunk:292".to_string()]);
        let packet = ServicePacket {
            src_addr,
            src_port: DROPBOX_SERVICE_PORT,
            dst_port: MOBILE_RESPONSE_PORT,
            payload: DropboxMessage::Ack {
                id: transfer_id.to_string(),
                status: "chunk:291".to_string(),
                sha256: None,
                size: None,
                path: None,
            }
            .to_payload()
            .unwrap(),
        };

        apply_dropbox_ack_packet(
            &packet,
            src_addr,
            MOBILE_RESPONSE_PORT,
            transfer_id,
            &mut missing,
        )
        .unwrap();

        assert_eq!(missing, BTreeSet::from(["chunk:292".to_string()]));
    }

    #[test]
    fn dropbox_ack_packet_ignores_other_transfer_ids() {
        let transfer_id = "0102030405060708";
        let src_addr = NodeAddr::from_bytes([1u8; 16]);
        let mut missing = BTreeSet::from(["chunk:1".to_string()]);
        let packet = ServicePacket {
            src_addr,
            src_port: DROPBOX_SERVICE_PORT,
            dst_port: MOBILE_RESPONSE_PORT,
            payload: DropboxMessage::Ack {
                id: "0807060504030201".to_string(),
                status: "chunk:1".to_string(),
                sha256: None,
                size: None,
                path: None,
            }
            .to_payload()
            .unwrap(),
        };

        apply_dropbox_ack_packet(
            &packet,
            src_addr,
            MOBILE_RESPONSE_PORT,
            transfer_id,
            &mut missing,
        )
        .unwrap();

        assert_eq!(missing, BTreeSet::from(["chunk:1".to_string()]));
    }

    #[test]
    fn dropbox_ack_packet_surfaces_remote_error() {
        let transfer_id = "0102030405060708";
        let src_addr = NodeAddr::from_bytes([1u8; 16]);
        let mut missing = BTreeSet::from(["stored".to_string()]);
        let packet = ServicePacket {
            src_addr,
            src_port: DROPBOX_SERVICE_PORT,
            dst_port: MOBILE_RESPONSE_PORT,
            payload: DropboxMessage::Error {
                id: Some(transfer_id.to_string()),
                reason: "missing chunk 291".to_string(),
            }
            .to_payload()
            .unwrap(),
        };

        let err = apply_dropbox_ack_packet(
            &packet,
            src_addr,
            MOBILE_RESPONSE_PORT,
            transfer_id,
            &mut missing,
        )
        .unwrap_err();

        assert!(matches!(
            err,
            FipsMobileError::DropboxRemoteError { reason, .. }
                if reason == "missing chunk 291"
        ));
    }

    #[test]
    fn dropbox_payload_size_limit_is_10mb() {
        assert!(validate_dropbox_file_size(DROPBOX_MAX_FILE_BYTES).is_ok());

        let err = validate_dropbox_file_size(DROPBOX_MAX_FILE_BYTES + 1).unwrap_err();

        assert!(matches!(
            err,
            FipsMobileError::DropboxFileTooLarge {
                size,
                max: DROPBOX_MAX_FILE_BYTES,
            } if size == DROPBOX_MAX_FILE_BYTES + 1
        ));
    }

    #[test]
    fn blob_missing_reports_are_filtered_to_sent_prefix() {
        let missing = vec![3, 7, 32, 33, 34, 99];

        assert_eq!(missing_chunks_at_or_below(&missing, 31), vec![3, 7]);
        assert_eq!(
            missing_chunks_at_or_below(&missing, 34),
            vec![3, 7, 32, 33, 34]
        );
    }

    #[test]
    fn blob_transfer_tuning_backs_off_on_loss_and_grows_after_clean_windows() {
        let mut tuning = DropboxBlobTransferTuning::default();

        assert_eq!(tuning.window_size(), DROPBOX_BLOB_WINDOW_SIZE);
        assert_eq!(tuning.repair_batch_size(), DROPBOX_BLOB_REPAIR_BATCH_SIZE);
        assert_eq!(
            tuning.chunk_spacing(),
            Duration::from_millis(DROPBOX_BLOB_DEFAULT_CHUNK_SPACING_MS)
        );

        tuning.record_window_result(32, 5);
        assert_eq!(tuning.window_size(), 16);
        assert_eq!(tuning.repair_batch_size(), 4);
        assert_eq!(tuning.chunk_spacing(), Duration::from_millis(8));

        for _ in 0..DROPBOX_BLOB_CLEAN_WINDOWS_BEFORE_GROW {
            tuning.record_window_result(16, 0);
        }
        assert_eq!(tuning.window_size(), 24);
        assert_eq!(tuning.repair_batch_size(), 6);
        assert_eq!(tuning.chunk_spacing(), Duration::from_millis(7));

        tuning.record_timeout();
        assert_eq!(tuning.window_size(), DROPBOX_BLOB_MIN_WINDOW_SIZE);
        assert_eq!(
            tuning.repair_batch_size(),
            DROPBOX_BLOB_MIN_REPAIR_BATCH_SIZE
        );
        assert_eq!(
            tuning.chunk_spacing(),
            Duration::from_millis(DROPBOX_BLOB_MAX_CHUNK_SPACING_MS)
        );
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
        let mut client = FipsMobileClient::start(FipsMobileConfig::new(Config::default()))
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
        let mut client = FipsMobileClient::start(FipsMobileConfig::new(Config::default()))
            .await
            .unwrap();
        let npub = peer_npub();

        let err = client
            .send_dropbox_blob_to_npub(&npub, "note.txt", None, b"hello")
            .await
            .unwrap_err();

        assert!(
            matches!(err, FipsMobileError::RouteUnavailable { ref reason, .. } if reason.contains("no active FIPS service session"))
        );
        assert!(err.to_string().contains("Connect first"));
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
