//! Streaming-friendly Markdown-to-ratatui renderer.
//!
//! Converts Markdown text (headings, bold, italic, code blocks, lists,
//! blockquotes, horizontal rules) into styled [`Line`]/[`Span`] objects
//! suitable for rendering in ratatui `Paragraph` widgets.
//!
//! The [`MarkdownRenderer`] struct tracks state (code-block mode) across
//! streaming chunks. When a `\n` arrives, the completed line is parsed
//! and styled. The incomplete trailing fragment stays as plain text until
//! more content arrives.

use ratatui::prelude::*;

/// Streaming markdown renderer that accumulates text and converts
/// complete lines into styled ratatui [`Line`] objects.
///
/// Tracks whether we are inside a fenced code block across multiple
/// `append_text` calls, enabling correct incremental rendering of
/// streaming AI output.
#[derive(Debug, Default)]
pub struct MarkdownRenderer {
    /// Whether we are currently inside a fenced code block (``` ... ```).
    in_code_block: bool,
    /// Trailing text fragment that has not yet been terminated by `\n`.
    pending: String,
}

impl MarkdownRenderer {
    /// Creates a new renderer with no pending state.
    pub fn new() -> Self {
        Self {
            in_code_block: false,
            pending: String::new(),
        }
    }

    /// Appends streaming text, converting complete lines into styled
    /// [`Line`] objects and pushing them onto `buffer`.
    ///
    /// Incomplete trailing text (no newline yet) is held internally and
    /// will be flushed on the next call that completes the line.
    pub fn append_text(&mut self, buffer: &mut Vec<Line<'static>>, text: &str) {
        // Combine pending text with the new chunk
        let combined = if self.pending.is_empty() {
            text.to_string()
        } else {
            let mut s = std::mem::take(&mut self.pending);
            s.push_str(text);
            s
        };

        let mut parts = combined.split('\n').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_some() {
                // This line is terminated by \n — parse and style it
                self.finalize_line(buffer, part);
            } else {
                // Last fragment — no trailing newline, hold as pending
                self.pending = part.to_string();
            }
        }
    }

    /// Parses and appends a single completed line to the buffer, updating
    /// code-block state as needed.
    fn finalize_line(&mut self, buffer: &mut Vec<Line<'static>>, line: &str) {
        let trimmed = line.trim();

        // Toggle code-block mode on fences
        if trimmed.starts_with("```") {
            self.in_code_block = !self.in_code_block;
            buffer.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(Color::DarkGray),
            )));
            return;
        }

        if self.in_code_block {
            // Lines inside a code block: green monospace look
            buffer.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(Color::Green),
            )));
            return;
        }

        // Delegate to the line parser for block- and inline-level styling
        buffer.push(parse_markdown_line(line));
    }
}

/// Parses a single complete markdown line into a styled [`Line`].
///
/// Handles block-level elements (headings, lists, blockquotes, horizontal
/// rules, blank lines) and delegates inline formatting to [`parse_inline`].
///
/// # Examples
///
/// ```
/// use coda_cli::markdown::parse_markdown_line;
///
/// let line = parse_markdown_line("## Section Title");
/// assert!(!line.spans.is_empty());
/// ```
pub fn parse_markdown_line(line: &str) -> Line<'static> {
    let trimmed = line.trim();

    // Blank line — use explicit Span to ensure at least one span exists
    if trimmed.is_empty() {
        return Line::from(Span::raw(String::new()));
    }

    // Horizontal rule: ---, ***, ___
    if is_horizontal_rule(trimmed) {
        let rule = "\u{2500}".repeat(40);
        return Line::from(Span::styled(rule, Style::default().fg(Color::DarkGray)));
    }

    // Headings
    if let Some(rest) = trimmed.strip_prefix("### ") {
        return Line::from(Span::styled(
            rest.to_string(),
            Style::default().fg(Color::DarkGray).bold(),
        ));
    }
    if let Some(rest) = trimmed.strip_prefix("## ") {
        return Line::from(Span::styled(
            rest.to_string(),
            Style::default().fg(Color::White).bold(),
        ));
    }
    if let Some(rest) = trimmed.strip_prefix("# ") {
        return Line::from(Span::styled(
            rest.to_string(),
            Style::default().fg(Color::Cyan).bold(),
        ));
    }

    // Blockquote
    if let Some(rest) = trimmed.strip_prefix("> ") {
        return Line::from(Span::styled(
            rest.to_string(),
            Style::default().fg(Color::DarkGray).italic(),
        ));
    }
    if trimmed == ">" {
        return Line::from(Span::styled(
            String::new(),
            Style::default().fg(Color::DarkGray).italic(),
        ));
    }

    // Unordered list: - item or * item
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        let mut spans = vec![Span::styled(
            "  \u{2022} ".to_string(),
            Style::default().fg(Color::Cyan),
        )];
        spans.extend(parse_inline(rest, Style::default().fg(Color::White)));
        return Line::from(spans);
    }

    // Ordered list: N. item
    if let Some(pos) = trimmed.find(". ") {
        let prefix = &trimmed[..pos];
        if !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit()) {
            let rest = &trimmed[pos + 2..];
            let mut spans = vec![Span::styled(
                format!("  {prefix}. "),
                Style::default().fg(Color::Cyan),
            )];
            spans.extend(parse_inline(rest, Style::default().fg(Color::White)));
            return Line::from(spans);
        }
    }

    // Regular text with inline formatting
    Line::from(parse_inline(trimmed, Style::default().fg(Color::White)))
}

