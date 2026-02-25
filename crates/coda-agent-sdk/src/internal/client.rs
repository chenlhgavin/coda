//! Internal client implementation.
//!
//! Delegates to the appropriate [`Backend`](crate::backend::Backend) based on
//! the `backend` field in [`AgentOptions`](crate::options::AgentOptions).

use crate::backend::{BackendKind, create_backend};
use crate::error::Result;
use crate::options::AgentOptions;
use crate::types::{Message, Prompt};
use futures::Stream;
use std::pin::Pin;

pub struct InternalClient;

impl InternalClient {
    pub fn new() -> Self {
        Self
    }

    pub fn process_query(
        &self,
        prompt: Prompt,
        options: AgentOptions,
    ) -> Pin<Box<dyn Stream<Item = Result<Message>> + Send>> {
        let kind = options.backend.unwrap_or(BackendKind::Claude);
        let backend = create_backend(kind);

        match backend.one_shot_query(prompt, &options) {
            Ok(stream) => stream,
            Err(e) => Box::pin(futures::stream::once(async move { Err(e) })),
        }
    }
}

impl Default for InternalClient {
    fn default() -> Self {
        Self::new()
    }
}
