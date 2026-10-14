use std::path::{Path, PathBuf};

use vanta_document::{ByteOffset, Document, EditOrigin, TextEdit, Transaction};
use vanta_file_io::{FileIoError, FileState, TextFormat, load_text, save_text_atomic};

/// Platform-independent state for the first desktop editor window.
pub struct EditorModel {
    document: Document,
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
        let length = text.len();
        let transaction = Transaction::new(self.document.version(), EditOrigin::User)
            .with_edit(TextEdit::insert(self.cursor, text));
        self.document.apply(transaction)?;
        self.cursor.0 += length;
        Ok(())
    }

    /// Deletes the Unicode scalar immediately before the cursor.
    ///
    /// Grapheme-aware deletion will replace this scalar-boundary behavior after
    /// v0.2 selects its Unicode segmentation policy.
    ///
    /// # Errors
    ///
    /// Returns an error if the document rejects the transaction.
    pub fn backspace(&mut self) -> Result<(), vanta_document::DocumentError> {
        if self.cursor.0 == 0 {
            return Ok(());
        }
        let start = self.text()[..self.cursor.0]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
        let transaction = Transaction::new(self.document.version(), EditOrigin::User)
            .with_edit(TextEdit::delete(ByteOffset(start)..self.cursor));
        self.document.apply(transaction)?;
        self.cursor = ByteOffset(start);
        Ok(())
    }

    pub fn move_left(&mut self) {
        if self.cursor.0 > 0 {
            self.cursor.0 = self.text()[..self.cursor.0]
                .char_indices()
                .next_back()
                .map_or(0, |(index, _)| index);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor.0 < self.text().len() {
            let character_length = self.text()[self.cursor.0..]
                .chars()
                .next()
                .map_or(0, char::len_utf8);
            self.cursor.0 += character_length;
        }
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

        editor.move_left();
        assert_eq!(editor.cursor(), ByteOffset(5));
        editor.backspace().unwrap();

        assert_eq!(editor.text(), "ab");
        assert_eq!(editor.cursor(), ByteOffset(1));
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
