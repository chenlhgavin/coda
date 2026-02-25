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

/// Converts standard Markdown to Slack's `mrkdwn` format.
///
/// Handles both block-level elements (headings, list items) and inline
/// formatting (bold, italic, links). Code blocks and inline code are
/// preserved as-is since Slack supports them natively.
///
/// # Conversions
///
/// | Markdown | Slack mrkdwn |
/// |----------|-------------|
/// | `**bold**` / `__bold__` | `*bold*` |
/// | `# Heading` | `*Heading*` (bold) |
/// | `[text](url)` | `<url\|text>` |
/// | `- list item` / `* list item` | `\u{2022} list item` |
/// | `*italic*` | `_italic_` |
/// | `` `code` `` | preserved |
/// | ```` ``` ```` code blocks | preserved |
pub fn markdown_to_slack(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_code_block = false;

    for line in text.lines() {
        if !result.is_empty() {
            result.push('\n');
        }

        let trimmed = line.trim();

        // Toggle code block state on fences
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            result.push_str(line);
            continue;
        }

        // Don't convert anything inside code blocks
        if in_code_block {
            result.push_str(line);
            continue;
        }

        // Headings → bold
        if let Some(rest) = trimmed
            .strip_prefix("### ")
            .or_else(|| trimmed.strip_prefix("## "))
            .or_else(|| trimmed.strip_prefix("# "))
        {
            result.push('*');
            result.push_str(&convert_inline_to_slack(rest));
            result.push('*');
            continue;
        }

        // Unordered list: - item or * item → bullet
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            result.push('\u{2022}');
            result.push(' ');
            result.push_str(&convert_inline_to_slack(rest));
            continue;
        }

        // Regular line with inline conversions
        result.push_str(&convert_inline_to_slack(line));
    }

    result
}

/// Converts inline Markdown formatting to Slack mrkdwn within a single line.
///
/// Handles bold, italic, inline code (preserved), and links.
fn convert_inline_to_slack(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Inline code: preserve as-is (Slack supports backtick code)
        if chars[i] == '`' {
            result.push('`');
            i += 1;
            while i < len && chars[i] != '`' {
                result.push(chars[i]);
                i += 1;
            }
            if i < len {
                result.push('`');
                i += 1;
            }
            continue;
        }

        // Markdown link: [text](url) → <url|text>
        if chars[i] == '['
            && let Some((link_text, url, end)) = try_parse_link(&chars, i)
        {
            result.push('<');
            result.push_str(&url);
            result.push('|');
            result.push_str(&link_text);
            result.push('>');
            i = end;
            continue;
        }

        // Bold: **text** or __text__ → *text*
        if i + 1 < len
            && ((chars[i] == '*' && chars[i + 1] == '*')
                || (chars[i] == '_' && chars[i + 1] == '_'))
        {
            let delim = chars[i];
            i += 2;
            let start = i;
            while i + 1 < len && !(chars[i] == delim && chars[i + 1] == delim) {
                i += 1;
            }
            let bold_text: String = chars[start..i].iter().collect();
            result.push('*');
            result.push_str(&bold_text);
            result.push('*');
            if i + 1 < len {
                i += 2;
            }
            continue;
        }

        // Italic: *text* → _text_ (single star only, not at word boundary confusion)
        if chars[i] == '*' {
            let mut end = i + 1;
            while end < len && chars[end] != '*' {
                end += 1;
            }
            if end < len && end > i + 1 {
                let italic_text: String = chars[i + 1..end].iter().collect();
                result.push('_');
                result.push_str(&italic_text);
                result.push('_');
                i = end + 1;
                continue;
            }
        }

        result.push(chars[i]);
        i += 1;
    }

    result
}

/// Attempts to parse a Markdown link `[text](url)` starting at position `start`.
///
/// Returns `Some((text, url, end_pos))` if successful, where `end_pos` is the
/// index after the closing `)`.
fn try_parse_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let len = chars.len();
    if start >= len || chars[start] != '[' {
        return None;
    }

    // Find closing ]
    let mut i = start + 1;
    let mut link_text = String::new();
    while i < len && chars[i] != ']' {
        link_text.push(chars[i]);
        i += 1;
    }
    if i >= len {
        return None;
    }
    i += 1; // skip ]

    // Must be followed by (
    if i >= len || chars[i] != '(' {
        return None;
    }
    i += 1; // skip (

    // Find closing )
    let mut url = String::new();
    while i < len && chars[i] != ')' {
        url.push(chars[i]);
        i += 1;
    }
    if i >= len {
        return None;
    }
    i += 1; // skip )

    if link_text.is_empty() || url.is_empty() {
        return None;
    }

    Some((link_text, url, i))
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

    // ── markdown_to_slack ────────────────────────────────────────────

    #[test]
    fn test_should_convert_bold_double_star_to_slack() {
        assert_eq!(markdown_to_slack("**bold**"), "*bold*");
    }

    #[test]
    fn test_should_convert_bold_double_underscore_to_slack() {
        assert_eq!(markdown_to_slack("__bold__"), "*bold*");
    }

    #[test]
    fn test_should_convert_italic_star_to_slack() {
        assert_eq!(markdown_to_slack("*italic*"), "_italic_");
    }

    #[test]
    fn test_should_convert_heading_to_bold_in_slack() {
        assert_eq!(markdown_to_slack("# Title"), "*Title*");
        assert_eq!(markdown_to_slack("## Section"), "*Section*");
        assert_eq!(markdown_to_slack("### Sub"), "*Sub*");
    }

    #[test]
    fn test_should_convert_link_to_slack_format() {
        assert_eq!(
            markdown_to_slack("[click](https://example.com)"),
            "<https://example.com|click>"
        );
    }

    #[test]
    fn test_should_convert_unordered_list_dash_to_bullet() {
        assert_eq!(markdown_to_slack("- item"), "\u{2022} item");
    }

    #[test]
    fn test_should_convert_unordered_list_star_to_bullet() {
        assert_eq!(markdown_to_slack("* item"), "\u{2022} item");
    }

    #[test]
    fn test_should_preserve_code_blocks() {
        let input = "```\nlet x = **not bold**;\n```";
        let result = markdown_to_slack(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_should_preserve_inline_code() {
        assert_eq!(markdown_to_slack("`**code**`"), "`**code**`");
    }

    #[test]
    fn test_should_handle_mixed_content() {
        let input = "# Heading\n\n**bold** and *italic*\n\n- list item";
        let result = markdown_to_slack(input);
        assert!(result.contains("*Heading*"));
        assert!(result.contains("*bold*"));
        assert!(result.contains("_italic_"));
        assert!(result.contains("\u{2022} list item"));
    }

    #[test]
    fn test_should_handle_plain_text_unchanged() {
        assert_eq!(markdown_to_slack("plain text"), "plain text");
    }

    #[test]
    fn test_should_convert_heading_with_inline_formatting() {
        // Bold inside heading: **Bold** → *Bold* via inline conversion,
        // then wrapped in heading bold markers: *...*
        assert_eq!(markdown_to_slack("# **Bold** Heading"), "**Bold* Heading*");
    }
}
