//! QUIC transport for FIPS data streams.
//!
//! Android and future iOS bindings use this as the primary data transport over
//! an already-established direct UDP path, while binding the QUIC TLS
//! certificate to the normal FIPS/npub identity instead of trusting WebPKI.

use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::Arc,
    time::Duration,
};

use crate::{EstablishedTraversal, Identity, PeerIdentity};
use quinn::{
    ClientConfig, Endpoint, EndpointConfig, ReadToEndError, ServerConfig, VarInt, WriteError,
    crypto::rustls::QuicClientConfig, crypto::rustls::QuicServerConfig,
};
use rcgen::{CertificateParams, CustomExtension, KeyPair};
use rustls::{
    CertificateError, DigitallySignedStruct, DistinguishedName, Error as RustlsError,
    SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
};
use secp256k1::schnorr::Signature;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::debug;

const ALPN: &[u8] = b"fips-quic/0";
const CERT_BINDING_OID: &[u64] = &[1, 3, 6, 1, 4, 1, 55555, 70, 1];
const CERT_BINDING_OID_TEXT: &str = "1.3.6.1.4.1.55555.70.1";
const CERT_BINDING_CONTEXT: &[u8] = b"fips-quic-cert-binding-v1";

/// Options for the one-stream QUIC proof helpers.
#[derive(Clone, Debug)]
pub struct FipsQuicOptions {
    /// Bounds discovery, QUIC handshake, and stream operations.
    pub timeout: Duration,
    /// Maximum inbound stream payload accepted into memory.
    pub max_stream_bytes: usize,
}

impl Default for FipsQuicOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_stream_bytes: 16 * 1024 * 1024,
        }
    }
}

/// npub-to-QUIC certificate binding embedded in a private X.509 extension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FipsQuicCertificateBinding {
    pub version: u8,
    pub npub: String,
    /// SHA-256 of the TLS certificate SubjectPublicKey bit string.
    pub tls_public_key_sha256: String,
    /// Schnorr signature by `npub` over the binding context and public-key hash.
    pub signature: String,
}

/// Result returned by the client-side one-stream proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FipsQuicStreamResponse {
    pub peer_npub: String,
    pub remote_addr: SocketAddr,
    pub response: Vec<u8>,
}

