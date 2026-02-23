//! Readline-style line editor for TUI input fields.
//!
//! Provides a [`LineEditor`] that handles common Emacs/readline keybindings
//! for inline text editing within a ratatui-based terminal UI.
//!
//! # Supported keybindings
//!
//! | Key              | Action                          |
//! |------------------|---------------------------------|
//! | `Left` / `Ctrl+B`  | Move cursor back one char    |
//! | `Right` / `Ctrl+F` | Move cursor forward one char |
//! | `Home` / `Ctrl+A`  | Move cursor to start of line |
//! | `End` / `Ctrl+E`   | Move cursor to end of line   |
//! | `Alt+B`            | Move cursor back one word    |
//! | `Alt+F`            | Move cursor forward one word |
//! | `Backspace` / `Ctrl+H` | Delete char before cursor |
//! | `Delete` / `Ctrl+D`   | Delete char at cursor      |
//! | `Ctrl+W`          | Delete word before cursor      |
//! | `Alt+D`           | Delete word after cursor       |
//! | `Ctrl+U`          | Delete from cursor to start    |
//! | `Ctrl+K`          | Delete from cursor to end      |

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

/// A readline-style single-line editor.
///
/// Tracks both the text buffer and the cursor position (in character
/// units, not bytes) so the host widget only needs to read [`content`]
/// and [`cursor`] when rendering.
#[derive(Debug)]
pub struct LineEditor {
    buf: String,
    /// Cursor position in **character** (not byte) units.
    cursor: usize,
}

