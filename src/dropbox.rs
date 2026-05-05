//! Minimal Dropbox-style app protocol over FIPS service ports.
//!
//! This is intentionally small and app-owned. It gives mobile/Pushstr PoCs a
//! concrete service protocol on top of the generic in-process FSP service-port
//! API without making the core FIPS transport Blossom-specific.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{NodeAddr, ServicePacket};

/// Reserved FSP service port for the first Dropbox-style PoC.
pub const DROPBOX_SERVICE_PORT: u16 = 4242;

/// Result type for the Dropbox service protocol.
pub type DropboxResult<T> = Result<T, DropboxError>;

/// Errors from Dropbox protocol parsing, validation, or storage.
#[derive(Debug, thiserror::Error)]
pub enum DropboxError {
    #[error("invalid filename: {0}")]
    InvalidFilename(String),
    #[error("invalid base64 payload: {0}")]
    InvalidBase64(String),
    #[error("declared size {declared} does not match payload size {actual}")]
    SizeMismatch { declared: u64, actual: u64 },
    #[error("declared sha256 {declared} does not match payload sha256 {actual}")]
    HashMismatch { declared: String, actual: String },
    #[error("missing upload state for transfer {0}")]
    MissingUpload(String),
    #[error("missing chunk {chunk_index} for transfer {id}")]
    MissingChunk { id: String, chunk_index: u32 },
    #[error("chunk index {chunk_index} out of range for transfer {id}")]
    ChunkOutOfRange { id: String, chunk_index: u32 },
    #[error("conflicting metadata for transfer {0}")]
    ConflictingMetadata(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Wire messages for `fips-dropbox-v0`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DropboxMessage {
    Hello {
        id: String,
        client: Option<String>,
    },
    Put {
        id: String,
        name: String,
        mime: Option<String>,
        sha256: Option<String>,
        size: Option<u64>,
        data_b64: String,
    },
    PutChunk {
        id: String,
        name: String,
        mime: Option<String>,
        sha256: Option<String>,
        size: Option<u64>,
        chunk_index: u32,
        chunk_count: u32,
        data_b64: String,
    },
    PutDone {
        id: String,
    },
    Ack {
        id: String,
        status: String,
        sha256: Option<String>,
        size: Option<u64>,
        path: Option<String>,
    },
    Error {
        id: Option<String>,
        reason: String,
    },
}

impl DropboxMessage {
    /// Serialize this message as UTF-8 JSON bytes for FSP service payloads.
    pub fn to_payload(&self) -> DropboxResult<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    /// Parse this message from UTF-8 JSON bytes carried over an FSP service port.
    pub fn from_payload(payload: &[u8]) -> DropboxResult<Self> {
        Ok(serde_json::from_slice(payload)?)
    }
}

/// Outbound response payload to send over a FIPS service port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropboxOutbound {
    pub dest_addr: NodeAddr,
    pub src_port: u16,
    pub dst_port: u16,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug)]
struct PendingUpload {
    name: String,
    mime: Option<String>,
    sha256: Option<String>,
    size: Option<u64>,
    chunks: Vec<Option<Vec<u8>>>,
}

/// Receiver-side state for the Pi4ssd Dropbox PoC agent.
#[derive(Debug)]
pub struct DropboxReceiver {
    root: PathBuf,
    pending: HashMap<String, PendingUpload>,
}

