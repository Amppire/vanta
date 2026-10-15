use std::ops::Range;
use std::path::{Path, PathBuf};

use unicode_segmentation::UnicodeSegmentation;
use vanta_document::{ByteOffset, Document, EditOrigin, TextEdit, TextPoint, Transaction};
use vanta_file_io::{
    FileIoError, FileState, RecoveryRecord, TextFormat, load_text, save_text_atomic,
};

/// Platform-independent state for the first desktop editor window.
pub struct EditorModel {
    document: Document,
    selection_anchor: ByteOffset,
    cursor: ByteOffset,
    extra_cursors: Vec<ByteOffset>,
    path: Option<PathBuf>,
    format: TextFormat,
    disk_state: Option<FileState>,
    saved_version: vanta_document::DocumentVersion,
    preferred_column: Option<usize>,
    selection_undo: Vec<SelectionHistory>,
    selection_redo: Vec<SelectionHistory>,
}

#[derive(Clone)]
struct SelectionHistory {
    before: (ByteOffset, ByteOffset),
    after: (ByteOffset, ByteOffset),
    extra_before: Vec<ByteOffset>,
    extra_after: Vec<ByteOffset>,
}

impl EditorModel {
    #[must_use]
    pub fn untitled() -> Self {
        let document = Document::new(String::new());
        Self {
            saved_version: document.version(),
            document,
            selection_anchor: ByteOffset(0),
            cursor: ByteOffset(0),
            extra_cursors: Vec::new(),
            path: None,
            format: TextFormat::default(),
            disk_state: None,
            preferred_column: None,
            selection_undo: Vec::new(),
            selection_redo: Vec::new(),
        }
    }

    /// Loads a supported text file.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read as supported text.
    pub fn open(path: &Path) -> Result<Self, FileIoError> {
        let loaded = load_text(path)?;
        let document = Document::new(loaded.text);
        Ok(Self {
            cursor: ByteOffset(0),
            extra_cursors: Vec::new(),
            selection_anchor: ByteOffset(0),
            saved_version: document.version(),
            document,
            path: Some(path.to_path_buf()),
            format: loaded.format,
            disk_state: Some(loaded.state),
            preferred_column: None,
            selection_undo: Vec::new(),
            selection_redo: Vec::new(),
        })
    }

    #[must_use]
    pub fn from_recovery(record: RecoveryRecord) -> Self {
        let document = Document::new(record.text);
        Self {
            cursor: ByteOffset(0),
            extra_cursors: Vec::new(),
            selection_anchor: ByteOffset(0),
            saved_version: vanta_document::DocumentVersion(u64::MAX),
            document,
            path: record.source_path.map(PathBuf::from),
            format: TextFormat {
                encoding: record.encoding,
                ..TextFormat::default()
            },
            disk_state: None,
            preferred_column: None,
            selection_undo: Vec::new(),
            selection_redo: Vec::new(),
        }
    }

    #[must_use]
    pub fn recovery_id(&self) -> String {
        self.path.as_ref().map_or_else(
            || "untitled-primary".to_owned(),
            |path| path.to_string_lossy().into_owned(),
        )
    }

