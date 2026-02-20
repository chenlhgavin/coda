//! Envelope-type handlers for Socket Mode events.
//!
//! Each submodule handles a specific envelope type:
//! - [`commands`] — slash command parsing and dispatch
//! - [`events`] — Events API (plan thread message routing)
//! - [`interactions`] — interactive component actions (button clicks)

pub mod commands;
pub mod events;
pub mod interactions;