impl LineEditor {
    /// Creates a new, empty editor.
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            cursor: 0,
        }
    }

    // ── Accessors ──────────────────────────────────────────────────

    /// Returns the current text content.
    pub fn content(&self) -> &str {
        &self.buf
    }

    /// Returns the cursor position in character units (test-only).
    #[cfg(test)]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Returns the display width (in terminal columns) of the entire buffer.
    pub fn display_width(&self) -> usize {
        self.buf
            .chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum()
    }

    /// Returns the display width (in terminal columns) of the text
    /// before the cursor.
    pub fn cursor_display_offset(&self) -> usize {
        self.buf
            .chars()
            .take(self.cursor)
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum()
    }

    /// Returns the number of characters in the buffer.
    fn char_count(&self) -> usize {
        self.buf.chars().count()
    }

    // ── Public API ─────────────────────────────────────────────────

    /// Handles a key event. Returns `true` if the event was consumed.
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        match key.code {
            // ── Enter / submit is NOT handled here ────────────
            KeyCode::Enter => return false,

            // ── Movement ──────────────────────────────────────
            KeyCode::Left if !alt => self.move_left(),
            KeyCode::Right if !alt => self.move_right(),
            KeyCode::Home => self.move_home(),
            KeyCode::End => self.move_end(),

            // Ctrl+Left / Alt+Left: move back one word
            KeyCode::Left if alt => self.move_word_back(),
            // Ctrl+Right / Alt+Right: move forward one word
            KeyCode::Right if alt => self.move_word_forward(),

            // ── Deletion ──────────────────────────────────────
            KeyCode::Backspace => self.delete_char_back(),
            KeyCode::Delete => self.delete_char_forward(),

            // ── Ctrl keybindings ──────────────────────────────
            KeyCode::Char('a') if ctrl => self.move_home(),
            KeyCode::Char('e') if ctrl => self.move_end(),
            KeyCode::Char('b') if ctrl => self.move_left(),
            KeyCode::Char('f') if ctrl => self.move_right(),
            KeyCode::Char('h') if ctrl => self.delete_char_back(),
            KeyCode::Char('d') if ctrl => self.delete_char_forward(),
            KeyCode::Char('w') if ctrl => self.delete_word_back(),
            KeyCode::Char('u') if ctrl => self.delete_to_start(),
            KeyCode::Char('k') if ctrl => self.delete_to_end(),

            // ── Alt keybindings ───────────────────────────────
            KeyCode::Char('b') if alt => self.move_word_back(),
            KeyCode::Char('f') if alt => self.move_word_forward(),
            KeyCode::Char('d') if alt => self.delete_word_forward(),

            // ── Regular character insertion ────────────────────
            KeyCode::Char(c) if !ctrl && !alt => self.insert_char(c),

            // Everything else is not consumed.
            _ => return false,
        }
        true
    }

    /// Clears the buffer and resets the cursor.
    pub fn clear(&mut self) {
        self.buf.clear();
        self.cursor = 0;
    }

    /// Takes the current content, clearing the editor. Returns the
    /// trimmed string.
    pub fn take_trimmed(&mut self) -> String {
        let s = self.buf.trim().to_string();
        self.clear();
        s
    }

    // ── Movement helpers ──────────────────────────────────────────

    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        if self.cursor < self.char_count() {
            self.cursor += 1;
        }
    }

    fn move_home(&mut self) {
        self.cursor = 0;
    }

    fn move_end(&mut self) {
        self.cursor = self.char_count();
    }

    fn move_word_back(&mut self) {
        let chars: Vec<char> = self.buf.chars().collect();
        let mut pos = self.cursor;
        // Skip whitespace
        while pos > 0 && !chars[pos - 1].is_alphanumeric() {
            pos -= 1;
        }
        // Skip word characters
        while pos > 0 && chars[pos - 1].is_alphanumeric() {
            pos -= 1;
        }
        self.cursor = pos;
    }

    fn move_word_forward(&mut self) {
        let chars: Vec<char> = self.buf.chars().collect();
        let len = chars.len();
        let mut pos = self.cursor;
        // Skip current word characters
        while pos < len && chars[pos].is_alphanumeric() {
            pos += 1;
        }
        // Skip whitespace
        while pos < len && !chars[pos].is_alphanumeric() {
            pos += 1;
        }
        self.cursor = pos;
    }

    // ── Deletion helpers ──────────────────────────────────────────

    fn insert_char(&mut self, c: char) {
        let byte_idx = self.char_to_byte(self.cursor);
        self.buf.insert(byte_idx, c);
        self.cursor += 1;
    }

    fn delete_char_back(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let byte_idx = self.char_to_byte(self.cursor);
            self.buf.remove(byte_idx);
        }
    }

    fn delete_char_forward(&mut self) {
        if self.cursor < self.char_count() {
            let byte_idx = self.char_to_byte(self.cursor);
            self.buf.remove(byte_idx);
        }
    }

    fn delete_word_back(&mut self) {
        let old = self.cursor;
        self.move_word_back();
        let new = self.cursor;
        if new < old {
            let start = self.char_to_byte(new);
            let end = self.char_to_byte(old);
            self.buf.drain(start..end);
        }
    }

    fn delete_word_forward(&mut self) {
        let start_cursor = self.cursor;
        let chars: Vec<char> = self.buf.chars().collect();
        let len = chars.len();
        let mut pos = start_cursor;
        // If between words (on non-word chars), skip to the next word first
        while pos < len && !chars[pos].is_alphanumeric() {
            pos += 1;
        }
        // Then skip the word characters to reach the word end
        while pos < len && chars[pos].is_alphanumeric() {
            pos += 1;
        }
        if pos > start_cursor {
            let start = self.char_to_byte(start_cursor);
            let end = self.char_to_byte(pos);
            self.buf.drain(start..end);
        }
    }

    fn delete_to_start(&mut self) {
        if self.cursor > 0 {
            let byte_idx = self.char_to_byte(self.cursor);
            self.buf.drain(..byte_idx);
            self.cursor = 0;
        }
    }

    fn delete_to_end(&mut self) {
        let byte_idx = self.char_to_byte(self.cursor);
        self.buf.truncate(byte_idx);
    }

    // ── Utilities ─────────────────────────────────────────────────

    /// Converts a character-based position to a byte index.
    fn char_to_byte(&self, char_pos: usize) -> usize {
        self.buf
            .char_indices()
            .nth(char_pos)
            .map_or(self.buf.len(), |(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn alt(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::ALT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn type_str(editor: &mut LineEditor, s: &str) {
        for c in s.chars() {
            editor.handle_key(&press(KeyCode::Char(c)));
        }
    }

    #[test]
    fn test_should_insert_and_read_content() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "hello");
        assert_eq!(ed.content(), "hello");
        assert_eq!(ed.cursor(), 5);
    }

    #[test]
    fn test_should_move_to_beginning_with_ctrl_a() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "hello");
        ed.handle_key(&ctrl('a'));
        assert_eq!(ed.cursor(), 0);
    }

    #[test]
    fn test_should_move_to_end_with_ctrl_e() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "hello");
        ed.handle_key(&ctrl('a'));
        ed.handle_key(&ctrl('e'));
        assert_eq!(ed.cursor(), 5);
    }

    #[test]
    fn test_should_delete_word_back_with_ctrl_w() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "hello world");
        ed.handle_key(&ctrl('w'));
        assert_eq!(ed.content(), "hello ");
        assert_eq!(ed.cursor(), 6);
    }

    #[test]
    fn test_should_delete_to_start_with_ctrl_u() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "hello world");
        // Move cursor to position 5 (before space)
        ed.handle_key(&ctrl('a'));
        for _ in 0..5 {
            ed.handle_key(&press(KeyCode::Right));
        }
        ed.handle_key(&ctrl('u'));
        assert_eq!(ed.content(), " world");
        assert_eq!(ed.cursor(), 0);
    }

    #[test]
    fn test_should_delete_to_end_with_ctrl_k() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "hello world");
        ed.handle_key(&ctrl('a'));
        for _ in 0..5 {
            ed.handle_key(&press(KeyCode::Right));
        }
        ed.handle_key(&ctrl('k'));
        assert_eq!(ed.content(), "hello");
        assert_eq!(ed.cursor(), 5);
    }

    #[test]
    fn test_should_move_word_back_with_alt_b() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "one two three");
        ed.handle_key(&alt('b'));
        assert_eq!(ed.cursor(), 8); // before "three"
    }

    #[test]
    fn test_should_move_word_forward_with_alt_f() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "one two three");
        ed.handle_key(&ctrl('a'));
        ed.handle_key(&alt('f'));
        assert_eq!(ed.cursor(), 4); // after "one "
    }

    #[test]
    fn test_should_delete_word_forward_with_alt_d() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "hello world");
        ed.handle_key(&ctrl('a'));
        ed.handle_key(&alt('d'));
        assert_eq!(ed.content(), " world");
        assert_eq!(ed.cursor(), 0);
    }

    #[test]
    fn test_should_delete_forward_with_ctrl_d() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "abc");
        ed.handle_key(&ctrl('a'));
        ed.handle_key(&ctrl('d'));
        assert_eq!(ed.content(), "bc");
        assert_eq!(ed.cursor(), 0);
    }

    #[test]
    fn test_should_clear_and_take_trimmed() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "  hello  ");
        let s = ed.take_trimmed();
        assert_eq!(s, "hello");
        assert_eq!(ed.content(), "");
        assert_eq!(ed.cursor(), 0);
    }

    #[test]
    fn test_should_not_consume_enter() {
        let mut ed = LineEditor::new();
        let consumed = ed.handle_key(&press(KeyCode::Enter));
        assert!(!consumed);
    }

    #[test]
    fn test_should_handle_backspace_at_start() {
        let mut ed = LineEditor::new();
        ed.handle_key(&press(KeyCode::Backspace));
        assert_eq!(ed.content(), "");
        assert_eq!(ed.cursor(), 0);
    }

    #[test]
    fn test_should_insert_at_cursor_position() {
        let mut ed = LineEditor::new();
        type_str(&mut ed, "hllo");
        // Move cursor back 3 positions to after 'h'
        ed.handle_key(&press(KeyCode::Left));
        ed.handle_key(&press(KeyCode::Left));
        ed.handle_key(&press(KeyCode::Left));
        ed.handle_key(&press(KeyCode::Char('e')));
        assert_eq!(ed.content(), "hello");
        assert_eq!(ed.cursor(), 2);
    }
}