    #[must_use]
    pub fn recovery_record(&self) -> RecoveryRecord {
        RecoveryRecord {
            document_id: self.recovery_id(),
            source_path: self
                .path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            version: self.document.version().0,
            encoding: self.format.encoding,
            text: self.text().to_owned(),
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        self.document.text()
    }

    #[must_use]
    pub const fn version(&self) -> vanta_document::DocumentVersion {
        self.document.version()
    }

    pub fn apply_external_transaction(
        &mut self,
        transaction: Transaction,
    ) -> Result<(), vanta_document::DocumentError> {
        self.document.apply(transaction)?;
        let mut cursor = self.cursor.0.min(self.document.byte_len());
        while !self.document.text().is_char_boundary(cursor) {
            cursor = cursor.saturating_sub(1);
        }
        self.cursor = ByteOffset(cursor);
        self.selection_anchor = self.cursor;
        self.extra_cursors.clear();
        self.preferred_column = None;
        Ok(())
    }

    #[must_use]
    pub const fn cursor(&self) -> ByteOffset {
        self.cursor
    }

    #[must_use]
    pub fn cursor_point(&self) -> TextPoint {
        self.document
            .offset_to_point(self.cursor)
            .expect("the editor cursor must remain on a valid document boundary")
    }

    #[must_use]
    pub fn cursor_points(&self) -> Vec<TextPoint> {
        std::iter::once(self.cursor)
            .chain(self.extra_cursors.iter().copied())
            .filter_map(|offset| self.document.offset_to_point(offset).ok())
            .collect()
    }

    #[must_use]
    pub fn selection_points(&self) -> Option<(TextPoint, TextPoint)> {
        let range = self.selection_range()?;
        Some((
            self.document
                .offset_to_point(range.start)
                .expect("selection start must remain valid"),
            self.document
                .offset_to_point(range.end)
                .expect("selection end must remain valid"),
        ))
    }

    #[must_use]
    pub fn selection_range(&self) -> Option<Range<ByteOffset>> {
        if self.selection_anchor == self.cursor {
            return None;
        }
        Some(if self.selection_anchor < self.cursor {
            self.selection_anchor..self.cursor
        } else {
            self.cursor..self.selection_anchor
        })
    }

    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.selection_anchor != self.cursor
    }

    #[must_use]
    pub fn selected_text(&self) -> Option<&str> {
        let range = self.selection_range()?;
        Some(&self.text()[range.start.0..range.end.0])
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.document.version() != self.saved_version
    }

    #[must_use]
    pub fn title(&self) -> String {
        let name = self.path.as_deref().and_then(Path::file_name).map_or_else(
            || "Untitled".into(),
            |name| name.to_string_lossy().into_owned(),
        );
        if self.is_dirty() {
            format!("{name} — Vanta •")
        } else {
            format!("{name} — Vanta")
        }
    }

    #[must_use]
    pub fn display_name(&self) -> String {
        let name = self.path.as_deref().and_then(Path::file_name).map_or_else(
            || "Untitled".into(),
            |name| name.to_string_lossy().into_owned(),
        );
        if self.is_dirty() {
            format!("{name} •")
        } else {
            name
        }
    }

    /// Inserts text at the cursor as one transaction.
    ///
    /// # Errors
    ///
    /// Returns an error if the document rejects the transaction.
    pub fn insert(&mut self, text: &str) -> Result<(), vanta_document::DocumentError> {
        if text.is_empty() {
            return Ok(());
        }
        if !self.extra_cursors.is_empty() && !self.has_selection() {
            return self.insert_at_cursors(text);
        }
        let range = self.selection_range().unwrap_or(self.cursor..self.cursor);
        let start = range.start;
        let transaction = Transaction::new(self.document.version(), EditOrigin::User)
            .with_edit(TextEdit::replace(range, text));
        let after = ByteOffset(start.0 + text.len());
        self.apply_edit(transaction, after)
    }

    fn insert_at_cursors(&mut self, text: &str) -> Result<(), vanta_document::DocumentError> {
        let mut positions = vec![self.cursor];
        positions.extend(self.extra_cursors.iter().copied());
        positions.sort_unstable();
        positions.dedup();
        let mut transaction = Transaction::new(self.document.version(), EditOrigin::User);
        for position in &positions {
            transaction = transaction.with_edit(TextEdit::insert(*position, text));
        }
        let map = |position: ByteOffset| {
            ByteOffset(
                position.0
                    + text.len()
                        * positions
                            .iter()
                            .filter(|candidate| **candidate <= position)
                            .count(),
            )
        };
        let cursor_after = map(self.cursor);
        let extras_after = self.extra_cursors.iter().copied().map(map).collect();
        self.apply_edit_with_cursors(transaction, cursor_after, extras_after)
    }

