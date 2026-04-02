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
// macOS: dedicated reader thread with async channel
//
// BPF fds don't support kqueue, so we can't use AsyncFd. Instead of
// spawn_blocking per packet (which was the bottleneck causing 84 Mbps),
// we spawn a single dedicated reader thread that loops on blocking
// read() and feeds frames through a tokio mpsc channel.
// =============================================================================

#[cfg(target_os = "macos")]
mod async_impl {
    use super::PacketSocket;
    use crate::transport::TransportError;
    use std::sync::Arc;

    /// A received frame: (payload, source_mac).
    type Frame = (Vec<u8>, [u8; 6]);

    pub struct AsyncPacketSocket {
        inner: Arc<PacketSocket>,
        rx: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Frame>>,
        _reader_thread: std::thread::JoinHandle<()>,
    }

    impl AsyncPacketSocket {
        pub fn new(socket: PacketSocket) -> Result<Self, TransportError> {
            // Channel capacity: buffer up to 1024 frames to decouple
            // the blocking reader from the async consumer.
            let (tx, rx) = tokio::sync::mpsc::channel::<Frame>(1024);
            let inner = Arc::new(socket);
            let reader_socket = Arc::clone(&inner);

            let reader_thread = std::thread::Builder::new()
                .name("bpf-reader".into())
                .spawn(move || {
                    let mut buf = vec![0u8; 65536];
                    loop {
                        match reader_socket.recv_from(&mut buf) {
                            Ok((n, mac)) => {
                                let data = buf[..n].to_vec();
                                if tx.blocking_send((data, mac)).is_err() {
                                    break; // Channel closed, shutting down
                                }
                            }
                            Err(e) => {
                                // EBADF means fd was closed (shutdown)
                                if e.raw_os_error() == Some(libc::EBADF) {
                                    break;
                                }
                                // Other errors: brief pause to avoid spinning
                                std::thread::sleep(std::time::Duration::from_millis(1));
                            }
                        }
                    }
                })
                .map_err(|e| TransportError::StartFailed(format!("reader thread: {}", e)))?;

            Ok(Self {
                inner,
                rx: tokio::sync::Mutex::new(rx),
                _reader_thread: reader_thread,
            })
        }

        pub async fn send_to(&self, data: &[u8], dest_mac: &[u8; 6]) -> Result<usize, TransportError> {
            self.inner
                .send_to(data, dest_mac)
                .map_err(|e| TransportError::SendFailed(format!("{}", e)))
        }

        pub async fn recv_from(
            &self,
            buf: &mut [u8],
        ) -> Result<(usize, [u8; 6]), TransportError> {
            let mut rx = self.rx.lock().await;
            match rx.recv().await {
                Some((data, mac)) => {
                    let n = data.len().min(buf.len());
                    buf[..n].copy_from_slice(&data[..n]);
                    Ok((n, mac))
                }
                None => Err(TransportError::RecvFailed("reader thread stopped".into())),
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
