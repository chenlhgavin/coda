//! Shared streaming utilities for Slack message updates.
//!
//! Provides constants and helpers used by both the `plan` and `run` command
//! handlers to stream incremental AI output into Slack messages while
//! respecting rate limits and message size constraints.

use std::time::Duration;

/// Minimum interval between debounced `chat.update` calls for streaming
/// content into Slack messages.
pub const STREAM_UPDATE_DEBOUNCE: Duration = Duration::from_secs(3);

/// Maximum length of inline content in a Slack section block.
/// Content exceeding this is either truncated or split into multiple messages.
pub const SLACK_SECTION_CHAR_LIMIT: usize = 3000;

/// Interval between heartbeat updates when the stream is idle or
/// text has exceeded the inline limit.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// Returns a UTF-8-safe truncated preview of `text` at
/// [`SLACK_SECTION_CHAR_LIMIT`].
pub fn truncated_preview(text: &str) -> &str {
    let mut end = SLACK_SECTION_CHAR_LIMIT.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Formats a tool activity line for display in Slack.
///
/// Produces `_:mag: tool_name summary_` or `_:mag: tool_name_` if
/// the summary is empty.
pub fn format_tool_activity(tool_name: &str, summary: &str) -> String {
    if summary.is_empty() {
        format!("_:mag: {tool_name}_")
    } else {
        format!("_:mag: {tool_name} {summary}_")
    }
}

/// Splits text into chunks of at most `max_len` bytes, preferring to
/// break at newline boundaries for cleaner output. Handles multi-byte
/// character boundaries safely.
pub fn split_into_chunks(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let remaining = &text[start..];
        if remaining.len() <= max_len {
            chunks.push(remaining);
            break;
        }

        // Find the largest valid byte offset within max_len
        let mut end = start + max_len;
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }

        let chunk = &text[start..end];

        // Prefer splitting at the last newline for cleaner breaks
        if let Some(last_newline) = chunk.rfind('\n') {
            let split_at = start + last_newline + 1;
            chunks.push(&text[start..split_at]);
            start = split_at;
        } else {
            chunks.push(chunk);
            start = end;
        }
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_define_slack_section_char_limit() {
        assert_eq!(SLACK_SECTION_CHAR_LIMIT, 3000);
    }

    #[test]
    fn test_should_define_stream_update_debounce() {
        assert_eq!(STREAM_UPDATE_DEBOUNCE, Duration::from_secs(3));
    }

    #[test]
    fn test_should_define_heartbeat_interval() {
        assert_eq!(HEARTBEAT_INTERVAL, Duration::from_secs(15));
    }

    #[test]
    fn test_should_return_single_chunk_for_short_text() {
        let chunks = split_into_chunks("hello world", 100);
        assert_eq!(chunks, vec!["hello world"]);
    }

    #[test]
    fn test_should_split_at_newline_boundary() {
        let text = "line1\nline2\nline3\nline4\n";
        let chunks = split_into_chunks(text, 12);
        assert_eq!(chunks[0], "line1\nline2\n");
        assert_eq!(chunks[1], "line3\nline4\n");
    }

    #[test]
    fn test_should_split_without_newline() {
        let text = "abcdefghij";
        let chunks = split_into_chunks(text, 4);
        assert_eq!(chunks, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn test_should_handle_exact_boundary() {
        let text = "abc";
        let chunks = split_into_chunks(text, 3);
        assert_eq!(chunks, vec!["abc"]);
    }

    #[test]
    fn test_should_handle_multibyte_chars() {
        // Each CJK char is 3 bytes in UTF-8
        let text = "你好世界测试";
        // max_len=9 fits exactly 3 CJK chars (9 bytes)
        let chunks = split_into_chunks(text, 9);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "你好世");
        assert_eq!(chunks[1], "界测试");
    }

    #[test]
    fn test_should_reassemble_to_original() {
        let text = "aaa\nbbb\nccc\nddd\neee\n";
        let chunks = split_into_chunks(text, 8);
        let reassembled: String = chunks.concat();
        assert_eq!(reassembled, text);
    }

    #[test]
    fn test_should_truncate_preview_short_text() {
        let text = "short text";
        assert_eq!(truncated_preview(text), text);
    }

    #[test]
    fn test_should_truncate_preview_long_text() {
        let text = "a".repeat(4000);
        let preview = truncated_preview(&text);
        assert_eq!(preview.len(), SLACK_SECTION_CHAR_LIMIT);
    }

    #[test]
    fn test_should_truncate_preview_multibyte_boundary() {
        // 1000 CJK chars = 3000 bytes, plus one more = 3003 bytes
        let text: String = "你".repeat(1001);
        let preview = truncated_preview(&text);
        assert!(preview.len() <= SLACK_SECTION_CHAR_LIMIT);
        assert!(preview.is_char_boundary(preview.len()));
    }

    #[test]
    fn test_should_format_tool_activity_with_summary() {
        let result = format_tool_activity("Bash", "cargo build");
        assert_eq!(result, "_:mag: Bash cargo build_");
    }

    #[test]
    fn test_should_format_tool_activity_without_summary() {
        let result = format_tool_activity("Bash", "");
        assert_eq!(result, "_:mag: Bash_");
    }
}