    /// Deletes the extended grapheme immediately before the cursor.
    ///
    /// # Errors
    ///
    /// Returns an error if the document rejects the transaction.
    pub fn backspace(&mut self) -> Result<(), vanta_document::DocumentError> {
        if let Some(range) = self.selection_range() {
            return self.delete_range(range);
        }
        if self.cursor.0 == 0 {
            return Ok(());
        }
        let start = self.text()[..self.cursor.0]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index);
        self.delete_range(ByteOffset(start)..self.cursor)
    }

    /// Deletes the selection or the extended grapheme after the cursor.
    ///
    /// # Errors
    ///
    /// Returns an error if the document rejects the transaction.
    pub fn delete_forward(&mut self) -> Result<(), vanta_document::DocumentError> {
        if let Some(range) = self.selection_range() {
            return self.delete_range(range);
        }
        let Some(grapheme) = self.text()[self.cursor.0..].graphemes(true).next() else {
            return Ok(());
        };
        self.delete_range(self.cursor..ByteOffset(self.cursor.0 + grapheme.len()))
    }

    /// Deletes the active selection and does nothing when there is no selection.
    ///
    /// # Errors
    ///
    /// Returns an error if the document rejects the transaction.
    pub fn delete_selection(&mut self) -> Result<(), vanta_document::DocumentError> {
        if let Some(range) = self.selection_range() {
            self.delete_range(range)?;
        }
        Ok(())
    }

    pub fn move_left(&mut self, extend: bool) {
        if !extend && let Some(range) = self.selection_range() {
            self.set_cursor(range.start, false);
            return;
        }
        let next = self.text()[..self.cursor.0]
            .grapheme_indices(true)
            .next_back()
            .map_or(ByteOffset(0), |(index, _)| ByteOffset(index));
        self.set_cursor(next, extend);
        self.preferred_column = None;
    }

    pub fn move_right(&mut self, extend: bool) {
        if !extend && let Some(range) = self.selection_range() {
            self.set_cursor(range.end, false);
            return;
        }
        let next = self.text()[self.cursor.0..]
            .graphemes(true)
            .next()
            .map_or(self.cursor, |grapheme| {
                ByteOffset(self.cursor.0 + grapheme.len())
            });
        self.set_cursor(next, extend);
        self.preferred_column = None;
    }

    pub fn move_vertical(&mut self, line_delta: isize, extend: bool) {
        let point = self.cursor_point();
        let preferred = self.preferred_column.unwrap_or(point.byte_column);
        let last_line = self.text().split('\n').count().saturating_sub(1);
        let target_line = point.line.saturating_add_signed(line_delta).min(last_line);
        let target_text = self.text().split('\n').nth(target_line).unwrap_or("");
        let mut column = preferred.min(target_text.len());
        while column > 0 && !target_text.is_char_boundary(column) {
            column -= 1;
        }
        if self
            .set_cursor_point(
                TextPoint {
                    line: target_line,
                    byte_column: column,
                },
                extend,
            )
            .is_ok()
        {
            self.preferred_column = Some(preferred);
        }
    }

    pub fn move_line_boundary(&mut self, end: bool, extend: bool) {
        let point = self.cursor_point();
        let column = if end {
            self.text().split('\n').nth(point.line).map_or(0, str::len)
        } else {
            0
        };
        let _ = self.set_cursor_point(
            TextPoint {
                line: point.line,
                byte_column: column,
            },
            extend,
        );
        self.preferred_column = None;
    }

    /// Undoes the last local edit and restores its selection.
    pub fn undo(&mut self) -> Result<bool, vanta_document::DocumentError> {
        let Some(history) = self.selection_undo.pop() else {
            return Ok(false);
        };
        if self.document.undo()?.is_none() {
            return Ok(false);
        }
        self.selection_anchor = history.before.0;
        self.cursor = history.before.1;
        self.extra_cursors.clone_from(&history.extra_before);
        self.selection_redo.push(history);
        self.preferred_column = None;
        Ok(true)
    }

    /// Redoes the last undone edit and restores its resulting selection.
    pub fn redo(&mut self) -> Result<bool, vanta_document::DocumentError> {
        let Some(history) = self.selection_redo.pop() else {
            return Ok(false);
        };
        if self.document.redo()?.is_none() {
            return Ok(false);
        }
        self.selection_anchor = history.after.0;
        self.cursor = history.after.1;
        self.extra_cursors.clone_from(&history.extra_after);
        self.selection_undo.push(history);
        self.preferred_column = None;
        Ok(true)
    }

    pub fn select_all(&mut self) {
        self.extra_cursors.clear();
        self.selection_anchor = ByteOffset(0);
        self.cursor = ByteOffset(self.text().len());
        self.preferred_column = None;
    }

    /// Moves the cursor to a logical text point, optionally extending selection.
    ///
    /// # Errors
    ///
    /// Returns an error when the point is outside the document or not a valid
    /// UTF-8 boundary.
    pub fn set_cursor_point(
        &mut self,
        point: TextPoint,
        extend: bool,
    ) -> Result<(), vanta_document::DocumentError> {
        let offset = self.document.point_to_offset(point)?;
        self.set_cursor(offset, extend);
        Ok(())
    }

    /// Adds a caret unless one already exists at the point.
    pub fn add_cursor_point(
        &mut self,
        point: TextPoint,
    ) -> Result<(), vanta_document::DocumentError> {
        let offset = self.document.point_to_offset(point)?;
        if offset != self.cursor && !self.extra_cursors.contains(&offset) {
            self.extra_cursors.push(offset);
        }
        self.selection_anchor = self.cursor;
        Ok(())
    }

    fn set_cursor(&mut self, offset: ByteOffset, extend: bool) {
        self.cursor = offset;
        if !extend {
            self.selection_anchor = offset;
        }
    }

    fn delete_range(
        &mut self,
        range: Range<ByteOffset>,
    ) -> Result<(), vanta_document::DocumentError> {
        let start = range.start;
        let transaction = Transaction::new(self.document.version(), EditOrigin::User)
            .with_edit(TextEdit::delete(range));
        self.apply_edit(transaction, start)
    }

    fn apply_edit(
        &mut self,
        transaction: Transaction,
        cursor_after: ByteOffset,
    ) -> Result<(), vanta_document::DocumentError> {
        self.apply_edit_with_cursors(transaction, cursor_after, Vec::new())
    }

    fn apply_edit_with_cursors(
        &mut self,
        transaction: Transaction,
        cursor_after: ByteOffset,
        extras_after: Vec<ByteOffset>,
    ) -> Result<(), vanta_document::DocumentError> {
        let before = (self.selection_anchor, self.cursor);
        let extra_before = self.extra_cursors.clone();
        self.document.apply(transaction)?;
        self.cursor = cursor_after;
        self.selection_anchor = cursor_after;
        self.extra_cursors.clone_from(&extras_after);
        self.selection_undo.push(SelectionHistory {
            before,
            after: (cursor_after, cursor_after),
            extra_before,
            extra_after: extras_after,
        });
        self.selection_redo.clear();
        self.preferred_column = None;
        Ok(())
    }

    /// Saves an existing file while refusing external-change overwrites.
    ///
    /// # Errors
    ///
    /// Returns an error for untitled documents, conflicts, or filesystem
    /// failures.
    pub fn save(&mut self) -> Result<(), EditorSaveError> {
        let path = self.path.as_deref().ok_or(EditorSaveError::Untitled)?;
        let state = save_text_atomic(path, self.text(), self.format, self.disk_state.as_ref())?;
        self.disk_state = Some(state);
        self.saved_version = self.document.version();
        Ok(())
    }

    /// Saves to a selected path and makes it the document's backing file.
    ///
    /// # Errors
    ///
    /// Returns an error when the destination cannot be written safely.
    pub fn save_as(&mut self, path: &Path) -> Result<(), EditorSaveError> {
        let state = save_text_atomic(path, self.text(), self.format, None)?;
        self.path = Some(path.to_path_buf());
        self.disk_state = Some(state);
        self.saved_version = self.document.version();
        Ok(())
    }

    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