impl DropboxReceiver {
    /// Create a receiver that writes files under `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            pending: HashMap::new(),
        }
    }

    /// Handle one decrypted FIPS service packet and build reply payloads.
    pub fn handle_service_packet(
        &mut self,
        packet: &ServicePacket,
    ) -> DropboxResult<Vec<DropboxOutbound>> {
        let message = DropboxMessage::from_payload(&packet.payload)?;
        let replies = self.handle_message(message)?;
        replies
            .into_iter()
            .map(|reply| {
                Ok(DropboxOutbound {
                    dest_addr: packet.src_addr,
                    src_port: DROPBOX_SERVICE_PORT,
                    dst_port: packet.src_port,
                    payload: reply.to_payload()?,
                })
            })
            .collect()
    }

    /// Handle one decoded Dropbox message.
    pub fn handle_message(
        &mut self,
        message: DropboxMessage,
    ) -> DropboxResult<Vec<DropboxMessage>> {
        match message {
            DropboxMessage::Hello { id, .. } => Ok(vec![DropboxMessage::Ack {
                id,
                status: "hello".to_string(),
                sha256: None,
                size: None,
                path: None,
            }]),
            DropboxMessage::Put {
                id,
                name,
                mime: _,
                sha256,
                size,
                data_b64,
            } => {
                let data = decode_b64(&data_b64)?;
                validate_size(size, data.len())?;
                let actual_hash = sha256_hex(&data);
                validate_hash(sha256.as_deref(), &actual_hash)?;
                let path = self.write_file(&name, &data)?;
                Ok(vec![DropboxMessage::Ack {
                    id,
                    status: "stored".to_string(),
                    sha256: Some(actual_hash),
                    size: Some(data.len() as u64),
                    path: Some(path),
                }])
            }
            DropboxMessage::PutChunk {
                id,
                name,
                mime,
                sha256,
                size,
                chunk_index,
                chunk_count,
                data_b64,
            } => {
                let data = decode_b64(&data_b64)?;
                self.put_chunk(IncomingChunk {
                    id: id.clone(),
                    name,
                    mime,
                    sha256,
                    size,
                    chunk_index,
                    chunk_count,
                    data,
                })?;
                Ok(vec![DropboxMessage::Ack {
                    id,
                    status: format!("chunk:{chunk_index}"),
                    sha256: None,
                    size: None,
                    path: None,
                }])
            }
            DropboxMessage::PutDone { id } => {
                let (path, hash, size) = self.finish_upload(&id)?;
                Ok(vec![DropboxMessage::Ack {
                    id,
                    status: "stored".to_string(),
                    sha256: Some(hash),
                    size: Some(size),
                    path: Some(path),
                }])
            }
            DropboxMessage::Ack { .. } | DropboxMessage::Error { .. } => Ok(Vec::new()),
        }
    }

    fn put_chunk(&mut self, chunk: IncomingChunk) -> DropboxResult<()> {
        validate_filename(&chunk.name)?;
        let chunk_count = chunk.chunk_count as usize;
        if chunk.chunk_index >= chunk.chunk_count {
            return Err(DropboxError::ChunkOutOfRange {
                id: chunk.id,
                chunk_index: chunk.chunk_index,
            });
        }

        let entry = self
            .pending
            .entry(chunk.id.clone())
            .or_insert_with(|| PendingUpload {
                name: chunk.name.clone(),
                mime: chunk.mime.clone(),
                sha256: chunk.sha256.clone(),
                size: chunk.size,
                chunks: vec![None; chunk_count],
            });

        if entry.name != chunk.name
            || entry.mime != chunk.mime
            || entry.sha256 != chunk.sha256
            || entry.size != chunk.size
            || entry.chunks.len() != chunk_count
        {
            return Err(DropboxError::ConflictingMetadata(chunk.id));
        }

        entry.chunks[chunk.chunk_index as usize] = Some(chunk.data);
        Ok(())
    }

    fn finish_upload(&mut self, id: &str) -> DropboxResult<(String, String, u64)> {
        let upload = self
            .pending
            .remove(id)
            .ok_or_else(|| DropboxError::MissingUpload(id.to_string()))?;
        let mut data = Vec::new();
        for (index, chunk) in upload.chunks.into_iter().enumerate() {
            let Some(chunk) = chunk else {
                return Err(DropboxError::MissingChunk {
                    id: id.to_string(),
                    chunk_index: index as u32,
                });
            };
            data.extend_from_slice(&chunk);
        }

        validate_size(upload.size, data.len())?;
        let actual_hash = sha256_hex(&data);
        validate_hash(upload.sha256.as_deref(), &actual_hash)?;
        let path = self.write_file(&upload.name, &data)?;
        Ok((path, actual_hash, data.len() as u64))
    }

    fn write_file(&self, name: &str, data: &[u8]) -> DropboxResult<String> {
        validate_filename(name)?;
        std::fs::create_dir_all(&self.root)?;
        let path = self.root.join(name);
        std::fs::write(&path, data)?;
        Ok(name.to_string())
    }
}

#[derive(Debug)]
struct IncomingChunk {
    id: String,
    name: String,
    mime: Option<String>,
    sha256: Option<String>,
    size: Option<u64>,
    chunk_index: u32,
    chunk_count: u32,
    data: Vec<u8>,
}

fn decode_b64(data_b64: &str) -> DropboxResult<Vec<u8>> {
    BASE64
        .decode(data_b64)
        .map_err(|e| DropboxError::InvalidBase64(e.to_string()))
}

/// Encode bytes as base64 for `DropboxMessage` payloads.
pub fn encode_b64(data: &[u8]) -> String {
    BASE64.encode(data)
}

