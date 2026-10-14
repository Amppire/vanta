use std::ops::Range;
use std::path::{Path, PathBuf};

use unicode_segmentation::UnicodeSegmentation;
use vanta_document::{ByteOffset, Document, EditOrigin, TextEdit, TextPoint, Transaction};
use vanta_file_io::{FileIoError, FileState, TextFormat, load_text, save_text_atomic};

/// Platform-independent state for the first desktop editor window.
pub struct EditorModel {
    document: Document,
    selection_anchor: ByteOffset,
    cursor: ByteOffset,
    path: Option<PathBuf>,
    format: TextFormat,
    disk_state: Option<FileState>,
    saved_version: vanta_document::DocumentVersion,
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
            path: None,
            format: TextFormat::default(),
            disk_state: None,
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
            selection_anchor: ByteOffset(0),
            saved_version: document.version(),
            document,
            path: Some(path.to_path_buf()),
            format: loaded.format,
            disk_state: Some(loaded.state),
        })
    }

    #[must_use]
    pub fn text(&self) -> &str {
        self.document.text()
    }

    #[must_use]
    #[cfg(test)]
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

    /// Inserts text at the cursor as one transaction.
    ///
    /// # Errors
    ///
    /// Returns an error if the document rejects the transaction.
    pub fn insert(&mut self, text: &str) -> Result<(), vanta_document::DocumentError> {
        if text.is_empty() {
            return Ok(());
        }
        let range = self.selection_range().unwrap_or(self.cursor..self.cursor);
        let start = range.start;
        let transaction = Transaction::new(self.document.version(), EditOrigin::User)
            .with_edit(TextEdit::replace(range, text));
        self.document.apply(transaction)?;
        self.cursor = ByteOffset(start.0 + text.len());
        self.selection_anchor = self.cursor;
        Ok(())
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
    }

    pub fn select_all(&mut self) {
        self.selection_anchor = ByteOffset(0);
        self.cursor = ByteOffset(self.text().len());
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
        self.document.apply(transaction)?;
        self.cursor = start;
        self.selection_anchor = start;
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
}
