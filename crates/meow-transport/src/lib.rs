//! Reusable composable stream-transport layers for meow-rs.
//!
//! Each layer wraps an inner [`Box<dyn Stream>`] and produces a new one.
//! Layers compose by chaining [`Transport::connect`] calls:
//!
//! ```text
//! let tcp:  Box<dyn Stream> = tcp_connect(addr).await?;
//! let s     = tls_layer.connect(tcp).await?;
//! let s     = ws_layer.connect(s).await?;
//! // `s` is handed to the VMess/VLESS protocol codec
//! ```
//!
//! Architecture: [ADR-0001](../../docs/adr/0001-meow-transport-crate.md).
//!
//! # Crate boundary invariants (enforced by CI)
//!
//! * No dependency on any other workspace crate (`meow-common`, `meow-proxy`,
//!   `meow-dns`, `meow-config`). This crate is a protocol-agnostic leaf.
//! * No `anyhow::Error` in any public function signature — only [`TransportError`].
//! * No server-side code (`accept`/`bind`/`listen`/`TcpListener`) in `src/`.
//!   Test helpers in `tests/support/` are whitelisted.

use std::any::Any;
use std::fmt;

use tokio::io::{AsyncRead, AsyncWrite};

pub use error::TransportError;

mod error;

#[cfg(feature = "tls")]
pub mod tls;

#[cfg(all(feature = "tls", feature = "boring-tls"))]
mod reality_tls;

#[cfg(feature = "ws")]
pub mod ws;

#[cfg(feature = "grpc")]
pub mod grpc;

#[cfg(feature = "h2")]
pub mod h2;

#[cfg(feature = "httpupgrade")]
pub mod httpupgrade;

/// A duplex byte stream — the currency passed between transport layers.
///
/// Blanket-implemented for every `T: AsyncRead + AsyncWrite + Unpin + Send + Sync`,
/// so `TcpStream`, `TlsStream<…>`, `WebSocketStream<…>`, etc. all qualify.
///
/// `Sync` is required (in addition to ADR-0001's `Send`) so that a
/// `Box<dyn Stream>` can satisfy `ProxyConn` in `meow-proxy`, which
/// requires `Sync` for connection-table access.  All concrete stream types
/// we use (`TcpStream`, `TlsStream`, `WsStream`) are `Sync`; the bound
/// adds no real restriction in practice.
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send + Sync + Any {
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: AsyncRead + AsyncWrite + Unpin + Send + Sync + Any> Stream for T {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamCapabilities {
    pub raw_read_passthrough: bool,
    pub raw_write_passthrough: bool,
}

impl StreamCapabilities {
    pub const NONE: Self = Self {
        raw_read_passthrough: false,
        raw_write_passthrough: false,
    };

    pub const RAW_PASSTHROUGH: Self = Self {
        raw_read_passthrough: true,
        raw_write_passthrough: true,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawPassthroughDirection {
    Read,
    Write,
}

impl fmt::Display for RawPassthroughDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read => f.write_str("read"),
            Self::Write => f.write_str("write"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawPassthroughError {
    direction: RawPassthroughDirection,
}

impl RawPassthroughError {
    fn unsupported(direction: RawPassthroughDirection) -> Self {
        Self { direction }
    }

    pub fn direction(self) -> RawPassthroughDirection {
        self.direction
    }
}

impl fmt::Display for RawPassthroughError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "stream does not support raw {} passthrough",
            self.direction
        )
    }
}

impl std::error::Error for RawPassthroughError {}

pub fn stream_capabilities(stream: &mut dyn Stream) -> StreamCapabilities {
    #[cfg(all(feature = "tls", feature = "boring-tls"))]
    {
        if stream
            .as_any_mut()
            .downcast_mut::<reality_tls::RealityTlsStream>()
            .is_some()
        {
            return StreamCapabilities::RAW_PASSTHROUGH;
        }
    }

    let _ = stream;
    StreamCapabilities::NONE
}

pub fn enable_raw_passthrough(stream: &mut dyn Stream) -> bool {
    let read = enable_raw_read_passthrough(stream);
    let write = enable_raw_write_passthrough(stream);
    read || write
}

pub fn try_enable_raw_read_passthrough(
    stream: &mut dyn Stream,
) -> std::result::Result<(), RawPassthroughError> {
    #[cfg(all(feature = "tls", feature = "boring-tls"))]
    {
        if let Some(reality) = stream
            .as_any_mut()
            .downcast_mut::<reality_tls::RealityTlsStream>()
        {
            reality.enable_raw_read_passthrough();
            return Ok(());
        }
    }

    let _ = stream;
    Err(RawPassthroughError::unsupported(
        RawPassthroughDirection::Read,
    ))
}

pub fn try_enable_raw_write_passthrough(
    stream: &mut dyn Stream,
) -> std::result::Result<(), RawPassthroughError> {
    #[cfg(all(feature = "tls", feature = "boring-tls"))]
    {
        if let Some(reality) = stream
            .as_any_mut()
            .downcast_mut::<reality_tls::RealityTlsStream>()
        {
            reality.enable_raw_write_passthrough();
            return Ok(());
        }
    }

    let _ = stream;
    Err(RawPassthroughError::unsupported(
        RawPassthroughDirection::Write,
    ))
}

pub fn enable_raw_read_passthrough(stream: &mut dyn Stream) -> bool {
    try_enable_raw_read_passthrough(stream).is_ok()
}

pub fn enable_raw_write_passthrough(stream: &mut dyn Stream) -> bool {
    try_enable_raw_write_passthrough(stream).is_ok()
}

/// A transport layer that wraps an inner [`Stream`] and produces a new one.
///
/// Implementations are cheap to clone (typically an `Arc<Config>` inside).
/// The trait is object-safe: `Box<dyn Transport>` is valid.
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    /// Wrap `inner` with this transport layer and return the upgraded stream.
    async fn connect(&self, inner: Box<dyn Stream>) -> Result<Box<dyn Stream>>;
}

/// Crate-level `Result` alias.  Errors are always [`TransportError`].
pub type Result<T> = std::result::Result<T, TransportError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_stream_reports_no_raw_passthrough_capability() {
        let (mut stream, _peer) = tokio::io::duplex(64);

        assert_eq!(stream_capabilities(&mut stream), StreamCapabilities::NONE);
        let err = try_enable_raw_read_passthrough(&mut stream).unwrap_err();
        assert_eq!(err.direction(), RawPassthroughDirection::Read);
        let err = try_enable_raw_write_passthrough(&mut stream).unwrap_err();
        assert_eq!(err.direction(), RawPassthroughDirection::Write);
    }
}