/// Parses inline markdown formatting within a line, producing a
/// vector of styled [`Span`]s.
///
/// Handles:
/// - `**bold**` / `__bold__` → bold
/// - `*italic*` / `_italic_` → italic
/// - `` `code` `` → green
///
/// # Examples
///
/// ```
/// use ratatui::prelude::*;
/// use coda_cli::markdown::parse_inline;
///
/// let spans = parse_inline("hello **world**", Style::default().fg(Color::White));
/// assert_eq!(spans.len(), 2);
/// ```
pub fn parse_inline(text: &str, base_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut buf = String::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Inline code: `code`
        if chars[i] == '`' {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), base_style));
            }
            i += 1;
            let start = i;
            while i < len && chars[i] != '`' {
                i += 1;
            }
            let code: String = chars[start..i].iter().collect();
            spans.push(Span::styled(code, Style::default().fg(Color::Green)));
            if i < len {
                i += 1; // skip closing `
            }
            continue;
        }

        // Bold: **text** or __text__
        if i + 1 < len
            && ((chars[i] == '*' && chars[i + 1] == '*')
                || (chars[i] == '_' && chars[i + 1] == '_'))
        {
            let delim = chars[i];
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), base_style));
            }
            i += 2;
            let start = i;
            // Find closing delimiter
            while i + 1 < len && !(chars[i] == delim && chars[i + 1] == delim) {
                i += 1;
            }
            let bold_text: String = chars[start..i].iter().collect();
            spans.push(Span::styled(
                bold_text,
                Style::default().fg(Color::White).bold(),
            ));
            if i + 1 < len {
                i += 2; // skip closing **
            }
            continue;
        }

        // Italic: *text* or _text_ (single delimiter, not at word boundary for _)
        if chars[i] == '*' || chars[i] == '_' {
            let delim = chars[i];
            // Look ahead for closing delimiter
            let mut end = i + 1;
            while end < len && chars[end] != delim {
                end += 1;
            }
            if end < len && end > i + 1 {
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut buf), base_style));
                }
                i += 1;
                let italic_text: String = chars[i..end].iter().collect();
                spans.push(Span::styled(
                    italic_text,
                    Style::default().fg(Color::White).italic(),
                ));
                i = end + 1;
                continue;
            }
        }

        buf.push(chars[i]);
        i += 1;
    }

    if !buf.is_empty() {
        spans.push(Span::styled(buf, base_style));
    }

    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base_style));
    }

    spans
}

