//! Mobile-oriented FIPS client crate.
//!
//! This crate is the stable Rust package boundary for Android wrappers today
//! and iOS wrappers later. The implementation currently delegates to the
//! embedded mobile facade in the root `fips` crate, while exposing product-name
//! aliases for FIPS Drop so new bindings do not need to learn the older
//! dropbox terminology.

pub use fips::mobile::{
    DROPBOX_BLOB_REPAIR_BATCH_SIZE, DROPBOX_BLOB_WINDOW_SIZE, DROPBOX_CHUNK_DATA_BYTES,
    DROPBOX_CHUNK_WINDOW_SIZE, DROPBOX_INLINE_PAYLOAD_BYTES, DROPBOX_MAX_FILE_BYTES,
    DROPBOX_MAX_SEND_ATTEMPTS, FipsMobileClient, FipsMobileConfig, FipsMobileError,
    MOBILE_RESPONSE_PORT, build_dropbox_put_message,
};

pub use fips::{
    Config, EmbeddedNodeStatus, Identity, IdentityError, NodeAddr, PeerIdentity, ServicePacket,
    decode_npub, encode_npub,
};

pub use fips::dropbox::{
    DROPBOX_BLOB_CHUNK_DATA_BYTES as FIPS_DROP_BLOB_CHUNK_DATA_BYTES,
    DROPBOX_BLOB_MISSING_REPORT_LIMIT as FIPS_DROP_BLOB_MISSING_REPORT_LIMIT,
    DROPBOX_SERVICE_PORT as FIPS_DROP_SERVICE_PORT, DropboxError as FipsDropError,
    DropboxMessage as FipsDropMessage, DropboxResult as FipsDropResult,
    encode_b64 as encode_fips_drop_b64, sha256_hex as fips_drop_sha256_hex,
};

/// Maximum serialized FIPS Drop payload to send as a single service packet.
pub const FIPS_DROP_INLINE_PAYLOAD_BYTES: usize = DROPBOX_INLINE_PAYLOAD_BYTES;

/// Raw file bytes per FIPS Drop binary blob chunk used by the mobile sender.
pub const FIPS_DROP_CHUNK_DATA_BYTES: usize = DROPBOX_CHUNK_DATA_BYTES;

/// Mobile PoC maximum FIPS Drop file size.
pub const FIPS_DROP_MAX_FILE_BYTES: usize = DROPBOX_MAX_FILE_BYTES;

/// Number of legacy chunk messages sent before waiting for ACKs.
pub const FIPS_DROP_CHUNK_WINDOW_SIZE: usize = DROPBOX_CHUNK_WINDOW_SIZE;

/// Number of binary blob chunks sent before asking for a sparse repair report.
pub const FIPS_DROP_BLOB_WINDOW_SIZE: usize = DROPBOX_BLOB_WINDOW_SIZE;

/// Number of missing binary blob chunks to repair before asking for a fresh report.
pub const FIPS_DROP_BLOB_REPAIR_BATCH_SIZE: usize = DROPBOX_BLOB_REPAIR_BATCH_SIZE;

/// Number of times to retry missing FIPS Drop chunks.
pub const FIPS_DROP_MAX_SEND_ATTEMPTS: usize = DROPBOX_MAX_SEND_ATTEMPTS;

/// Product-name alias for building a single-payload FIPS Drop message.
pub fn build_fips_drop_put_message(
    name: &str,
    mime: Option<String>,
    data: &[u8],
) -> FipsDropResult<FipsDropMessage> {
    build_dropbox_put_message(name, mime, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fips_drop_aliases_match_embedded_client_defaults() {
        assert_eq!(MOBILE_RESPONSE_PORT, 49_152);
        assert_eq!(FIPS_DROP_SERVICE_PORT, 4_242);
        assert_eq!(FIPS_DROP_CHUNK_DATA_BYTES, FIPS_DROP_BLOB_CHUNK_DATA_BYTES);
        assert_eq!(FIPS_DROP_BLOB_WINDOW_SIZE, DROPBOX_BLOB_WINDOW_SIZE);
    }

    #[test]
    fn builds_product_named_put_message() {
        let data = b"mobile crate smoke test";
        let message = build_fips_drop_put_message("note.txt", Some("text/plain".to_string()), data)
            .expect("put message builds");
        let expected_sha256 = fips_drop_sha256_hex(data);

        match message {
            FipsDropMessage::Put {
                name,
                mime,
                size,
                sha256,
                ..
            } => {
                assert_eq!(name, "note.txt");
                assert_eq!(mime.as_deref(), Some("text/plain"));
                assert_eq!(size, Some(data.len() as u64));
                assert_eq!(sha256.as_deref(), Some(expected_sha256.as_str()));
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }
}