#[derive(Debug)]
pub enum EditorSaveError {
    Untitled,
    File(FileIoError),
}

impl From<FileIoError> for EditorSaveError {
    fn from(error: FileIoError) -> Self {
        Self::File(error)
    }
}

impl std::fmt::Display for EditorSaveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Untitled => formatter.write_str("Save As is not implemented for untitled files"),
            Self::File(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for EditorSaveError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_insertion_movement_and_backspace_use_valid_boundaries() {
        let mut editor = EditorModel::untitled();
        editor.insert("a💡b").unwrap();
        assert_eq!(editor.cursor(), ByteOffset(6));

        editor.move_left(false);
        assert_eq!(editor.cursor(), ByteOffset(5));
        editor.backspace().unwrap();

        assert_eq!(editor.text(), "ab");
        assert_eq!(editor.cursor(), ByteOffset(1));
    }

    #[test]
    fn backspace_deletes_one_extended_grapheme() {
        let mut editor = EditorModel::untitled();
        editor.insert("e\u{301}👨‍👩‍👧‍👦").unwrap();

        editor.backspace().unwrap();
        assert_eq!(editor.text(), "e\u{301}");
        editor.backspace().unwrap();
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn typing_replaces_selection_atomically() {
        let mut editor = EditorModel::untitled();
        editor.insert("alpha beta").unwrap();
        editor.select_all();
        editor.insert("γ").unwrap();

        assert_eq!(editor.text(), "γ");
        assert!(editor.selection_range().is_none());
    }

    #[test]
    fn dirty_state_tracks_edits_and_untitled_save_is_explicit() {
        let mut editor = EditorModel::untitled();
        assert!(!editor.is_dirty());
        editor.insert("draft").unwrap();
        assert!(editor.is_dirty());
        assert!(matches!(editor.save(), Err(EditorSaveError::Untitled)));
    }

    #[test]
    fn vertical_movement_preserves_preferred_column() {
        let mut editor = EditorModel::untitled();
        editor.insert("abcdef\nx\nabcdef").unwrap();
        editor.move_line_boundary(false, false);
        editor.move_vertical(-2, false);
        editor.move_right(false);
        editor.move_right(false);
        editor.move_right(false);
        editor.move_right(false);
        editor.move_vertical(1, false);
        assert_eq!(
            editor.cursor_point(),
            TextPoint {
                line: 1,
                byte_column: 1
            }
        );
        editor.move_vertical(1, false);
        assert_eq!(
            editor.cursor_point(),
            TextPoint {
                line: 2,
                byte_column: 4
            }
        );
    }

    #[test]
    fn undo_and_redo_restore_text_and_selection() {
        let mut editor = EditorModel::untitled();
        editor.insert("hello").unwrap();
        editor.move_left(true);
        editor.move_left(true);
        editor.insert("!").unwrap();
        assert_eq!(editor.text(), "hel!");
        assert!(editor.undo().unwrap());
        assert_eq!(editor.text(), "hello");
        assert_eq!(editor.selection_range(), Some(ByteOffset(3)..ByteOffset(5)));
        assert!(editor.redo().unwrap());
        assert_eq!(editor.text(), "hel!");
        assert_eq!(editor.cursor(), ByteOffset(4));
    }

    #[test]
    fn multi_caret_insert_is_atomic_and_undo_restores_carets() {
        let mut editor = EditorModel::untitled();
        editor.insert("ab\ncd").unwrap();
        editor.move_line_boundary(false, false);
        editor
            .add_cursor_point(TextPoint {
                line: 0,
                byte_column: 1,
            })
            .unwrap();
        editor.insert("!").unwrap();
        assert_eq!(editor.text(), "a!b\n!cd");
        assert_eq!(editor.cursor_points().len(), 2);
        editor.undo().unwrap();
        assert_eq!(editor.text(), "ab\ncd");
        assert_eq!(editor.cursor_points().len(), 2);
    }

    #[test]
    fn randomized_multi_caret_insert_matches_string_model() {
        let mut seed = 0x5eed_u64;
        for _ in 0..200 {
            let mut editor = EditorModel::untitled();
            editor.insert("abcdefghij").unwrap();
            let mut positions = Vec::new();
            for _ in 0..4 {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                positions.push(usize::try_from(seed % 11).unwrap());
            }
            positions.sort_unstable();
            positions.dedup();
            let primary = positions[0];
            editor
                .set_cursor_point(
                    TextPoint {
                        line: 0,
                        byte_column: primary,
                    },
                    false,
                )
                .unwrap();
            for position in positions.iter().skip(1) {
                editor
                    .add_cursor_point(TextPoint {
                        line: 0,
                        byte_column: *position,
                    })
                    .unwrap();
            }
            let mut expected = "abcdefghij".to_owned();
            for position in positions.iter().rev() {
                expected.insert(*position, '!');
            }
            editor.insert("!").unwrap();
            assert_eq!(editor.text(), expected);
            editor.undo().unwrap();
            assert_eq!(editor.text(), "abcdefghij");
        }
    }
}