/// Result returned by the server-side one-stream proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FipsQuicReceivedStream {
    pub peer_npub: String,
    pub remote_addr: SocketAddr,
    pub request: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum FipsQuicError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("no Quinn async runtime is available")]
    NoRuntime,
    #[error("certificate generation failed: {0}")]
    CertificateGeneration(#[from] rcgen::Error),
    #[error("rustls configuration failed: {0}")]
    Rustls(#[from] RustlsError),
    #[error("quic/rustls has no usable initial cipher suite: {0}")]
    NoInitialCipherSuite(#[from] quinn::crypto::rustls::NoInitialCipherSuite),
    #[error("quic connect failed: {0}")]
    Connect(#[from] quinn::ConnectError),
    #[error("quic connection failed: {0}")]
    Connection(#[from] quinn::ConnectionError),
    #[error("quic write failed: {0}")]
    Write(#[from] WriteError),
    #[error("quic read failed: {0}")]
    ReadToEnd(#[from] ReadToEndError),
    #[error("quic stream was already closed")]
    ClosedStream,
    #[error("invalid FIPS peer identity: {0}")]
    Identity(#[from] crate::IdentityError),
    #[error("invalid FIPS QUIC certificate binding: {0}")]
    CertificateBinding(String),
    #[error("{operation} timed out after {timeout:?}")]
    Timeout {
        operation: &'static str,
        timeout: Duration,
    },
    #[cfg(feature = "nostr-discovery")]
    #[error("nostr/stun discovery failed: {0}")]
    NostrDiscovery(String),
}

type Result<T> = std::result::Result<T, FipsQuicError>;

struct LocalQuicCertificate {
    cert_chain: Vec<CertificateDer<'static>>,
    private_key: PrivateKeyDer<'static>,
}

/// Generate a self-signed QUIC certificate whose public key is signed by the
/// local FIPS identity and embedded as a private extension.
pub fn generate_identity_bound_certificate(
    identity: &Identity,
) -> Result<FipsQuicCertificateBinding> {
    let tls_key = KeyPair::generate()?;
    let public_key_hash = sha256_array(tls_key.public_key_raw());
    Ok(build_certificate_binding(identity, public_key_hash))
}

/// Parse and verify a FIPS-bound QUIC certificate against an expected peer npub.
pub fn verify_identity_bound_certificate(
    certificate_der: &[u8],
    expected_peer_npub: &str,
) -> Result<FipsQuicCertificateBinding> {
    verify_certificate_binding(certificate_der, expected_peer_npub)
}

/// Connect to the remote traversal endpoint, open one bidirectional QUIC
/// stream, send `payload`, and return the peer response.
///
/// The supplied traversal socket must be the same UDP socket that Nostr/STUN
/// used for hole punching. QUIC certificate validation is pinned to
/// `traversal.peer_npub`.
pub async fn connect_one_stream(
    identity: &Identity,
    traversal: EstablishedTraversal,
    payload: &[u8],
    options: FipsQuicOptions,
) -> Result<FipsQuicStreamResponse> {
    timeout_result(
        "quic client stream",
        options.timeout,
        connect_one_stream_inner(identity, traversal, payload, options.max_stream_bytes),
    )
    .await
}

/// Accept one incoming QUIC connection on the traversal socket, read a single
/// bidirectional stream, write `response`, and return the received request.
///
/// The client certificate must be bound to `traversal.peer_npub`.
pub async fn accept_one_stream(
    identity: &Identity,
    traversal: EstablishedTraversal,
    response: &[u8],
    options: FipsQuicOptions,
) -> Result<FipsQuicReceivedStream> {
    timeout_result(
        "quic server stream",
        options.timeout,
        accept_one_stream_inner(identity, traversal, response, options.max_stream_bytes),
    )
    .await
}

/// Accept one incoming QUIC connection, derive the response from the request,
/// and return the received request after the response has been sent.
pub async fn accept_one_stream_with_response(
    identity: &Identity,
    traversal: EstablishedTraversal,
    options: FipsQuicOptions,
    responder: impl FnOnce(&[u8]) -> Vec<u8>,
) -> Result<FipsQuicReceivedStream> {
    timeout_result(
        "quic server stream",
        options.timeout,
        accept_one_stream_with_response_inner(
            identity,
            traversal,
            options.max_stream_bytes,
            responder,
        ),
    )
    .await
}

#[cfg(feature = "nostr-discovery")]
/// Run Nostr/STUN discovery to obtain an established direct UDP traversal, then
/// prove one client-initiated QUIC stream over that socket.
pub async fn connect_one_stream_via_nostr_stun(
    identity: &Identity,
    mut discovery_config: crate::config::NostrDiscoveryConfig,
    peer_npub: impl Into<String>,
    payload: &[u8],
    options: FipsQuicOptions,
) -> Result<FipsQuicStreamResponse> {
    use crate::config::PeerConfig;
    use crate::discovery::nostr::NostrDiscovery;

    discovery_config.enabled = true;
    let peer_npub = peer_npub.into();
    let discovery = NostrDiscovery::start(identity, discovery_config)
        .await
        .map_err(|err| FipsQuicError::NostrDiscovery(err.to_string()))?;

    let result = async {
        discovery
            .request_connect(PeerConfig {
                npub: peer_npub.clone(),
                via_nostr: true,
                ..PeerConfig::default()
            })
            .await;
        let traversal =
            wait_for_established_traversal(&discovery, Some(peer_npub.as_str()), options.timeout)
                .await?;
        connect_one_stream(identity, traversal, payload, options).await
    }
    .await;

    let _ = discovery.shutdown().await;
    result
}

#[cfg(feature = "nostr-discovery")]
/// Run Nostr/STUN discovery as an advertiser/responder, then prove one
/// server-side QUIC stream over the established traversal socket.
pub async fn accept_one_stream_via_nostr_stun(
    identity: &Identity,
    mut discovery_config: crate::config::NostrDiscoveryConfig,
    response: &[u8],
    options: FipsQuicOptions,
) -> Result<FipsQuicReceivedStream> {
    use crate::discovery::nostr::NostrDiscovery;

    discovery_config.enabled = true;
    discovery_config.advertise = true;
    let local_advert = build_quic_nat_overlay_advert(&discovery_config);
    let discovery = NostrDiscovery::start(identity, discovery_config)
        .await
        .map_err(|err| FipsQuicError::NostrDiscovery(err.to_string()))?;
    discovery
        .update_local_advert(Some(local_advert))
        .await
        .map_err(|err| FipsQuicError::NostrDiscovery(err.to_string()))?;

    let result = async {
        let traversal = wait_for_established_traversal(&discovery, None, options.timeout).await?;
        accept_one_stream(identity, traversal, response, options).await
    }
    .await;

    let _ = discovery.shutdown().await;
    result
}

#[cfg(feature = "nostr-discovery")]
/// Run Nostr/STUN discovery as an advertiser/responder, then handle one
/// server-side QUIC stream over the established traversal socket.
pub async fn accept_one_stream_via_nostr_stun_with_response(
    identity: &Identity,
    mut discovery_config: crate::config::NostrDiscoveryConfig,
    options: FipsQuicOptions,
    responder: impl FnOnce(&[u8]) -> Vec<u8>,
) -> Result<FipsQuicReceivedStream> {
    use crate::discovery::nostr::NostrDiscovery;

    discovery_config.enabled = true;
    discovery_config.advertise = true;
    let local_advert = build_quic_nat_overlay_advert(&discovery_config);
    let discovery = NostrDiscovery::start(identity, discovery_config)
        .await
        .map_err(|err| FipsQuicError::NostrDiscovery(err.to_string()))?;
    discovery
        .update_local_advert(Some(local_advert))
        .await
        .map_err(|err| FipsQuicError::NostrDiscovery(err.to_string()))?;

    let result = async {
        let traversal = wait_for_established_traversal(&discovery, None, options.timeout).await?;
        accept_one_stream_with_response(identity, traversal, options, responder).await
    }
    .await;

    let _ = discovery.shutdown().await;
    result
}

async fn connect_one_stream_inner(
    identity: &Identity,
    traversal: EstablishedTraversal,
    payload: &[u8],
    max_stream_bytes: usize,
) -> Result<FipsQuicStreamResponse> {
    let peer_npub = traversal.peer_npub.clone();
    let remote_addr = traversal.remote_addr;
    let local_addr = traversal.socket.local_addr().ok();
    debug!(
        peer = %peer_npub,
        remote = %remote_addr,
        local = %local_addr.map(|addr| addr.to_string()).unwrap_or_else(|| "-".to_string()),
        len = payload.len(),
        "FIPS QUIC client endpoint connecting"
    );
    let mut endpoint = endpoint_with_socket(None, traversal.socket)?;
    endpoint.set_default_client_config(build_client_config(identity, &peer_npub)?);

    let connection = endpoint.connect(remote_addr, "fips-quic")?.await?;
    debug!(
        peer = %peer_npub,
        remote = %remote_addr,
        "FIPS QUIC client connection established"
    );
    let (mut send, mut recv) = connection.open_bi().await?;
    send.write_all(payload).await?;
    send.finish().map_err(|_| FipsQuicError::ClosedStream)?;
    let response = recv.read_to_end(max_stream_bytes).await?;

    connection.close(VarInt::from_u32(0), b"done");
    endpoint.wait_idle().await;

    Ok(FipsQuicStreamResponse {
        peer_npub,
        remote_addr,
        response,
    })
}

async fn accept_one_stream_inner(
    identity: &Identity,
    traversal: EstablishedTraversal,
    response: &[u8],
    max_stream_bytes: usize,
) -> Result<FipsQuicReceivedStream> {
    let peer_npub = traversal.peer_npub.clone();
    let remote_addr = traversal.remote_addr;
    let local_addr = traversal.socket.local_addr().ok();
    let endpoint = endpoint_with_socket(
        Some(build_server_config(identity, &peer_npub)?),
        traversal.socket,
    )?;
    debug!(
        peer = %peer_npub,
        remote = %remote_addr,
        local = %local_addr.map(|addr| addr.to_string()).unwrap_or_else(|| "-".to_string()),
        "FIPS QUIC server endpoint awaiting connection"
    );

    let incoming = endpoint.accept().await.ok_or(FipsQuicError::Connection(
        quinn::ConnectionError::LocallyClosed,
    ))?;
    let connection = incoming.await?;
    debug!(
        peer = %peer_npub,
        remote = %remote_addr,
        "FIPS QUIC server connection accepted"
    );
    let (mut send, mut recv) = connection.accept_bi().await?;
    let request = recv.read_to_end(max_stream_bytes).await?;
    send.write_all(response).await?;
    send.finish().map_err(|_| FipsQuicError::ClosedStream)?;
    let _ = send.stopped().await;

    connection.close(VarInt::from_u32(0), b"done");
    endpoint.wait_idle().await;

    Ok(FipsQuicReceivedStream {
        peer_npub,
        remote_addr,
        request,
    })
}

async fn accept_one_stream_with_response_inner(
    identity: &Identity,
    traversal: EstablishedTraversal,
    max_stream_bytes: usize,
    responder: impl FnOnce(&[u8]) -> Vec<u8>,
) -> Result<FipsQuicReceivedStream> {
    let peer_npub = traversal.peer_npub.clone();
    let remote_addr = traversal.remote_addr;
    let local_addr = traversal.socket.local_addr().ok();
    let endpoint = endpoint_with_socket(
        Some(build_server_config(identity, &peer_npub)?),
        traversal.socket,
    )?;
    debug!(
        peer = %peer_npub,
        remote = %remote_addr,
        local = %local_addr.map(|addr| addr.to_string()).unwrap_or_else(|| "-".to_string()),
        "FIPS QUIC server endpoint awaiting request stream"
    );

    let incoming = endpoint.accept().await.ok_or(FipsQuicError::Connection(
        quinn::ConnectionError::LocallyClosed,
    ))?;
    let connection = incoming.await?;
    debug!(
        peer = %peer_npub,
        remote = %remote_addr,
        "FIPS QUIC server connection accepted"
    );
    let (mut send, mut recv) = connection.accept_bi().await?;
    let request = recv.read_to_end(max_stream_bytes).await?;
    let response = responder(&request);
    send.write_all(&response).await?;
    send.finish().map_err(|_| FipsQuicError::ClosedStream)?;
    let _ = send.stopped().await;

    connection.close(VarInt::from_u32(0), b"done");
    endpoint.wait_idle().await;

    Ok(FipsQuicReceivedStream {
        peer_npub,
        remote_addr,
        request,
    })
}

fn endpoint_with_socket(
    server_config: Option<ServerConfig>,
    socket: UdpSocket,
) -> Result<Endpoint> {
    let runtime = quinn::default_runtime().ok_or(FipsQuicError::NoRuntime)?;
    Ok(Endpoint::new(
        EndpointConfig::default(),
        server_config,
        socket,
        runtime,
    )?)
}

fn build_client_config(identity: &Identity, expected_peer_npub: &str) -> Result<ClientConfig> {
    let local = build_local_certificate(identity)?;
    let mut client_crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(FipsQuicPeerVerifier::new(expected_peer_npub)))
        .with_client_auth_cert(local.cert_chain, local.private_key)?;
    client_crypto.alpn_protocols = vec![ALPN.to_vec()];
    Ok(ClientConfig::new(Arc::new(QuicClientConfig::try_from(
        client_crypto,
    )?)))
}

fn build_server_config(identity: &Identity, expected_peer_npub: &str) -> Result<ServerConfig> {
    let local = build_local_certificate(identity)?;
    let mut server_crypto = rustls::ServerConfig::builder()
        .with_client_cert_verifier(Arc::new(FipsQuicPeerVerifier::new(expected_peer_npub)))
        .with_single_cert(local.cert_chain, local.private_key)?;
    server_crypto.alpn_protocols = vec![ALPN.to_vec()];

    let mut server_config =
        ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server_crypto)?));
    let transport = Arc::get_mut(&mut server_config.transport)
        .expect("new server config has a unique transport config");
    transport.max_concurrent_uni_streams(0_u8.into());
    Ok(server_config)
}

fn build_local_certificate(identity: &Identity) -> Result<LocalQuicCertificate> {
    let tls_key = KeyPair::generate()?;
    let public_key_hash = sha256_array(tls_key.public_key_raw());
    let binding = build_certificate_binding(identity, public_key_hash);
    let binding_json = serde_json::to_vec(&binding)
        .map_err(|err| FipsQuicError::CertificateBinding(err.to_string()))?;

    let mut params = CertificateParams::new(vec!["fips-quic".to_string()])?;
    params
        .custom_extensions
        .push(CustomExtension::from_oid_content(
            CERT_BINDING_OID,
            der_octet_string(&binding_json),
        ));

    let cert = params.self_signed(&tls_key)?;
    let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(tls_key.serialize_der()));
    Ok(LocalQuicCertificate {
        cert_chain: vec![cert.der().clone()],
        private_key,
    })
}

fn build_certificate_binding(
    identity: &Identity,
    public_key_hash: [u8; 32],
) -> FipsQuicCertificateBinding {
    let signature = identity.sign(&binding_signature_payload(&public_key_hash));
    FipsQuicCertificateBinding {
        version: 1,
        npub: identity.npub(),
        tls_public_key_sha256: hex::encode(public_key_hash),
        signature: hex::encode(signature.to_byte_array()),
    }
}

fn verify_certificate_binding(
    certificate_der: &[u8],
    expected_peer_npub: &str,
) -> Result<FipsQuicCertificateBinding> {
    use x509_parser::prelude::*;

    let (_, cert) = X509Certificate::from_der(certificate_der)
        .map_err(|err| FipsQuicError::CertificateBinding(format!("bad x509: {err}")))?;
    let binding_ext = cert
        .tbs_certificate
        .iter_extensions()
        .filter(|ext| ext.oid.to_id_string() == CERT_BINDING_OID_TEXT)
        .collect::<Vec<_>>();
    if binding_ext.len() != 1 {
        return Err(FipsQuicError::CertificateBinding(format!(
            "expected one FIPS QUIC binding extension, found {}",
            binding_ext.len()
        )));
    }

    let binding_bytes = parse_der_octet_string(binding_ext[0].value)?;
    let binding: FipsQuicCertificateBinding = serde_json::from_slice(binding_bytes)
        .map_err(|err| FipsQuicError::CertificateBinding(err.to_string()))?;
    if binding.version != 1 {
        return Err(FipsQuicError::CertificateBinding(format!(
            "unsupported binding version {}",
            binding.version
        )));
    }
    if binding.npub != expected_peer_npub {
        return Err(FipsQuicError::CertificateBinding(format!(
            "certificate npub {} did not match expected {}",
            binding.npub, expected_peer_npub
        )));
    }

    let actual_hash = sha256_array(cert.tbs_certificate.subject_pki.subject_public_key.data);
    let expected_hash = parse_hash_hex(&binding.tls_public_key_sha256)?;
    if actual_hash != expected_hash {
        return Err(FipsQuicError::CertificateBinding(
            "certificate public key hash did not match binding".to_string(),
        ));
    }

    let signature_bytes = hex::decode(&binding.signature)
        .map_err(|err| FipsQuicError::CertificateBinding(err.to_string()))?;
    let signature_array: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| FipsQuicError::CertificateBinding("bad schnorr signature length".into()))?;
    let signature = Signature::from_byte_array(signature_array);
    let peer = PeerIdentity::from_npub(&binding.npub)?;
    if !peer.verify(&binding_signature_payload(&actual_hash), &signature) {
        return Err(FipsQuicError::CertificateBinding(
            "certificate binding signature was invalid".to_string(),
        ));
    }

    Ok(binding)
}

#[derive(Debug)]
struct FipsQuicPeerVerifier {
    expected_peer_npub: String,
    provider: Arc<rustls::crypto::CryptoProvider>,
    root_hints: Vec<DistinguishedName>,
}

impl FipsQuicPeerVerifier {
    fn new(expected_peer_npub: impl Into<String>) -> Self {
        Self {
            expected_peer_npub: expected_peer_npub.into(),
            provider: Arc::new(rustls::crypto::ring::default_provider()),
            root_hints: Vec::new(),
        }
    }

    fn verify_binding(
        &self,
        end_entity: &CertificateDer<'_>,
    ) -> std::result::Result<(), RustlsError> {
        verify_certificate_binding(end_entity.as_ref(), &self.expected_peer_npub)
            .map(|_| ())
            .map_err(|_| {
                RustlsError::InvalidCertificate(CertificateError::ApplicationVerificationFailure)
            })
    }
}

impl ServerCertVerifier for FipsQuicPeerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, RustlsError> {
        self.verify_binding(end_entity)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl ClientCertVerifier for FipsQuicPeerVerifier {
    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &self.root_hints
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, RustlsError> {
        self.verify_binding(end_entity)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(feature = "nostr-discovery")]
async fn wait_for_established_traversal(
    discovery: &Arc<crate::discovery::nostr::NostrDiscovery>,
    expected_peer_npub: Option<&str>,
    timeout: Duration,
) -> Result<EstablishedTraversal> {
    use crate::discovery::nostr::BootstrapEvent;

    timeout_result("nostr/stun traversal", timeout, async {
        loop {
            for event in discovery.drain_events().await {
                match event {
                    BootstrapEvent::Established { traversal } => {
                        if expected_peer_npub
                            .map(|expected| expected == traversal.peer_npub)
                            .unwrap_or(true)
                        {
                            return Ok(traversal);
                        }
                    }
                    BootstrapEvent::Failed {
                        peer_config,
                        reason,
                    } => {
                        if expected_peer_npub
                            .map(|expected| expected == peer_config.npub)
                            .unwrap_or(true)
                        {
                            return Err(FipsQuicError::NostrDiscovery(reason));
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
}

#[cfg(feature = "nostr-discovery")]
pub(crate) fn build_quic_nat_overlay_advert(
    config: &crate::config::NostrDiscoveryConfig,
) -> crate::discovery::nostr::OverlayAdvert {
    crate::discovery::nostr::OverlayAdvert {
        identifier: crate::discovery::nostr::ADVERT_IDENTIFIER.to_string(),
        version: crate::discovery::nostr::ADVERT_VERSION,
        endpoints: vec![crate::discovery::nostr::OverlayEndpointAdvert {
            transport: crate::discovery::nostr::OverlayTransportKind::Udp,
            addr: "nat".to_string(),
        }],
        signal_relays: Some(config.dm_relays.clone()),
        stun_servers: (!config.stun_servers.is_empty()).then(|| config.stun_servers.clone()),
        stun_services: None,
    }
}

async fn timeout_result<T>(
    operation: &'static str,
    timeout: Duration,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| FipsQuicError::Timeout { operation, timeout })?
}

fn sha256_array(input: impl AsRef<[u8]>) -> [u8; 32] {
    let digest = Sha256::digest(input.as_ref());
    digest.into()
}

fn parse_hash_hex(input: &str) -> Result<[u8; 32]> {
    let bytes =
        hex::decode(input).map_err(|err| FipsQuicError::CertificateBinding(err.to_string()))?;
    bytes
        .try_into()
        .map_err(|_| FipsQuicError::CertificateBinding("bad SHA-256 length".into()))
}

fn binding_signature_payload(public_key_hash: &[u8; 32]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(CERT_BINDING_CONTEXT.len() + 1 + public_key_hash.len());
    payload.extend_from_slice(CERT_BINDING_CONTEXT);
    payload.push(0);
    payload.extend_from_slice(public_key_hash);
    payload
}

fn der_octet_string(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 5);
    out.push(0x04);
    encode_der_len(payload.len(), &mut out);
    out.extend_from_slice(payload);
    out
}

fn parse_der_octet_string(input: &[u8]) -> Result<&[u8]> {
    if input.first().copied() != Some(0x04) {
        return Err(FipsQuicError::CertificateBinding(
            "binding extension was not a DER OCTET STRING".to_string(),
        ));
    }
    let (len, header_len) = decode_der_len(&input[1..])?;
    let start = 1 + header_len;
    let end = start + len;
    if input.len() != end {
        return Err(FipsQuicError::CertificateBinding(
            "binding extension had trailing or truncated DER bytes".to_string(),
        ));
    }
    Ok(&input[start..end])
}

fn encode_der_len(len: usize, out: &mut Vec<u8>) {
    if len < 0x80 {
        out.push(len as u8);
        return;
    }
    let bytes = len.to_be_bytes();
    let first_nonzero = bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len() - 1);
    let encoded = &bytes[first_nonzero..];
    out.push(0x80 | encoded.len() as u8);
    out.extend_from_slice(encoded);
}

fn decode_der_len(input: &[u8]) -> Result<(usize, usize)> {
    let first = input
        .first()
        .copied()
        .ok_or_else(|| FipsQuicError::CertificateBinding("missing DER length".to_string()))?;
    if first < 0x80 {
        return Ok((first as usize, 1));
    }
    let len_len = (first & 0x7f) as usize;
    if len_len == 0 || len_len > std::mem::size_of::<usize>() || input.len() < 1 + len_len {
        return Err(FipsQuicError::CertificateBinding(
            "invalid DER length".to_string(),
        ));
    }
    let mut len = 0usize;
    for byte in &input[1..1 + len_len] {
        len = (len << 8) | (*byte as usize);
    }
    Ok((len, 1 + len_len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn der_octet_string_round_trips() {
        let payload = vec![7u8; 300];
        let encoded = der_octet_string(&payload);
        assert_eq!(
            parse_der_octet_string(&encoded).unwrap(),
            payload.as_slice()
        );
    }

    #[test]
    fn generated_binding_uses_identity_npub() {
        let identity = Identity::generate();
        let binding = generate_identity_bound_certificate(&identity).unwrap();
        assert_eq!(binding.version, 1);
        assert_eq!(binding.npub, identity.npub());
        assert_eq!(binding.tls_public_key_sha256.len(), 64);
        assert_eq!(binding.signature.len(), 128);
    }

    #[cfg(feature = "nostr-discovery")]
    #[test]
    fn quic_responder_advertises_nat_endpoint_for_discovery() {
        let mut config = crate::config::NostrDiscoveryConfig::default();
        config.dm_relays = vec!["wss://relay.example".to_string()];
        config.stun_servers = vec!["stun:stun.example:3478".to_string()];

        let advert = build_quic_nat_overlay_advert(&config);

        assert_eq!(advert.endpoints.len(), 1);
        assert_eq!(
            advert.endpoints[0].transport,
            crate::discovery::nostr::OverlayTransportKind::Udp
        );
        assert_eq!(advert.endpoints[0].addr, "nat");
        assert_eq!(advert.signal_relays, Some(config.dm_relays));
        assert_eq!(advert.stun_servers, Some(config.stun_servers));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_quic_stream_over_direct_udp_with_mutual_fips_identity_binding() {
        let client_identity = Identity::generate();
        let server_identity = Identity::generate();
        let client_npub = client_identity.npub();
        let server_npub = server_identity.npub();
        let client_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let client_addr = client_socket.local_addr().unwrap();
        let server_addr = server_socket.local_addr().unwrap();

        let server_traversal = EstablishedTraversal::new(
            "direct-quic-test",
            client_npub.clone(),
            client_addr,
            server_socket,
        );
        let client_traversal = EstablishedTraversal::new(
            "direct-quic-test",
            server_npub.clone(),
            server_addr,
            client_socket,
        );

        let server = tokio::spawn(async move {
            accept_one_stream(
                &server_identity,
                server_traversal,
                b"quic-response",
                FipsQuicOptions {
                    timeout: Duration::from_secs(10),
                    ..FipsQuicOptions::default()
                },
            )
            .await
            .unwrap()
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        let client = connect_one_stream(
            &client_identity,
            client_traversal,
            b"quic-request",
            FipsQuicOptions {
                timeout: Duration::from_secs(10),
                ..FipsQuicOptions::default()
            },
        )
        .await
        .unwrap();

        let server = server.await.unwrap();
        assert_eq!(client.response, b"quic-response");
        assert_eq!(server.request, b"quic-request");
        assert_eq!(client.peer_npub, server_npub);
        assert_eq!(server.peer_npub, client_npub);
    }
}