/// Compute lowercase hex SHA-256 for file payload verification.
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    hex::encode(digest)
}

fn validate_size(expected: Option<u64>, actual: usize) -> DropboxResult<()> {
    if let Some(declared) = expected
        && declared != actual as u64
    {
        return Err(DropboxError::SizeMismatch {
            declared,
            actual: actual as u64,
        });
    }
    Ok(())
}

fn validate_hash(expected: Option<&str>, actual: &str) -> DropboxResult<()> {
    if let Some(declared) = expected
        && !declared.eq_ignore_ascii_case(actual)
    {
        return Err(DropboxError::HashMismatch {
            declared: declared.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(())
}

fn validate_filename(name: &str) -> DropboxResult<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || Path::new(name).file_name().and_then(|s| s.to_str()) != Some(name)
    {
        return Err(DropboxError::InvalidFilename(name.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(src_port: u16, payload: DropboxMessage) -> ServicePacket {
        ServicePacket {
            src_addr: NodeAddr::from_bytes([9u8; 16]),
            src_port,
            dst_port: DROPBOX_SERVICE_PORT,
            payload: payload.to_payload().unwrap(),
        }
    }

    #[test]
    fn message_round_trips_json_payload() {
        let message = DropboxMessage::Hello {
            id: "hello-1".to_string(),
            client: Some("android".to_string()),
        };

        let payload = message.to_payload().unwrap();
        assert_eq!(DropboxMessage::from_payload(&payload).unwrap(), message);
    }

    #[test]
    fn base64_and_hash_helpers_are_stable() {
        let data = b"hello fips";

        assert_eq!(encode_b64(data), "aGVsbG8gZmlwcw==");
        assert_eq!(
            sha256_hex(data),
            "8daa55fd0c9b4912b5e382117cb2aad0a8b0630700d604d11eb71d8104577c4d"
        );
    }

    #[test]
    fn receiver_acks_hello_to_source_port() {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = DropboxReceiver::new(dir.path());
        let replies = receiver
            .handle_service_packet(&packet(
                5000,
                DropboxMessage::Hello {
                    id: "h1".to_string(),
                    client: None,
                },
            ))
            .unwrap();

        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].dest_addr, NodeAddr::from_bytes([9u8; 16]));
        assert_eq!(replies[0].src_port, DROPBOX_SERVICE_PORT);
        assert_eq!(replies[0].dst_port, 5000);
        assert_eq!(
            DropboxMessage::from_payload(&replies[0].payload).unwrap(),
            DropboxMessage::Ack {
                id: "h1".to_string(),
                status: "hello".to_string(),
                sha256: None,
                size: None,
                path: None,
            }
        );
    }

    #[test]
    fn receiver_stores_single_put_and_verifies_hash() {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = DropboxReceiver::new(dir.path());
        let data = b"small file";
        let replies = receiver
            .handle_message(DropboxMessage::Put {
                id: "p1".to_string(),
                name: "note.txt".to_string(),
                mime: Some("text/plain".to_string()),
                sha256: Some(sha256_hex(data)),
                size: Some(data.len() as u64),
                data_b64: encode_b64(data),
            })
            .unwrap();

        assert_eq!(std::fs::read(dir.path().join("note.txt")).unwrap(), data);
        assert_eq!(
            replies,
            vec![DropboxMessage::Ack {
                id: "p1".to_string(),
                status: "stored".to_string(),
                sha256: Some(sha256_hex(data)),
                size: Some(data.len() as u64),
                path: Some("note.txt".to_string()),
            }]
        );
    }

    #[test]
    fn receiver_rejects_unsafe_filename() {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = DropboxReceiver::new(dir.path());
        let err = receiver
            .handle_message(DropboxMessage::Put {
                id: "p1".to_string(),
                name: "../escape.txt".to_string(),
                mime: None,
                sha256: None,
                size: None,
                data_b64: encode_b64(b"bad"),
            })
            .unwrap_err();

        assert!(matches!(err, DropboxError::InvalidFilename(_)));
    }

    #[test]
    fn receiver_rejects_size_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = DropboxReceiver::new(dir.path());
        let err = receiver
            .handle_message(DropboxMessage::Put {
                id: "p1".to_string(),
                name: "note.txt".to_string(),
                mime: None,
                sha256: None,
                size: Some(999),
                data_b64: encode_b64(b"short"),
            })
            .unwrap_err();

        assert!(matches!(err, DropboxError::SizeMismatch { .. }));
    }

    #[test]
    fn receiver_rejects_hash_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = DropboxReceiver::new(dir.path());
        let err = receiver
            .handle_message(DropboxMessage::Put {
                id: "p1".to_string(),
                name: "note.txt".to_string(),
                mime: None,
                sha256: Some("00".repeat(32)),
                size: None,
                data_b64: encode_b64(b"content"),
            })
            .unwrap_err();

        assert!(matches!(err, DropboxError::HashMismatch { .. }));
    }

    #[test]
    fn receiver_stores_chunked_put_on_done() {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = DropboxReceiver::new(dir.path());
        let data = b"chunk-one/chunk-two";
        let expected_hash = sha256_hex(data);

        receiver
            .handle_message(DropboxMessage::PutChunk {
                id: "c1".to_string(),
                name: "song.bin".to_string(),
                mime: None,
                sha256: Some(expected_hash.clone()),
                size: Some(data.len() as u64),
                chunk_index: 0,
                chunk_count: 2,
                data_b64: encode_b64(b"chunk-one/"),
            })
            .unwrap();
        receiver
            .handle_message(DropboxMessage::PutChunk {
                id: "c1".to_string(),
                name: "song.bin".to_string(),
                mime: None,
                sha256: Some(expected_hash.clone()),
                size: Some(data.len() as u64),
                chunk_index: 1,
                chunk_count: 2,
                data_b64: encode_b64(b"chunk-two"),
            })
            .unwrap();

        let replies = receiver
            .handle_message(DropboxMessage::PutDone {
                id: "c1".to_string(),
            })
            .unwrap();

        assert_eq!(std::fs::read(dir.path().join("song.bin")).unwrap(), data);
        assert_eq!(
            replies,
            vec![DropboxMessage::Ack {
                id: "c1".to_string(),
                status: "stored".to_string(),
                sha256: Some(expected_hash),
                size: Some(data.len() as u64),
                path: Some("song.bin".to_string()),
            }]
        );
    }

    #[test]
    fn receiver_rejects_missing_chunk_on_done() {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = DropboxReceiver::new(dir.path());

        receiver
            .handle_message(DropboxMessage::PutChunk {
                id: "c1".to_string(),
                name: "song.bin".to_string(),
                mime: None,
                sha256: None,
                size: None,
                chunk_index: 1,
                chunk_count: 2,
                data_b64: encode_b64(b"chunk-two"),
            })
            .unwrap();

        let err = receiver
            .handle_message(DropboxMessage::PutDone {
                id: "c1".to_string(),
            })
            .unwrap_err();

        assert!(matches!(
            err,
            DropboxError::MissingChunk { chunk_index: 0, .. }
        ));
    }

    #[test]
    fn receiver_rejects_conflicting_chunk_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = DropboxReceiver::new(dir.path());

        receiver
            .handle_message(DropboxMessage::PutChunk {
                id: "c1".to_string(),
                name: "one.bin".to_string(),
                mime: None,
                sha256: None,
                size: None,
                chunk_index: 0,
                chunk_count: 2,
                data_b64: encode_b64(b"one"),
            })
            .unwrap();

        let err = receiver
            .handle_message(DropboxMessage::PutChunk {
                id: "c1".to_string(),
                name: "two.bin".to_string(),
                mime: None,
                sha256: None,
                size: None,
                chunk_index: 1,
                chunk_count: 2,
                data_b64: encode_b64(b"two"),
            })
            .unwrap_err();

        assert!(matches!(err, DropboxError::ConflictingMetadata(_)));
    }

    #[test]
    fn receiver_rejects_chunk_index_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = DropboxReceiver::new(dir.path());

        let err = receiver
            .handle_message(DropboxMessage::PutChunk {
                id: "c1".to_string(),
                name: "one.bin".to_string(),
                mime: None,
                sha256: None,
                size: None,
                chunk_index: 2,
                chunk_count: 2,
                data_b64: encode_b64(b"one"),
            })
            .unwrap_err();

        assert!(matches!(err, DropboxError::ChunkOutOfRange { .. }));
    }

    #[test]
    fn receiver_ignores_ack_and_error_messages() {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = DropboxReceiver::new(dir.path());

        assert!(
            receiver
                .handle_message(DropboxMessage::Ack {
                    id: "a1".to_string(),
                    status: "stored".to_string(),
                    sha256: None,
                    size: None,
                    path: None,
                })
                .unwrap()
                .is_empty()
        );
        assert!(
            receiver
                .handle_message(DropboxMessage::Error {
                    id: Some("e1".to_string()),
                    reason: "remote error".to_string(),
                })
                .unwrap()
                .is_empty()
        );
    }
}
