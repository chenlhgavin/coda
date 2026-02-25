//! Message parser re-export for backward compatibility.
//!
//! The canonical implementation lives at [`crate::backend::claude::message_parser`].

pub use crate::backend::claude::message_parser::parse_message;
