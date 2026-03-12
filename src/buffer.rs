use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::PathBuf;

use regex::{Regex, RegexBuilder};
use ropey::Rope;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchRange {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone)]
struct BufferSnapshot {
    text: String,
    cursor: Position,
    selection_anchor: Option<Position>,
    dirty: bool,
}

#[derive(Debug, Clone)]
pub struct TextBuffer {
    pub path: Option<PathBuf>,
    rope: Rope,
    pub cursor: Position,
    pub selection_anchor: Option<Position>,
    pub scroll_x: usize,
    pub scroll_y: usize,
    pub dirty: bool,
    pub last_error: Option<String>,
    undo_stack: Vec<BufferSnapshot>,
    redo_stack: Vec<BufferSnapshot>,
    pub line_ending: LineEnding,
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self {
            path: None,
            rope: Rope::new(),
            cursor: Position { line: 0, column: 0 },
            selection_anchor: None,
            scroll_x: 0,
            scroll_y: 0,
            dirty: false,
            last_error: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            line_ending: default_line_ending(),
        }
    }
}

impl TextBuffer {
    pub fn from_file(path: PathBuf) -> io::Result<Self> {
        let bytes = fs::read(&path)?;
        let contents = String::from_utf8(bytes).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Unsupported or binary file; only UTF-8 text files can be opened",
            )
        })?;
        let line_ending = if contents.contains("\r\n") {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        };

        Ok(Self {
            path: Some(path),
            rope: Rope::from_str(&contents.replace("\r\n", "\n")),
            line_ending,
            ..Self::default()
        })
    }

    pub fn with_text(path: Option<PathBuf>, text: &str) -> Self {
        Self {
            path,
            rope: Rope::from_str(text),
            ..Self::default()
        }
    }

    pub fn display_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".to_string())
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines().max(1)
    }

    pub fn line_len(&self, line: usize) -> usize {
        if line >= self.line_count() {
            return 0;
        }

        let slice = self.rope.line(line);
        let mut len = slice.len_chars();
        if len > 0 && slice.char(len - 1) == '\n' {
            len -= 1;
        }
        len
    }

    pub fn line_text(&self, line: usize) -> String {
        if line >= self.line_count() {
            return String::new();
        }

        let mut text = self.rope.line(line).to_string();
        if text.ends_with('\n') {
            text.pop();
        }
        text
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn selection_bounds(&self) -> Option<(Position, Position)> {
        self.selection_anchor
            .filter(|anchor| *anchor != self.cursor)
            .map(|anchor| sort_positions(anchor, self.cursor))
    }

    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection_char_range()?;
        Some(self.rope.slice(start..end).to_string())
    }

    pub fn clear_selection(&mut self) {
        self.selection_anchor = None;
    }

    pub fn set_cursor(&mut self, position: Position, extend_selection: bool) {
        let position = self.clamp_position(position);
        if extend_selection {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor);
            }
        } else {
            self.selection_anchor = None;
        }
        self.cursor = position;
    }

    pub fn move_left(&mut self, extend_selection: bool) {
        let target = if self.cursor.column > 0 {
            Position {
                line: self.cursor.line,
                column: self.cursor.column - 1,
            }
        } else if self.cursor.line > 0 {
            Position {
                line: self.cursor.line - 1,
                column: self.line_len(self.cursor.line - 1),
            }
        } else {
            self.cursor
        };
        self.set_cursor(target, extend_selection);
    }

    pub fn move_right(&mut self, extend_selection: bool) {
        let line_len = self.line_len(self.cursor.line);
        let target = if self.cursor.column < line_len {
            Position {
                line: self.cursor.line,
                column: self.cursor.column + 1,
            }
        } else if self.cursor.line + 1 < self.line_count() {
            Position {
                line: self.cursor.line + 1,
                column: 0,
            }
        } else {
            self.cursor
        };
        self.set_cursor(target, extend_selection);
    }

    pub fn move_up(&mut self, extend_selection: bool) {
        let line = self.cursor.line.saturating_sub(1);
        self.set_cursor(
            Position {
                line,
                column: self.cursor.column.min(self.line_len(line)),
            },
            extend_selection,
        );
    }

    pub fn move_down(&mut self, extend_selection: bool) {
        let line = (self.cursor.line + 1).min(self.line_count().saturating_sub(1));
        self.set_cursor(
            Position {
                line,
                column: self.cursor.column.min(self.line_len(line)),
            },
            extend_selection,
        );
    }

    pub fn move_home(&mut self, extend_selection: bool) {
        self.set_cursor(
            Position {
                line: self.cursor.line,
                column: 0,
            },
            extend_selection,
        );
    }

    pub fn move_end(&mut self, extend_selection: bool) {
        self.set_cursor(
            Position {
                line: self.cursor.line,
                column: self.line_len(self.cursor.line),
            },
            extend_selection,
        );
    }

    pub fn move_page_up(&mut self, rows: usize, extend_selection: bool) {
        let line = self.cursor.line.saturating_sub(rows);
        self.set_cursor(
            Position {
                line,
                column: self.cursor.column.min(self.line_len(line)),
            },
            extend_selection,
        );
    }

    pub fn move_page_down(&mut self, rows: usize, extend_selection: bool) {
        let line = (self.cursor.line + rows).min(self.line_count().saturating_sub(1));
        self.set_cursor(
            Position {
                line,
                column: self.cursor.column.min(self.line_len(line)),
            },
            extend_selection,
        );
    }

    pub fn move_word_left(&mut self, extend_selection: bool) {
        let content = self.text();
        let chars = content.chars().collect::<Vec<_>>();
        let mut index = self.cursor_char_index();
        if index == 0 {
            self.set_cursor(self.cursor, extend_selection);
            return;
        }

        index -= 1;
        while index > 0 && chars.get(index).is_some_and(|ch| ch.is_whitespace()) {
            index -= 1;
        }
        while index > 0 && chars.get(index - 1).is_some_and(is_word_char) {
            index -= 1;
        }
        self.set_cursor(self.position_for_char(index), extend_selection);
    }

    pub fn move_word_right(&mut self, extend_selection: bool) {
        let content = self.text();
        let chars = content.chars().collect::<Vec<_>>();
        let mut index = self.cursor_char_index();
        while index < chars.len() && chars.get(index).is_some_and(is_word_char) {
            index += 1;
        }
        while index < chars.len() && chars.get(index).is_some_and(|ch| ch.is_whitespace()) {
            index += 1;
        }
        self.set_cursor(self.position_for_char(index), extend_selection);
    }

    pub fn insert_char(&mut self, ch: char) {
        self.push_undo_snapshot();
        self.remove_selection_without_snapshot();
        let index = self.cursor_char_index();
        self.rope.insert_char(index, ch);
        self.cursor = self.position_for_char(index + 1);
        self.mark_dirty();
    }

    pub fn insert_str(&mut self, text: &str) {
        self.push_undo_snapshot();
        self.remove_selection_without_snapshot();
        let index = self.cursor_char_index();
        self.rope.insert(index, text);
        self.cursor = self.position_for_char(index + text.chars().count());
        self.mark_dirty();
    }

    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub fn insert_indent(&mut self, tab_width: usize) {
        self.insert_str(&" ".repeat(tab_width));
    }

    pub fn backspace(&mut self) {
        if self.delete_selection_if_present() {
            return;
        }

        let index = self.cursor_char_index();
        if index == 0 {
            return;
        }

        self.push_undo_snapshot();
        self.rope.remove(index - 1..index);
        self.cursor = self.position_for_char(index - 1);
        self.mark_dirty();
    }

    pub fn delete_forward(&mut self) {
        if self.delete_selection_if_present() {
            return;
        }

        let index = self.cursor_char_index();
        if index >= self.rope.len_chars() {
            return;
        }

        self.push_undo_snapshot();
        self.rope.remove(index..index + 1);
        self.mark_dirty();
    }

    pub fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo_stack.pop() else {
            return false;
        };

        self.redo_stack.push(self.snapshot());
        self.restore(snapshot);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo_stack.pop() else {
            return false;
        };

        self.undo_stack.push(self.snapshot());
        self.restore(snapshot);
        true
    }

    pub fn save(&mut self) -> io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Buffer has no file path",
            ));
        };

        self.save_as(path)
    }

    pub fn save_as(&mut self, path: PathBuf) -> io::Result<()> {
        let text = self.persisted_text();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, text)?;
        self.path = Some(path);
        self.dirty = false;
        self.last_error = None;
        Ok(())
    }

    pub fn reload_from_disk(&mut self) -> io::Result<()> {
        let Some(path) = self.path.clone() else {
            *self = Self::default();
            return Ok(());
        };

        *self = Self::from_file(path)?;
        Ok(())
    }

    pub fn goto_line(&mut self, line: usize) {
        let zero_based = line
            .saturating_sub(1)
            .min(self.line_count().saturating_sub(1));
        let column = self.cursor.column.min(self.line_len(zero_based));
        self.set_cursor(
            Position {
                line: zero_based,
                column,
            },
            false,
        );
    }

    pub fn find_all(&self, query: &str, case_sensitive: bool) -> Vec<MatchRange> {
        if query.is_empty() {
            return Vec::new();
        }

        let haystack = self.text();
        if case_sensitive {
            haystack
                .match_indices(query)
                .map(|(start, matched)| {
                    let end = start + matched.len();
                    MatchRange {
                        start: self.position_for_char(byte_to_char_index(&haystack, start)),
                        end: self.position_for_char(byte_to_char_index(&haystack, end)),
                    }
                })
                .collect()
        } else {
            let Some(regex) = compile_literal_regex(query, false) else {
                return Vec::new();
            };
            regex
                .find_iter(&haystack)
                .map(|matched| MatchRange {
                    start: self.position_for_char(byte_to_char_index(&haystack, matched.start())),
                    end: self.position_for_char(byte_to_char_index(&haystack, matched.end())),
                })
                .collect()
        }
    }

    pub fn search_next(&mut self, query: &str, case_sensitive: bool) -> Option<MatchRange> {
        let matches = self.find_all(query, case_sensitive);
        if matches.is_empty() {
            return None;
        }

        let current = self.cursor_char_index();
        let next = matches
            .iter()
            .find(|match_range| self.char_index(match_range.start) > current)
            .cloned()
            .unwrap_or_else(|| matches[0].clone());
        self.select_match(&next);
        Some(next)
    }

    pub fn search_previous(&mut self, query: &str, case_sensitive: bool) -> Option<MatchRange> {
        let matches = self.find_all(query, case_sensitive);
        if matches.is_empty() {
            return None;
        }

        let current = self.cursor_char_index();
        let previous = matches
            .iter()
            .rev()
            .find(|match_range| self.char_index(match_range.start) < current)
            .cloned()
            .unwrap_or_else(|| matches.last().cloned().unwrap());
        self.select_match(&previous);
        Some(previous)
    }

    pub fn replace_current(
        &mut self,
        query: &str,
        replacement: &str,
        case_sensitive: bool,
    ) -> bool {
        let Some((start, end)) = self.selection_char_range() else {
            return false;
        };
        let selected_text = self.rope.slice(start..end).to_string();
        if !matches_query(&selected_text, query, case_sensitive) {
            return false;
        }

        self.push_undo_snapshot();
        self.rope.remove(start..end);
        self.rope.insert(start, replacement);
        self.cursor = self.position_for_char(start + replacement.chars().count());
        self.selection_anchor = None;
        self.mark_dirty();
        true
    }

    pub fn replace_all(&mut self, query: &str, replacement: &str, case_sensitive: bool) -> usize {
        let matches = self.find_all(query, case_sensitive);
        if matches.is_empty() {
            return 0;
        }

        let text = self.text();
        let replaced = if case_sensitive {
            text.replace(query, replacement)
        } else {
            let Some(regex) = compile_literal_regex(query, false) else {
                return 0;
            };
            regex.replace_all(&text, replacement).into_owned()
        };

        self.push_undo_snapshot();
        self.rope = Rope::from_str(&replaced);
        self.cursor = Position { line: 0, column: 0 };
        self.selection_anchor = None;
        self.mark_dirty();
        matches.len()
    }

    pub fn ensure_cursor_visible(&mut self, height: usize, width: usize, gutter_width: usize) {
        if self.cursor.line < self.scroll_y {
            self.scroll_y = self.cursor.line;
        } else if height > 0 && self.cursor.line >= self.scroll_y + height {
            self.scroll_y = self.cursor.line + 1 - height;
        }

        let visible_width = width.saturating_sub(gutter_width).max(1);
        if self.cursor.column < self.scroll_x {
            self.scroll_x = self.cursor.column;
        } else if self.cursor.column >= self.scroll_x + visible_width {
            self.scroll_x = self.cursor.column + 1 - visible_width;
        }
    }

    pub fn scroll_lines(&mut self, delta: i32) {
        if delta.is_negative() {
            self.scroll_y = self.scroll_y.saturating_sub(delta.unsigned_abs() as usize);
        } else {
            self.scroll_y =
                (self.scroll_y + delta as usize).min(self.line_count().saturating_sub(1));
        }
    }

    pub fn set_cursor_from_screen(&mut self, line: usize, column: usize) {
        self.set_cursor(
            Position {
                line: line.min(self.line_count().saturating_sub(1)),
                column,
            },
            false,
        );
    }

    pub fn char_index(&self, position: Position) -> usize {
        let position = self.clamp_position(position);
        self.rope.line_to_char(position.line) + position.column
    }

    pub fn position_for_char(&self, index: usize) -> Position {
        let index = index.min(self.rope.len_chars());
        let line = self.rope.char_to_line(index);
        let line_start = self.rope.line_to_char(line);
        Position {
            line,
            column: index.saturating_sub(line_start),
        }
    }

    fn persisted_text(&self) -> String {
        match self.line_ending {
            LineEnding::Lf => self.text(),
            LineEnding::CrLf => self.text().replace('\n', "\r\n"),
        }
    }

    fn snapshot(&self) -> BufferSnapshot {
        BufferSnapshot {
            text: self.text(),
            cursor: self.cursor,
            selection_anchor: self.selection_anchor,
            dirty: self.dirty,
        }
    }

    fn restore(&mut self, snapshot: BufferSnapshot) {
        self.rope = Rope::from_str(&snapshot.text);
        self.cursor = snapshot.cursor;
        self.selection_anchor = snapshot.selection_anchor;
        self.dirty = snapshot.dirty;
    }

    fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(self.snapshot());
        self.redo_stack.clear();
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.last_error = None;
    }

    fn delete_selection_if_present(&mut self) -> bool {
        let Some((start, end)) = self.selection_char_range() else {
            return false;
        };

        self.push_undo_snapshot();
        self.rope.remove(start..end);
        self.cursor = self.position_for_char(start);
        self.selection_anchor = None;
        self.mark_dirty();
        true
    }

    fn remove_selection_without_snapshot(&mut self) {
        if let Some((start, end)) = self.selection_char_range() {
            self.rope.remove(start..end);
            self.cursor = self.position_for_char(start);
            self.selection_anchor = None;
        }
    }

    fn selection_char_range(&self) -> Option<(usize, usize)> {
        let (start, end) = self.selection_bounds()?;
        Some((self.char_index(start), self.char_index(end)))
    }

    fn cursor_char_index(&self) -> usize {
        self.char_index(self.cursor)
    }

    fn clamp_position(&self, position: Position) -> Position {
        let line = position.line.min(self.line_count().saturating_sub(1));
        Position {
            line,
            column: position.column.min(self.line_len(line)),
        }
    }

    fn select_match(&mut self, match_range: &MatchRange) {
        self.selection_anchor = Some(match_range.start);
        self.cursor = match_range.end;
    }
}