/// Returns `true` if the trimmed line is a horizontal rule (`---`, `***`,
/// or `___` with at least 3 characters and optional spaces).
fn is_horizontal_rule(trimmed: &str) -> bool {
    if trimmed.len() < 3 {
        return false;
    }
    let chars: Vec<char> = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    if chars.len() < 3 {
        return false;
    }
    let first = chars[0];
    (first == '-' || first == '*' || first == '_') && chars.iter().all(|&c| c == first)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── MarkdownRenderer ─────────────────────────────────────────────

    #[test]
    fn test_should_render_plain_text() {
        let mut renderer = MarkdownRenderer::new();
        let mut buf: Vec<Line<'static>> = Vec::new();
        renderer.append_text(&mut buf, "hello world\n");
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].spans[0].content.as_ref(), "hello world");
    }

    #[test]
    fn test_should_hold_pending_text_without_newline() {
        let mut renderer = MarkdownRenderer::new();
        let mut buf: Vec<Line<'static>> = Vec::new();
        renderer.append_text(&mut buf, "partial");
        assert!(buf.is_empty());
        renderer.append_text(&mut buf, " text\n");
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].spans[0].content.as_ref(), "partial text");
    }

    #[test]
    fn test_should_track_code_block_state() {
        let mut renderer = MarkdownRenderer::new();
        let mut buf: Vec<Line<'static>> = Vec::new();
        renderer.append_text(&mut buf, "```\nlet x = 1;\n```\n");
        assert_eq!(buf.len(), 3);
        // First line is the opening fence (DarkGray)
        assert_eq!(buf[0].spans[0].style.fg, Some(Color::DarkGray));
        // Code line is Green
        assert_eq!(buf[1].spans[0].style.fg, Some(Color::Green));
        // Closing fence is DarkGray
        assert_eq!(buf[2].spans[0].style.fg, Some(Color::DarkGray));
        assert!(!renderer.in_code_block);
    }

    #[test]
    fn test_should_handle_multiple_streaming_chunks() {
        let mut renderer = MarkdownRenderer::new();
        let mut buf: Vec<Line<'static>> = Vec::new();
        renderer.append_text(&mut buf, "# He");
        renderer.append_text(&mut buf, "ading\n");
        renderer.append_text(&mut buf, "body\n");
        assert_eq!(buf.len(), 2);
        // Heading should be bold + cyan
        assert_eq!(buf[0].spans[0].content.as_ref(), "Heading");
        assert!(buf[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(buf[0].spans[0].style.fg, Some(Color::Cyan));
    }

    #[test]
    fn test_should_persist_code_block_across_chunks() {
        let mut renderer = MarkdownRenderer::new();
        let mut buf: Vec<Line<'static>> = Vec::new();
        renderer.append_text(&mut buf, "```\n");
        assert!(renderer.in_code_block);
        renderer.append_text(&mut buf, "code line\n");
        assert_eq!(buf[1].spans[0].style.fg, Some(Color::Green));
        renderer.append_text(&mut buf, "```\n");
        assert!(!renderer.in_code_block);
    }

    // ── parse_markdown_line ──────────────────────────────────────────

    #[test]
    fn test_should_parse_h1_heading() {
        let line = parse_markdown_line("# Title");
        assert_eq!(line.spans[0].content.as_ref(), "Title");
        assert_eq!(line.spans[0].style.fg, Some(Color::Cyan));
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_should_parse_h2_heading() {
        let line = parse_markdown_line("## Section");
        assert_eq!(line.spans[0].content.as_ref(), "Section");
        assert_eq!(line.spans[0].style.fg, Some(Color::White));
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_should_parse_h3_heading() {
        let line = parse_markdown_line("### Subsection");
        assert_eq!(line.spans[0].content.as_ref(), "Subsection");
        assert_eq!(line.spans[0].style.fg, Some(Color::DarkGray));
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_should_parse_unordered_list_dash() {
        let line = parse_markdown_line("- item one");
        assert_eq!(line.spans[0].content.as_ref(), "  \u{2022} ");
        assert_eq!(line.spans[0].style.fg, Some(Color::Cyan));
        assert_eq!(line.spans[1].content.as_ref(), "item one");
    }

    #[test]
    fn test_should_parse_unordered_list_star() {
        let line = parse_markdown_line("* item two");
        assert_eq!(line.spans[0].content.as_ref(), "  \u{2022} ");
    }

    #[test]
    fn test_should_parse_ordered_list() {
        let line = parse_markdown_line("1. first item");
        assert_eq!(line.spans[0].content.as_ref(), "  1. ");
        assert_eq!(line.spans[0].style.fg, Some(Color::Cyan));
        assert_eq!(line.spans[1].content.as_ref(), "first item");
    }

    #[test]
    fn test_should_parse_blockquote() {
        let line = parse_markdown_line("> some quote");
        assert_eq!(line.spans[0].content.as_ref(), "some quote");
        assert_eq!(line.spans[0].style.fg, Some(Color::DarkGray));
        assert!(line.spans[0].style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn test_should_parse_horizontal_rule() {
        let line = parse_markdown_line("---");
        assert_eq!(line.spans[0].style.fg, Some(Color::DarkGray));
        assert!(line.spans[0].content.contains('\u{2500}'));
    }

    #[test]
    fn test_should_parse_blank_line() {
        let line = parse_markdown_line("");
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content.as_ref(), "");
    }

    // ── parse_inline ─────────────────────────────────────────────────

    #[test]
    fn test_should_parse_bold_double_star() {
        let spans = parse_inline("hello **world**", Style::default().fg(Color::White));
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content.as_ref(), "hello ");
        assert_eq!(spans[1].content.as_ref(), "world");
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_should_parse_bold_double_underscore() {
        let spans = parse_inline("__bold__", Style::default().fg(Color::White));
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "bold");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_should_parse_italic_single_star() {
        let spans = parse_inline("*italic*", Style::default().fg(Color::White));
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "italic");
        assert!(spans[0].style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn test_should_parse_inline_code() {
        let spans = parse_inline("use `cargo build`", Style::default().fg(Color::White));
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content.as_ref(), "use ");
        assert_eq!(spans[1].content.as_ref(), "cargo build");
        assert_eq!(spans[1].style.fg, Some(Color::Green));
    }

    #[test]
    fn test_should_parse_plain_text_unchanged() {
        let spans = parse_inline("no formatting here", Style::default().fg(Color::White));
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "no formatting here");
    }

    #[test]
    fn test_should_parse_mixed_inline() {
        let spans = parse_inline(
            "**bold** and `code` and *italic*",
            Style::default().fg(Color::White),
        );
        assert!(spans.len() >= 5);
    }

    // ── is_horizontal_rule ───────────────────────────────────────────

    #[test]
    fn test_should_detect_dash_rule() {
        assert!(is_horizontal_rule("---"));
    }

    #[test]
    fn test_should_detect_star_rule() {
        assert!(is_horizontal_rule("***"));
    }

    #[test]
    fn test_should_detect_underscore_rule() {
        assert!(is_horizontal_rule("___"));
    }

    #[test]
    fn test_should_reject_short_rule() {
        assert!(!is_horizontal_rule("--"));
    }

    #[test]
    fn test_should_reject_mixed_chars() {
        assert!(!is_horizontal_rule("-*-"));
    }
}
