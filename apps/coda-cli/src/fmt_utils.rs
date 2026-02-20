//! Shared formatting utilities for the CODA CLI.
//!
//! Common helpers used across the app layer, init TUI, and run TUI.

use std::time::Duration;

/// Formats a `Duration` into a human-readable string (e.g., `"1m 23s"`, `"45s"`).
pub fn format_duration(d: Duration) -> String {
    let total_secs = d.as_secs();
    if total_secs >= 3600 {
        let hours = total_secs / 3600;
        let mins = (total_secs % 3600) / 60;
        let secs = total_secs % 60;
        format!("{hours}h {mins}m {secs}s")
    } else if total_secs >= 60 {
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        format!("{mins}m {secs}s")
    } else {
        format!("{total_secs}s")
    }
}

/// Truncates a string to fit within `max_len` characters, appending `…` if needed.
///
/// Uses character boundaries instead of byte offsets to avoid panics on
/// multi-byte UTF-8 sequences (e.g., CJK characters, emoji).
pub fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(1)).collect();
        format!("{truncated}\u{2026}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_format_duration_seconds() {
        assert_eq!(format_duration(Duration::from_secs(0)), "0s");
        assert_eq!(format_duration(Duration::from_secs(45)), "45s");
    }

    #[test]
    fn test_should_format_duration_minutes() {
        assert_eq!(format_duration(Duration::from_secs(90)), "1m 30s");
    }

    #[test]
    fn test_should_format_duration_hours() {
        assert_eq!(format_duration(Duration::from_secs(3661)), "1h 1m 1s");
    }

    #[test]
    fn test_should_truncate_long_string() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 8), "hello w\u{2026}");
    }

    #[test]
    fn test_should_not_truncate_short_string() {
        assert_eq!(truncate_str("hi", 10), "hi");
    }

    #[test]
    fn test_should_handle_exact_length() {
        assert_eq!(truncate_str("12345", 5), "12345");
    }

    #[test]
    fn test_should_truncate_to_one_char() {
        assert_eq!(truncate_str("hello", 1), "\u{2026}");
    }
}