fn sort_positions(left: Position, right: Position) -> (Position, Position) {
    match left.cmp(&right) {
        Ordering::Greater => (right, left),
        _ => (left, right),
    }
}

fn is_word_char(ch: &char) -> bool {
    ch.is_ascii_alphanumeric() || *ch == '_'
}

fn matches_query(text: &str, query: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        text == query
    } else {
        text.to_lowercase() == query.to_lowercase()
    }
}

fn compile_literal_regex(query: &str, case_sensitive: bool) -> Option<Regex> {
    RegexBuilder::new(&regex::escape(query))
        .case_insensitive(!case_sensitive)
        .unicode(true)
        .build()
        .ok()
}

fn byte_to_char_index(text: &str, byte_index: usize) -> usize {
    text[..byte_index].chars().count()
}

fn default_line_ending() -> LineEnding {
    if cfg!(windows) {
        LineEnding::CrLf
    } else {
        LineEnding::Lf
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn insert_undo_redo_round_trip() {
        let mut buffer = TextBuffer::default();
        buffer.insert_str("hello");
        assert_eq!(buffer.text(), "hello");

        assert!(buffer.undo());
        assert_eq!(buffer.text(), "");

        assert!(buffer.redo());
        assert_eq!(buffer.text(), "hello");
    }

    #[test]
    fn replace_all_updates_content() {
        let mut buffer = TextBuffer::with_text(None, "abc abc");
        let count = buffer.replace_all("abc", "xyz", true);
        assert_eq!(count, 2);
        assert_eq!(buffer.text(), "xyz xyz");
    }

    #[test]
    fn search_returns_match_ranges() {
        let buffer = TextBuffer::with_text(None, "alpha beta alpha");
        let matches = buffer.find_all("alpha", true);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].start, Position { line: 0, column: 0 });
        assert_eq!(
            matches[1].start,
            Position {
                line: 0,
                column: 11
            }
        );
    }

    #[test]
    fn unicode_search_and_replace_are_safe() {
        let mut buffer = TextBuffer::with_text(None, "Straße\ncafé\nStraße");
        let matches = buffer.find_all("straße", false);
        assert_eq!(matches.len(), 2);

        let replaced = buffer.replace_all("CAFÉ", "coffee", false);
        assert_eq!(replaced, 1);
        assert!(buffer.text().contains("coffee"));
    }

    #[test]
    fn undo_restores_clean_state() {
        let mut buffer = TextBuffer::with_text(None, "hello");
        buffer.insert_char('!');
        assert!(buffer.dirty);
        assert!(buffer.undo());
        assert!(!buffer.dirty);
    }

    #[test]
    fn rejects_binary_files_cleanly() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("binary.bin");
        fs::write(&path, [0xFF, 0xFE, 0x00, 0x01]).unwrap();

        let error = TextBuffer::from_file(path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
