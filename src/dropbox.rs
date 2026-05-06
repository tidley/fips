//! FIPS file-transfer service over FIPS service ports.
//!
//! FIPS supplies the encrypted service-session transport. The default upload
//! path uses a compact binary blob protocol with content-addressed metadata,
//! sparse repair, and one completion/missing report instead of per-block ACKs.
//! The older CoAP Block1 codec remains as a compatibility parser.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use coap_lite::block_handler::BlockValue;
use coap_lite::option_value::OptionValueU32;
use coap_lite::{
    CoapOption, ContentFormat, MessageClass, MessageType, Packet, RequestType, ResponseType,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, info};

use crate::{NodeAddr, ServicePacket};

/// Reserved FSP service port for the first FIPS drop receiver PoC.
pub const DROPBOX_SERVICE_PORT: u16 = 4242;

/// Raw file bytes per CoAP Block1 payload.
pub const DROPBOX_BLOCK_DATA_BYTES: usize = 512;

/// Default raw file bytes per binary blob packet. Keeps the full FIPS datagram
/// comfortably below the 1280-byte path used by the Android/Pi NAT tests.
pub const DROPBOX_BLOB_CHUNK_DATA_BYTES: usize = 768;

/// Maximum number of missing chunk indexes to include in one repair report.
pub const DROPBOX_BLOB_MISSING_REPORT_LIMIT: usize = 256;

const DROPBOX_COAP_URI_ROOT: &str = "dropbox";
const DROPBOX_COAP_BLOCK_SIZE: usize = DROPBOX_BLOCK_DATA_BYTES;
const DROPBOX_BLOB_MAGIC: &[u8; 4] = b"FDB1";
const DROPBOX_BLOB_START: u8 = 1;
const DROPBOX_BLOB_CHUNK: u8 = 2;
const DROPBOX_BLOB_ACK: u8 = 3;
const DROPBOX_BLOB_DONE: u8 = 4;
const DROPBOX_BLOB_STORED: u8 = 5;
const DROPBOX_BLOB_ERROR: u8 = 6;

/// Result type for the FIPS Drop service protocol.
pub type DropboxResult<T> = Result<T, DropboxError>;

