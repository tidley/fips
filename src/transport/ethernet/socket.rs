//! Raw Ethernet socket abstraction.
//!
//! Platform-specific implementations live in `socket_linux.rs` (AF_PACKET)
//! and `socket_macos.rs` (BPF). This module re-exports `PacketSocket` and
//! provides `AsyncPacketSocket`.

use crate::transport::TransportError;

/// Broadcast MAC address.
pub const ETHERNET_BROADCAST: [u8; 6] = [0xff; 6];

// Platform-specific PacketSocket implementation.
#[cfg(target_os = "linux")]
#[path = "socket_linux.rs"]
mod platform;

#[cfg(target_os = "macos")]
#[path = "socket_macos.rs"]
mod platform;

pub use platform::PacketSocket;

// =============================================================================
// Linux: AsyncFd-based async wrapper
// =============================================================================

#[cfg(target_os = "linux")]
mod async_impl {
    use super::PacketSocket;
    use crate::transport::TransportError;
    use tokio::io::unix::AsyncFd;

    pub struct AsyncPacketSocket {
        inner: AsyncFd<PacketSocket>,
    }

    impl AsyncPacketSocket {
        pub fn new(socket: PacketSocket) -> Result<Self, TransportError> {
            let async_fd = AsyncFd::new(socket)
                .map_err(|e| TransportError::StartFailed(format!("AsyncFd::new failed: {}", e)))?;
            Ok(Self { inner: async_fd })
        }

        pub async fn send_to(&self, data: &[u8], dest_mac: &[u8; 6]) -> Result<usize, TransportError> {
            loop {
                let mut guard = self
                    .inner
                    .writable()
                    .await
                    .map_err(|e| TransportError::SendFailed(format!("writable wait: {}", e)))?;

                match guard.try_io(|inner| inner.get_ref().send_to(data, dest_mac)) {
                    Ok(Ok(n)) => return Ok(n),
                    Ok(Err(e)) => return Err(TransportError::SendFailed(format!("{}", e))),
                    Err(_would_block) => continue,
                }
            }
        }

        pub async fn recv_from(
            &self,
            buf: &mut [u8],
        ) -> Result<(usize, [u8; 6]), TransportError> {
            loop {
                let mut guard = self
                    .inner
                    .readable()
                    .await
                    .map_err(|e| TransportError::RecvFailed(format!("readable wait: {}", e)))?;

                match guard.try_io(|inner| inner.get_ref().recv_from(buf)) {
                    Ok(Ok(result)) => return Ok(result),
                    Ok(Err(e)) => return Err(TransportError::RecvFailed(format!("{}", e))),
                    Err(_would_block) => continue,
                }
            }
        }

        pub fn get_ref(&self) -> &PacketSocket {
            self.inner.get_ref()
        }
    }
}

// =============================================================================
// macOS: blocking I/O with tokio spawn_blocking for recv
// =============================================================================

#[cfg(target_os = "macos")]
mod async_impl {
    use super::PacketSocket;
    use crate::transport::TransportError;
    use std::sync::Arc;

    pub struct AsyncPacketSocket {
        inner: Arc<PacketSocket>,
    }

    impl AsyncPacketSocket {
        pub fn new(socket: PacketSocket) -> Result<Self, TransportError> {
            Ok(Self {
                inner: Arc::new(socket),
            })
        }

        pub async fn send_to(&self, data: &[u8], dest_mac: &[u8; 6]) -> Result<usize, TransportError> {
            // BPF writes don't block meaningfully; call directly.
            self.inner
                .send_to(data, dest_mac)
                .map_err(|e| TransportError::SendFailed(format!("{}", e)))
        }

        pub async fn recv_from(
            &self,
            buf: &mut [u8],
        ) -> Result<(usize, [u8; 6]), TransportError> {
            // BPF reads block until a packet arrives. Use spawn_blocking
            // to avoid blocking the tokio runtime.
            let socket = Arc::clone(&self.inner);
            let buf_len = buf.len();

            let result = tokio::task::spawn_blocking(move || {
                let mut local_buf = vec![0u8; buf_len];
                socket.recv_from(&mut local_buf).map(|(n, mac)| (local_buf, n, mac))
            })
            .await
            .map_err(|e| TransportError::RecvFailed(format!("spawn_blocking: {}", e)))?;

            match result {
                Ok((local_buf, n, mac)) => {
                    buf[..n].copy_from_slice(&local_buf[..n]);
                    Ok((n, mac))
                }
                Err(e) => Err(TransportError::RecvFailed(format!("{}", e))),
            }
        }

        pub fn get_ref(&self) -> &PacketSocket {
            &self.inner
        }
    }
}

pub use async_impl::AsyncPacketSocket;

impl PacketSocket {
    /// Wrap this socket in an async wrapper for tokio integration.
    pub fn into_async(self) -> Result<AsyncPacketSocket, TransportError> {
        AsyncPacketSocket::new(self)
    }
}
