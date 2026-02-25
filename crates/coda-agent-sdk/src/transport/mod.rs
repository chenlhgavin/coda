//! Transport implementations for Claude SDK.

mod subprocess_cli;

pub use subprocess_cli::SubprocessCliTransport;

use crate::error::Result;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

/// Abstract transport for Claude communication.
///
/// This is a low-level transport interface that handles raw I/O with the Claude
/// process or service.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Connect the transport and prepare for communication.
    async fn connect(&mut self) -> Result<()>;

    /// Write raw data to the transport.
    async fn write(&mut self, data: &str) -> Result<()>;

    /// Read and parse messages from the transport.
    /// Returns an owned stream (does not borrow self) so transport can be used for write/close concurrently.
    fn read_messages(&mut self) -> Pin<Box<dyn Stream<Item = Result<serde_json::Value>> + Send>>;

    /// Close the transport connection.
    async fn close(&mut self) -> Result<()>;

    /// Check if transport is ready for communication.
    fn is_ready(&self) -> bool;

    /// End the input stream (close stdin for process transports).
    async fn end_input(&mut self) -> Result<()>;
}