/// Errors from FIPS Drop protocol parsing, validation, or storage.
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
    #[error("file-transfer protocol error: {0}")]
    Protocol(String),
    #[error("coap payload error: {0}")]
    Coap(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Wire messages for the FIPS Drop v0 service.
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
    BlobStart {
        id: String,
        name: String,
        mime: Option<String>,
        sha256: String,
        size: u64,
        chunk_size: u16,
        chunk_count: u32,
    },
    BlobChunk {
        id: String,
        chunk_index: u32,
        data: Vec<u8>,
    },
    BlobDone {
        id: String,
    },
    BlobAck {
        id: String,
        received_chunks: u32,
        highest_contiguous: Option<u32>,
        missing_chunks: Vec<u32>,
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
    /// Serialize this message for FSP service payloads.
    pub fn to_payload(&self) -> DropboxResult<Vec<u8>> {
        if self.is_blob_wire_message() {
            return blob_message_to_payload(self);
        }
        coap_message_to_payload(self)
    }

    /// Parse this message from bytes carried over an FSP service port.
    ///
    /// Binary blob payloads are preferred. CoAP and JSON parsers are kept as
    /// transitional fallbacks for older test tools and agents.
    pub fn from_payload(payload: &[u8]) -> DropboxResult<Self> {
        if payload.starts_with(DROPBOX_BLOB_MAGIC) {
            return blob_message_from_payload(payload);
        }

        match coap_message_from_payload(payload) {
            Ok(message) => Ok(message),
            Err(coap_error) => serde_json::from_slice(payload).map_err(|json_error| {
                DropboxError::Coap(format!("{coap_error}; json fallback failed: {json_error}"))
            }),
        }
    }

    fn is_blob_wire_message(&self) -> bool {
        match self {
            DropboxMessage::BlobStart { .. }
            | DropboxMessage::BlobChunk { .. }
            | DropboxMessage::BlobDone { .. }
            | DropboxMessage::BlobAck { .. }
            | DropboxMessage::Error { .. } => true,
            DropboxMessage::Ack { status, .. } => status == "stored",
            _ => false,
        }
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

#[derive(Debug, Serialize, Deserialize)]
struct CoapStoredPayload {
    id: String,
    status: String,
    sha256: Option<String>,
    size: Option<u64>,
    path: Option<String>,
}

fn blob_message_to_payload(message: &DropboxMessage) -> DropboxResult<Vec<u8>> {
    let mut payload = Vec::new();
    payload.extend_from_slice(DROPBOX_BLOB_MAGIC);

    match message {
        DropboxMessage::BlobStart {
            id,
            name,
            mime,
            sha256,
            size,
            chunk_size,
            chunk_count,
        } => {
            validate_filename(name)?;
            validate_short_text("id", id, u8::MAX as usize)?;
            validate_short_text("name", name, u16::MAX as usize)?;
            validate_short_text("sha256", sha256, u8::MAX as usize)?;
            if let Some(mime) = mime {
                validate_short_text("mime", mime, u16::MAX as usize)?;
            }
            payload.push(DROPBOX_BLOB_START);
            put_u8_len_prefixed(&mut payload, id.as_bytes())?;
            put_u16_len_prefixed(&mut payload, name.as_bytes())?;
            put_u16_len_prefixed(&mut payload, mime.as_deref().unwrap_or("").as_bytes())?;
            put_u8_len_prefixed(&mut payload, sha256.as_bytes())?;
            payload.extend_from_slice(&size.to_le_bytes());
            payload.extend_from_slice(&chunk_size.to_le_bytes());
            payload.extend_from_slice(&chunk_count.to_le_bytes());
        }
        DropboxMessage::BlobChunk {
            id,
            chunk_index,
            data,
        } => {
            validate_short_text("id", id, u8::MAX as usize)?;
            if data.len() > u16::MAX as usize {
                return Err(DropboxError::Protocol(format!(
                    "blob chunk payload too large: {}",
                    data.len()
                )));
            }
            payload.push(DROPBOX_BLOB_CHUNK);
            put_u8_len_prefixed(&mut payload, id.as_bytes())?;
            payload.extend_from_slice(&chunk_index.to_le_bytes());
            payload.extend_from_slice(&(data.len() as u16).to_le_bytes());
            payload.extend_from_slice(data);
        }
        DropboxMessage::BlobDone { id } => {
            validate_short_text("id", id, u8::MAX as usize)?;
            payload.push(DROPBOX_BLOB_DONE);
            put_u8_len_prefixed(&mut payload, id.as_bytes())?;
        }
        DropboxMessage::BlobAck {
            id,
            received_chunks,
            highest_contiguous,
            missing_chunks,
        } => {
            validate_short_text("id", id, u8::MAX as usize)?;
            if missing_chunks.len() > u16::MAX as usize {
                return Err(DropboxError::Protocol(format!(
                    "too many missing chunks in one ack: {}",
                    missing_chunks.len()
                )));
            }
            payload.push(DROPBOX_BLOB_ACK);
            put_u8_len_prefixed(&mut payload, id.as_bytes())?;
            payload.extend_from_slice(&received_chunks.to_le_bytes());
            payload.extend_from_slice(&highest_contiguous.unwrap_or(u32::MAX).to_le_bytes());
            payload.extend_from_slice(&(missing_chunks.len() as u16).to_le_bytes());
            for chunk_index in missing_chunks {
                payload.extend_from_slice(&chunk_index.to_le_bytes());
            }
        }
        DropboxMessage::Ack {
            id,
            status,
            sha256,
            size,
            path,
        } if status == "stored" => {
            validate_short_text("id", id, u8::MAX as usize)?;
            validate_short_text("sha256", sha256.as_deref().unwrap_or(""), u8::MAX as usize)?;
            validate_short_text("path", path.as_deref().unwrap_or(""), u16::MAX as usize)?;
            payload.push(DROPBOX_BLOB_STORED);
            put_u8_len_prefixed(&mut payload, id.as_bytes())?;
            put_u8_len_prefixed(&mut payload, sha256.as_deref().unwrap_or("").as_bytes())?;
            payload.extend_from_slice(&size.unwrap_or_default().to_le_bytes());
            put_u16_len_prefixed(&mut payload, path.as_deref().unwrap_or("").as_bytes())?;
        }
        DropboxMessage::Error { id, reason } => {
            validate_short_text("id", id.as_deref().unwrap_or(""), u8::MAX as usize)?;
            validate_short_text("reason", reason, u16::MAX as usize)?;
            payload.push(DROPBOX_BLOB_ERROR);
            put_u8_len_prefixed(&mut payload, id.as_deref().unwrap_or("").as_bytes())?;
            put_u16_len_prefixed(&mut payload, reason.as_bytes())?;
        }
        _ => {
            return Err(DropboxError::Protocol(
                "message is not supported by binary blob wire codec".to_string(),
            ));
        }
    }

    Ok(payload)
}

fn blob_message_from_payload(payload: &[u8]) -> DropboxResult<DropboxMessage> {
    let mut cursor = BlobCursor::new(payload);
    cursor.take_magic()?;
    let message_type = cursor.take_u8()?;
    match message_type {
        DROPBOX_BLOB_START => {
            let id = cursor.take_utf8_u8()?;
            let name = cursor.take_utf8_u16()?;
            let mime = empty_to_none(cursor.take_utf8_u16()?);
            let sha256 = cursor.take_utf8_u8()?;
            let size = cursor.take_u64()?;
            let chunk_size = cursor.take_u16()?;
            let chunk_count = cursor.take_u32()?;
            cursor.finish()?;
            Ok(DropboxMessage::BlobStart {
                id,
                name,
                mime,
                sha256,
                size,
                chunk_size,
                chunk_count,
            })
        }
        DROPBOX_BLOB_CHUNK => {
            let id = cursor.take_utf8_u8()?;
            let chunk_index = cursor.take_u32()?;
            let data_len = cursor.take_u16()? as usize;
            let data = cursor.take_bytes(data_len)?.to_vec();
            cursor.finish()?;
            Ok(DropboxMessage::BlobChunk {
                id,
                chunk_index,
                data,
            })
        }
        DROPBOX_BLOB_ACK => {
            let id = cursor.take_utf8_u8()?;
            let received_chunks = cursor.take_u32()?;
            let raw_highest = cursor.take_u32()?;
            let missing_count = cursor.take_u16()? as usize;
            let mut missing_chunks = Vec::with_capacity(missing_count);
            for _ in 0..missing_count {
                missing_chunks.push(cursor.take_u32()?);
            }
            cursor.finish()?;
            Ok(DropboxMessage::BlobAck {
                id,
                received_chunks,
                highest_contiguous: (raw_highest != u32::MAX).then_some(raw_highest),
                missing_chunks,
            })
        }
        DROPBOX_BLOB_DONE => {
            let id = cursor.take_utf8_u8()?;
            cursor.finish()?;
            Ok(DropboxMessage::BlobDone { id })
        }
        DROPBOX_BLOB_STORED => {
            let id = cursor.take_utf8_u8()?;
            let sha256 = empty_to_none(cursor.take_utf8_u8()?);
            let size = cursor.take_u64()?;
            let path = empty_to_none(cursor.take_utf8_u16()?);
            cursor.finish()?;
            Ok(DropboxMessage::Ack {
                id,
                status: "stored".to_string(),
                sha256,
                size: Some(size),
                path,
            })
        }
        DROPBOX_BLOB_ERROR => {
            let id = empty_to_none(cursor.take_utf8_u8()?);
            let reason = cursor.take_utf8_u16()?;
            cursor.finish()?;
            Ok(DropboxMessage::Error { id, reason })
        }
        other => Err(DropboxError::Protocol(format!(
            "unknown binary blob message type {other}"
        ))),
    }
}

fn put_u8_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) -> DropboxResult<()> {
    if bytes.len() > u8::MAX as usize {
        return Err(DropboxError::Protocol(format!(
            "u8 length-prefixed field too long: {}",
            bytes.len()
        )));
    }
    out.push(bytes.len() as u8);
    out.extend_from_slice(bytes);
    Ok(())
}

fn put_u16_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) -> DropboxResult<()> {
    if bytes.len() > u16::MAX as usize {
        return Err(DropboxError::Protocol(format!(
            "u16 length-prefixed field too long: {}",
            bytes.len()
        )));
    }
    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn validate_short_text(name: &str, value: &str, max_len: usize) -> DropboxResult<()> {
    if value.len() > max_len {
        return Err(DropboxError::Protocol(format!(
            "{name} is too long: {} > {max_len}",
            value.len()
        )));
    }
    Ok(())
}

fn empty_to_none(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

struct BlobCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> BlobCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take_magic(&mut self) -> DropboxResult<()> {
        let magic = self.take_bytes(DROPBOX_BLOB_MAGIC.len())?;
        if magic != DROPBOX_BLOB_MAGIC {
            return Err(DropboxError::Protocol(
                "invalid binary blob magic".to_string(),
            ));
        }
        Ok(())
    }

    fn take_u8(&mut self) -> DropboxResult<u8> {
        Ok(self.take_bytes(1)?[0])
    }

    fn take_u16(&mut self) -> DropboxResult<u16> {
        let bytes = self.take_bytes(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn take_u32(&mut self) -> DropboxResult<u32> {
        let bytes = self.take_bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn take_u64(&mut self) -> DropboxResult<u64> {
        let bytes = self.take_bytes(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn take_utf8_u8(&mut self) -> DropboxResult<String> {
        let len = self.take_u8()? as usize;
        self.take_utf8(len)
    }

    fn take_utf8_u16(&mut self) -> DropboxResult<String> {
        let len = self.take_u16()? as usize;
        self.take_utf8(len)
    }

    fn take_utf8(&mut self, len: usize) -> DropboxResult<String> {
        String::from_utf8(self.take_bytes(len)?.to_vec())
            .map_err(|e| DropboxError::Protocol(format!("invalid utf8: {e}")))
    }

    fn take_bytes(&mut self, len: usize) -> DropboxResult<&'a [u8]> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| DropboxError::Protocol("blob cursor overflow".to_string()))?;
        if end > self.bytes.len() {
            return Err(DropboxError::Protocol(format!(
                "truncated binary blob payload: need {len} bytes at offset {}",
                self.offset
            )));
        }
        let bytes = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(bytes)
    }

    fn finish(&self) -> DropboxResult<()> {
        if self.offset != self.bytes.len() {
            return Err(DropboxError::Protocol(format!(
                "trailing bytes in binary blob payload: {}",
                self.bytes.len() - self.offset
            )));
        }
        Ok(())
    }
}

fn coap_message_to_payload(message: &DropboxMessage) -> DropboxResult<Vec<u8>> {
    let mut packet = Packet::new();
    packet.header.set_type(MessageType::NonConfirmable);

    match message {
        DropboxMessage::Hello { id, client } => {
            packet.header.code = MessageClass::Request(RequestType::Post);
            set_coap_identity(&mut packet, id);
            add_uri_path(&mut packet, DROPBOX_COAP_URI_ROOT);
            add_uri_path(&mut packet, "hello");
            if let Some(client) = client {
                add_uri_query(&mut packet, "client", client);
            }
        }
        DropboxMessage::Put {
            id,
            name,
            mime,
            sha256,
            size,
            data_b64,
        } => {
            packet.header.code = MessageClass::Request(RequestType::Put);
            set_coap_identity(&mut packet, id);
            add_upload_options(&mut packet, name, mime, sha256, *size)?;
            packet.set_content_format(ContentFormat::ApplicationOctetStream);
            packet.payload = decode_b64(data_b64)?;
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
            packet.header.code = MessageClass::Request(RequestType::Put);
            set_coap_identity(&mut packet, id);
            add_upload_options(&mut packet, name, mime, sha256, *size)?;
            packet.set_content_format(ContentFormat::ApplicationOctetStream);
            let more = chunk_index.saturating_add(1) < *chunk_count;
            let block = BlockValue::new(*chunk_index as usize, more, DROPBOX_COAP_BLOCK_SIZE)
                .map_err(|e| DropboxError::Coap(e.to_string()))?;
            packet.add_option_as(CoapOption::Block1, block);
            packet.payload = decode_b64(data_b64)?;
        }
        DropboxMessage::PutDone { id } => {
            packet.header.code = MessageClass::Request(RequestType::Put);
            set_coap_identity(&mut packet, id);
            add_uri_path(&mut packet, DROPBOX_COAP_URI_ROOT);
            add_uri_path(&mut packet, "done");
        }
        DropboxMessage::Ack {
            id,
            status,
            sha256,
            size,
            path,
        } => {
            set_coap_identity(&mut packet, id);
            if let Some(chunk_index) = status.strip_prefix("chunk:") {
                packet.header.code = MessageClass::Response(ResponseType::Continue);
                let chunk_index = chunk_index
                    .parse::<usize>()
                    .map_err(|e| DropboxError::Coap(format!("invalid chunk ack: {e}")))?;
                let block = BlockValue::new(chunk_index, true, DROPBOX_COAP_BLOCK_SIZE)
                    .map_err(|e| DropboxError::Coap(e.to_string()))?;
                packet.add_option_as(CoapOption::Block1, block);
            } else {
                packet.header.code = MessageClass::Response(ResponseType::Changed);
                packet.set_content_format(ContentFormat::ApplicationJSON);
                packet.payload = serde_json::to_vec(&CoapStoredPayload {
                    id: id.clone(),
                    status: status.clone(),
                    sha256: sha256.clone(),
                    size: *size,
                    path: path.clone(),
                })?;
            }
        }
        DropboxMessage::Error { id, reason } => {
            packet.header.code = MessageClass::Response(ResponseType::BadRequest);
            packet.header.set_type(MessageType::NonConfirmable);
            if let Some(id) = id {
                set_coap_identity(&mut packet, id);
            }
            packet.set_content_format(ContentFormat::TextPlain);
            packet.payload = reason.as_bytes().to_vec();
        }
        DropboxMessage::BlobStart { .. }
        | DropboxMessage::BlobChunk { .. }
        | DropboxMessage::BlobDone { .. }
        | DropboxMessage::BlobAck { .. } => {
            return Err(DropboxError::Protocol(
                "binary blob message passed to CoAP encoder".to_string(),
            ));
        }
    }

    packet
        .to_bytes_with_limit(1_280)
        .map_err(|e| DropboxError::Coap(e.to_string()))
}

fn coap_message_from_payload(payload: &[u8]) -> DropboxResult<DropboxMessage> {
    let packet = Packet::from_bytes(payload).map_err(|e| DropboxError::Coap(e.to_string()))?;
    match packet.header.code {
        MessageClass::Request(RequestType::Post) => decode_coap_post(&packet),
        MessageClass::Request(RequestType::Put) => decode_coap_put(&packet),
        MessageClass::Response(response) => decode_coap_response(&packet, response),
        other => Err(DropboxError::Coap(format!(
            "unsupported CoAP message code {other}"
        ))),
    }
}

fn decode_coap_post(packet: &Packet) -> DropboxResult<DropboxMessage> {
    let paths = coap_uri_paths(packet)?;
    if paths.as_slice() != [DROPBOX_COAP_URI_ROOT, "hello"] {
        return Err(DropboxError::Coap(format!(
            "unsupported CoAP POST path {}",
            paths.join("/")
        )));
    }
    Ok(DropboxMessage::Hello {
        id: coap_id(packet),
        client: coap_query_value(packet, "client")?,
    })
}

fn decode_coap_put(packet: &Packet) -> DropboxResult<DropboxMessage> {
    let paths = coap_uri_paths(packet)?;
    let [root, name] = paths.as_slice() else {
        return Err(DropboxError::Coap(format!(
            "unsupported CoAP PUT path {}",
            paths.join("/")
        )));
    };
    if root != DROPBOX_COAP_URI_ROOT {
        return Err(DropboxError::Coap(format!(
            "unsupported CoAP PUT root {root}"
        )));
    }

    let id = coap_id(packet);
    if name == "done" && packet.payload.is_empty() {
        return Ok(DropboxMessage::PutDone { id });
    }

    let mime = coap_query_value(packet, "mime")?;
    let sha256 = coap_query_value(packet, "sha256")?;
    let size = coap_size1(packet)?;
    let data_b64 = encode_b64(&packet.payload);

    if let Some(block) = coap_block1(packet)? {
        let size = size
            .ok_or_else(|| DropboxError::Coap("CoAP Block1 upload is missing Size1".to_string()))?;
        let chunk_count = size.div_ceil(block.size() as u64) as u32;
        return Ok(DropboxMessage::PutChunk {
            id,
            name: name.to_string(),
            mime,
            sha256,
            size: Some(size),
            chunk_index: block.num as u32,
            chunk_count,
            data_b64,
        });
    }

    Ok(DropboxMessage::Put {
        id,
        name: name.to_string(),
        mime,
        sha256,
        size,
        data_b64,
    })
}

fn decode_coap_response(packet: &Packet, response: ResponseType) -> DropboxResult<DropboxMessage> {
    let id = coap_id(packet);
    if response.is_error() {
        let reason = String::from_utf8_lossy(&packet.payload).trim().to_string();
        return Ok(DropboxMessage::Error {
            id: Some(id),
            reason: if reason.is_empty() {
                format!("CoAP error {}", MessageClass::Response(response))
            } else {
                reason
            },
        });
    }

    if response == ResponseType::Continue {
        let block = coap_block1(packet)?.ok_or_else(|| {
            DropboxError::Coap("CoAP 2.31 response is missing Block1".to_string())
        })?;
        return Ok(DropboxMessage::Ack {
            id,
            status: format!("chunk:{}", block.num),
            sha256: None,
            size: None,
            path: None,
        });
    }

    if matches!(response, ResponseType::Changed | ResponseType::Created) {
        let stored = if packet.payload.is_empty() {
            CoapStoredPayload {
                id,
                status: "stored".to_string(),
                sha256: None,
                size: None,
                path: None,
            }
        } else {
            serde_json::from_slice::<CoapStoredPayload>(&packet.payload)?
        };
        return Ok(DropboxMessage::Ack {
            id: stored.id,
            status: stored.status,
            sha256: stored.sha256,
            size: stored.size,
            path: stored.path,
        });
    }

    Err(DropboxError::Coap(format!(
        "unsupported CoAP response {}",
        MessageClass::Response(response)
    )))
}

fn add_upload_options(
    packet: &mut Packet,
    name: &str,
    mime: &Option<String>,
    sha256: &Option<String>,
    size: Option<u64>,
) -> DropboxResult<()> {
    validate_filename(name)?;
    add_uri_path(packet, DROPBOX_COAP_URI_ROOT);
    add_uri_path(packet, name);
    if let Some(mime) = mime {
        add_uri_query(packet, "mime", mime);
    }
    if let Some(sha256) = sha256 {
        add_uri_query(packet, "sha256", sha256);
    }
    if let Some(size) = size {
        let size = u32::try_from(size)
            .map_err(|_| DropboxError::Coap(format!("Size1 too large: {size}")))?;
        packet.add_option_as(CoapOption::Size1, OptionValueU32(size));
    }
    Ok(())
}

fn set_coap_identity(packet: &mut Packet, id: &str) {
    let token = coap_token_from_id(id);
    packet.header.message_id = u16::from_be_bytes([token[6], token[7]]);
    packet.set_token(token);
}

fn coap_token_from_id(id: &str) -> Vec<u8> {
    if id.len() == 16
        && let Ok(bytes) = hex::decode(id)
        && bytes.len() == 8
    {
        return bytes;
    }

    let digest = Sha256::digest(id.as_bytes());
    digest[..8].to_vec()
}

fn coap_id(packet: &Packet) -> String {
    hex::encode(packet.get_token())
}

fn add_uri_path(packet: &mut Packet, path: &str) {
    packet.add_option(CoapOption::UriPath, path.as_bytes().to_vec());
}

fn add_uri_query(packet: &mut Packet, key: &str, value: &str) {
    packet.add_option(CoapOption::UriQuery, format!("{key}={value}").into_bytes());
}

fn coap_uri_paths(packet: &Packet) -> DropboxResult<Vec<String>> {
    let Some(paths) = packet.get_option(CoapOption::UriPath) else {
        return Ok(Vec::new());
    };
    paths
        .iter()
        .map(|path| {
            String::from_utf8(path.clone())
                .map_err(|e| DropboxError::Coap(format!("invalid Uri-Path: {e}")))
        })
        .collect()
}

fn coap_query_value(packet: &Packet, key: &str) -> DropboxResult<Option<String>> {
    let Some(queries) = packet.get_option(CoapOption::UriQuery) else {
        return Ok(None);
    };

    for query in queries {
        let query = String::from_utf8(query.clone())
            .map_err(|e| DropboxError::Coap(format!("invalid Uri-Query: {e}")))?;
        if let Some((query_key, query_value)) = query.split_once('=')
            && query_key == key
        {
            return Ok(Some(query_value.to_string()));
        }
    }

    Ok(None)
}

fn coap_size1(packet: &Packet) -> DropboxResult<Option<u64>> {
    packet
        .get_first_option_as::<OptionValueU32>(CoapOption::Size1)
        .transpose()
        .map(|size| size.map(|size| size.0 as u64))
        .map_err(|e| DropboxError::Coap(format!("invalid Size1: {e}")))
}

fn coap_block1(packet: &Packet) -> DropboxResult<Option<BlockValue>> {
    packet
        .get_first_option_as::<BlockValue>(CoapOption::Block1)
        .transpose()
        .map_err(|e| DropboxError::Coap(format!("invalid Block1: {e}")))
}

#[derive(Clone, Debug)]
struct PendingUpload {
    name: String,
    mime: Option<String>,
    sha256: Option<String>,
    size: Option<u64>,
    chunks: Vec<Option<Vec<u8>>>,
}

/// Receiver-side state for the FIPS Drop receiver agent.
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

    /// Handle one decoded FIPS Drop message.
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
                info!(
                    id = %id,
                    name = %name,
                    size = data.len(),
                    path = %path,
                    stored_path = %self.stored_path_display(&path),
                    "FIPS Drop inline file stored"
                );
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
                if chunk_index.saturating_add(1) == chunk_count {
                    let (path, hash, size) = self.finish_upload(&id)?;
                    info!(
                        id = %id,
                        chunk_count,
                        size,
                        path = %path,
                        stored_path = %self.stored_path_display(&path),
                        "FIPS Drop chunked file stored"
                    );
                    return Ok(vec![DropboxMessage::Ack {
                        id,
                        status: "stored".to_string(),
                        sha256: Some(hash),
                        size: Some(size),
                        path: Some(path),
                    }]);
                }
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
            DropboxMessage::BlobStart {
                id,
                name,
                mime,
                sha256,
                size,
                chunk_size,
                chunk_count,
            } => {
                self.start_blob_upload(
                    id.clone(),
                    name,
                    mime,
                    sha256,
                    size,
                    chunk_size,
                    chunk_count,
                )?;
                Ok(vec![self.blob_ack(&id)])
            }
            DropboxMessage::BlobChunk {
                id,
                chunk_index,
                data,
            } => {
                self.put_blob_chunk(&id, chunk_index, data)?;
                Ok(Vec::new())
            }
            DropboxMessage::BlobDone { id } => {
                if self.upload_complete(&id)? {
                    let (path, hash, size) = self.finish_upload(&id)?;
                    info!(
                        id = %id,
                        size,
                        path = %path,
                        stored_path = %self.stored_path_display(&path),
                        "FIPS Drop binary blob stored"
                    );
                    Ok(vec![DropboxMessage::Ack {
                        id,
                        status: "stored".to_string(),
                        sha256: Some(hash),
                        size: Some(size),
                        path: Some(path),
                    }])
                } else {
                    Ok(vec![self.blob_ack(&id)])
                }
            }
            DropboxMessage::BlobAck { .. }
            | DropboxMessage::Ack { .. }
            | DropboxMessage::Error { .. } => Ok(Vec::new()),
        }
    }

    fn start_blob_upload(
        &mut self,
        id: String,
        name: String,
        mime: Option<String>,
        sha256: String,
        size: u64,
        chunk_size: u16,
        chunk_count: u32,
    ) -> DropboxResult<()> {
        validate_filename(&name)?;
        if chunk_size == 0 {
            return Err(DropboxError::Protocol(
                "blob start declared zero chunk size".to_string(),
            ));
        }
        if chunk_count == 0 {
            return Err(DropboxError::Protocol(
                "blob start declared zero chunks".to_string(),
            ));
        }
        let min_chunks = size.div_ceil(chunk_size as u64);
        if min_chunks != chunk_count as u64 {
            return Err(DropboxError::Protocol(format!(
                "blob chunk count mismatch: size={size} chunk_size={chunk_size} chunk_count={chunk_count}"
            )));
        }

        let chunk_count_usize = usize::try_from(chunk_count).map_err(|_| {
            DropboxError::Protocol(format!("blob chunk count too large: {chunk_count}"))
        })?;
        match self.pending.get(&id) {
            Some(existing)
                if existing.name == name
                    && existing.mime == mime
                    && existing.sha256.as_deref() == Some(sha256.as_str())
                    && existing.size == Some(size)
                    && existing.chunks.len() == chunk_count_usize => {}
            Some(_) => return Err(DropboxError::ConflictingMetadata(id)),
            None => {
                self.pending.insert(
                    id,
                    PendingUpload {
                        name,
                        mime,
                        sha256: Some(sha256),
                        size: Some(size),
                        chunks: vec![None; chunk_count_usize],
                    },
                );
            }
        }
        Ok(())
    }

    fn put_blob_chunk(&mut self, id: &str, chunk_index: u32, data: Vec<u8>) -> DropboxResult<()> {
        let upload = self
            .pending
            .get_mut(id)
            .ok_or_else(|| DropboxError::MissingUpload(id.to_string()))?;
        if chunk_index as usize >= upload.chunks.len() {
            return Err(DropboxError::ChunkOutOfRange {
                id: id.to_string(),
                chunk_index,
            });
        }

        let chunk_len = data.len();
        upload.chunks[chunk_index as usize] = Some(data);
        let received_chunks = upload.chunks.iter().filter(|chunk| chunk.is_some()).count();
        if should_log_chunk_progress(chunk_index, upload.chunks.len() as u32, received_chunks) {
            debug!(
                id,
                name = %upload.name,
                chunk_index,
                chunk_count = upload.chunks.len(),
                received_chunks,
                chunk_len,
                "FIPS Drop binary blob chunk received"
            );
        }
        Ok(())
    }

    fn upload_complete(&self, id: &str) -> DropboxResult<bool> {
        let upload = self
            .pending
            .get(id)
            .ok_or_else(|| DropboxError::MissingUpload(id.to_string()))?;
        Ok(upload.chunks.iter().all(|chunk| chunk.is_some()))
    }

    fn blob_ack(&self, id: &str) -> DropboxMessage {
        let Some(upload) = self.pending.get(id) else {
            return DropboxMessage::BlobAck {
                id: id.to_string(),
                received_chunks: 0,
                highest_contiguous: None,
                missing_chunks: Vec::new(),
            };
        };
        let received_chunks = upload.chunks.iter().filter(|chunk| chunk.is_some()).count();
        let highest_contiguous = match upload.chunks.iter().position(|chunk| chunk.is_none()) {
            Some(0) => None,
            Some(missing_index) => Some((missing_index - 1) as u32),
            None => upload.chunks.len().checked_sub(1).map(|index| index as u32),
        };
        let missing_chunks = upload
            .chunks
            .iter()
            .enumerate()
            .filter_map(|(index, chunk)| chunk.is_none().then_some(index as u32))
            .take(DROPBOX_BLOB_MISSING_REPORT_LIMIT)
            .collect();

        DropboxMessage::BlobAck {
            id: id.to_string(),
            received_chunks: received_chunks as u32,
            highest_contiguous,
            missing_chunks,
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

        let chunk_len = chunk.data.len();
        entry.chunks[chunk.chunk_index as usize] = Some(chunk.data);
        let received_chunks = entry.chunks.iter().filter(|chunk| chunk.is_some()).count();
        if should_log_chunk_progress(chunk.chunk_index, chunk.chunk_count, received_chunks) {
            debug!(
                id = %chunk.id,
                name = %entry.name,
                chunk_index = chunk.chunk_index,
                chunk_count = chunk.chunk_count,
                received_chunks,
                chunk_len,
                "FIPS Drop chunk received"
            );
        }
        Ok(())
    }

    fn finish_upload(&mut self, id: &str) -> DropboxResult<(String, String, u64)> {
        let upload = self
            .pending
            .get(id)
            .ok_or_else(|| DropboxError::MissingUpload(id.to_string()))?;
        let mut data = Vec::new();
        for (index, chunk) in upload.chunks.iter().enumerate() {
            let Some(chunk) = chunk else {
                return Err(DropboxError::MissingChunk {
                    id: id.to_string(),
                    chunk_index: index as u32,
                });
            };
            data.extend_from_slice(chunk);
        }

        validate_size(upload.size, data.len())?;
        let actual_hash = sha256_hex(&data);
        validate_hash(upload.sha256.as_deref(), &actual_hash)?;
        let name = upload.name.clone();
        self.pending.remove(id);
        let path = self.write_file(&name, &data)?;
        Ok((path, actual_hash, data.len() as u64))
    }

    fn write_file(&self, name: &str, data: &[u8]) -> DropboxResult<String> {
        validate_filename(name)?;
        std::fs::create_dir_all(&self.root)?;
        let path = self.root.join(name);
        std::fs::write(&path, data)?;
        Ok(name.to_string())
    }

    fn stored_path_display(&self, relative_path: &str) -> String {
        self.root.join(relative_path).display().to_string()
    }
}

fn should_log_chunk_progress(chunk_index: u32, chunk_count: u32, received_chunks: usize) -> bool {
    chunk_index < 4
        || chunk_index.saturating_add(1) == chunk_count
        || received_chunks <= 4
        || received_chunks % 64 == 0
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

/// Encode bytes as base64 for legacy `DropboxMessage` payloads.
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
    fn message_round_trips_coap_payload() {
        let message = DropboxMessage::Hello {
            id: "0102030405060708".to_string(),
            client: Some("android".to_string()),
        };

        let payload = message.to_payload().unwrap();
        assert_ne!(payload.first(), Some(&b'{'));
        assert_eq!(DropboxMessage::from_payload(&payload).unwrap(), message);
    }

    #[test]
    fn blob_messages_round_trip_binary_payload() {
        let messages = [
            DropboxMessage::BlobStart {
                id: "0102030405060708".to_string(),
                name: "video.mp4".to_string(),
                mime: Some("video/mp4".to_string()),
                sha256: "11".repeat(32),
                size: 2048,
                chunk_size: DROPBOX_BLOB_CHUNK_DATA_BYTES as u16,
                chunk_count: 2,
            },
            DropboxMessage::BlobChunk {
                id: "0102030405060708".to_string(),
                chunk_index: 1,
                data: vec![1, 2, 3, 4],
            },
            DropboxMessage::BlobAck {
                id: "0102030405060708".to_string(),
                received_chunks: 1,
                highest_contiguous: Some(0),
                missing_chunks: vec![1],
            },
            DropboxMessage::BlobDone {
                id: "0102030405060708".to_string(),
            },
        ];

        for message in messages {
            let payload = message.to_payload().unwrap();
            assert_eq!(&payload[..4], DROPBOX_BLOB_MAGIC);
            assert_eq!(DropboxMessage::from_payload(&payload).unwrap(), message);
        }
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
                    id: "0102030405060708".to_string(),
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
                id: "0102030405060708".to_string(),
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
    fn receiver_stores_chunked_put_on_final_block() {
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
        let replies = receiver
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
    fn receiver_reports_missing_blob_chunks_then_stores_after_repair() {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = DropboxReceiver::new(dir.path());
        let data = b"first-second-third";
        let transfer = "0102030405060708";
        let hash = sha256_hex(data);

        let ready = receiver
            .handle_message(DropboxMessage::BlobStart {
                id: transfer.to_string(),
                name: "video.bin".to_string(),
                mime: None,
                sha256: hash.clone(),
                size: data.len() as u64,
                chunk_size: 6,
                chunk_count: 3,
            })
            .unwrap();
        assert_eq!(
            ready,
            vec![DropboxMessage::BlobAck {
                id: transfer.to_string(),
                received_chunks: 0,
                highest_contiguous: None,
                missing_chunks: vec![0, 1, 2],
            }]
        );

        receiver
            .handle_message(DropboxMessage::BlobChunk {
                id: transfer.to_string(),
                chunk_index: 0,
                data: b"first-".to_vec(),
            })
            .unwrap();
        receiver
            .handle_message(DropboxMessage::BlobChunk {
                id: transfer.to_string(),
                chunk_index: 2,
                data: b"third".to_vec(),
            })
            .unwrap();

        let missing = receiver
            .handle_message(DropboxMessage::BlobDone {
                id: transfer.to_string(),
            })
            .unwrap();
        assert_eq!(
            missing,
            vec![DropboxMessage::BlobAck {
                id: transfer.to_string(),
                received_chunks: 2,
                highest_contiguous: Some(0),
                missing_chunks: vec![1],
            }]
        );

        receiver
            .handle_message(DropboxMessage::BlobChunk {
                id: transfer.to_string(),
                chunk_index: 1,
                data: b"second-".to_vec(),
            })
            .unwrap();
        let stored = receiver
            .handle_message(DropboxMessage::BlobDone {
                id: transfer.to_string(),
            })
            .unwrap();

        assert_eq!(std::fs::read(dir.path().join("video.bin")).unwrap(), data);
        assert_eq!(
            stored,
            vec![DropboxMessage::Ack {
                id: transfer.to_string(),
                status: "stored".to_string(),
                sha256: Some(hash),
                size: Some(data.len() as u64),
                path: Some("video.bin".to_string()),
            }]
        );
    }

    #[test]
    fn receiver_stores_binary_blob_chunks_delivered_out_of_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = DropboxReceiver::new(dir.path());
        let chunks = [
            (0, b"zero-".as_slice()),
            (1, b"one-".as_slice()),
            (2, b"two-".as_slice()),
            (3, b"three".as_slice()),
        ];
        let data = chunks
            .iter()
            .flat_map(|(_, chunk)| chunk.iter().copied())
            .collect::<Vec<_>>();
        let transfer = "1111111111111111";
        let hash = sha256_hex(&data);

        receiver
            .handle_message(DropboxMessage::BlobStart {
                id: transfer.to_string(),
                name: "out-of-order.bin".to_string(),
                mime: None,
                sha256: hash.clone(),
                size: data.len() as u64,
                chunk_size: 5,
                chunk_count: chunks.len() as u32,
            })
            .unwrap();

        for index in [3, 1, 0, 2] {
            receiver
                .handle_message(DropboxMessage::BlobChunk {
                    id: transfer.to_string(),
                    chunk_index: index,
                    data: chunks[index as usize].1.to_vec(),
                })
                .unwrap();
        }

        let stored = receiver
            .handle_message(DropboxMessage::BlobDone {
                id: transfer.to_string(),
            })
            .unwrap();

        assert_eq!(
            std::fs::read(dir.path().join("out-of-order.bin")).unwrap(),
            data
        );
        assert_eq!(
            stored,
            vec![DropboxMessage::Ack {
                id: transfer.to_string(),
                status: "stored".to_string(),
                sha256: Some(hash),
                size: Some(data.len() as u64),
                path: Some("out-of-order.bin".to_string()),
            }]
        );
    }

    #[test]
    fn receiver_reports_sparse_missing_prefix_for_binary_blob() {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = DropboxReceiver::new(dir.path());
        let transfer = "2222222222222222";
        let data = b"aaaabbbbccccdddd";

        receiver
            .handle_message(DropboxMessage::BlobStart {
                id: transfer.to_string(),
                name: "sparse.bin".to_string(),
                mime: None,
                sha256: sha256_hex(data),
                size: data.len() as u64,
                chunk_size: 4,
                chunk_count: 4,
            })
            .unwrap();

        for (chunk_index, chunk) in [(0, b"aaaa".as_slice()), (3, b"dddd".as_slice())] {
            receiver
                .handle_message(DropboxMessage::BlobChunk {
                    id: transfer.to_string(),
                    chunk_index,
                    data: chunk.to_vec(),
                })
                .unwrap();
        }

        let missing = receiver
            .handle_message(DropboxMessage::BlobDone {
                id: transfer.to_string(),
            })
            .unwrap();

        assert_eq!(
            missing,
            vec![DropboxMessage::BlobAck {
                id: transfer.to_string(),
                received_chunks: 2,
                highest_contiguous: Some(0),
                missing_chunks: vec![1, 2],
            }]
        );
    }

    #[test]
    fn receiver_rejects_missing_chunk_on_final_block() {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = DropboxReceiver::new(dir.path());

        let err = receiver
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
