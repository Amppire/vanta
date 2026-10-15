//! The versioned text model used by Vanta.
//!
//! This crate begins with a deliberately simple storage backend. Its public
//! semantics are designed so that a rope or piece tree can replace that backend
//! without changing how callers describe positions and transactions.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

/// A byte offset into the document's internal UTF-8 representation.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct ByteOffset(pub usize);

/// A logical line and UTF-8 byte column.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TextPoint {
    pub line: usize,
    pub byte_column: usize,
}

/// The version used to reject edits computed from stale snapshots.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct DocumentVersion(pub u64);

/// Determines which side of an insertion or replacement an anchor follows.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Affinity {
    /// Stay before inserted text.
    #[default]
    Left,
    /// Move after inserted text.
    Right,
}

/// A byte position with deterministic behavior across edits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Anchor {
    pub offset: ByteOffset,
    pub affinity: Affinity,
}

/// A directed selection. `anchor` is fixed while `head` is the active end.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Selection {
    pub anchor: Anchor,
    pub head: Anchor,
}

impl Selection {
    #[must_use]
    pub const fn caret(offset: ByteOffset) -> Self {
        let anchor = Anchor {
            offset,
            affinity: Affinity::Right,
        };
        Self {
            anchor,
            head: anchor,
        }
    }
}

/// Identifies the subsystem that requested an edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditOrigin {
    User,
    Command(String),
    Extension(String),
    Formatter(String),
}

/// One replacement expressed in coordinates from the transaction's base version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextEdit {
    pub range: Range<ByteOffset>,
    pub insert: String,
}

impl TextEdit {
    #[must_use]
    pub fn insert(at: ByteOffset, text: impl Into<String>) -> Self {
        Self {
            range: at..at,
            insert: text.into(),
        }
    }

    #[must_use]
    pub fn delete(range: Range<ByteOffset>) -> Self {
        Self {
            range,
            insert: String::new(),
        }
    }

    #[must_use]
    pub fn replace(range: Range<ByteOffset>, text: impl Into<String>) -> Self {
        Self {
            range,
            insert: text.into(),
        }
    }
}

/// A set of atomic edits against one exact document version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transaction {
    pub base_version: DocumentVersion,
    pub edits: Vec<TextEdit>,
    pub selections_before: Vec<Selection>,
    pub selections_after: Vec<Selection>,
    pub origin: EditOrigin,
}

impl Transaction {
    #[must_use]
    pub fn new(base_version: DocumentVersion, origin: EditOrigin) -> Self {
        Self {
            base_version,
            edits: Vec::new(),
            selections_before: Vec::new(),
            selections_after: Vec::new(),
            origin,
        }
    }

    #[must_use]
    pub fn with_edit(mut self, edit: TextEdit) -> Self {
        self.edits.push(edit);
        self
    }
}

/// An immutable view of a document at one version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    version: DocumentVersion,
    text: Arc<str>,
}

impl Snapshot {
    #[must_use]
    pub const fn version(&self) -> DocumentVersion {
        self.version
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HistoryEntry {
    before: Arc<str>,
    after: Arc<str>,
    selections_before: Vec<Selection>,
    selections_after: Vec<Selection>,
    origin: EditOrigin,
}

/// The authoritative, single-writer state of one text document.
#[derive(Clone, Debug)]
pub struct Document {
    text: Arc<str>,
    version: DocumentVersion,
    selections: Vec<Selection>,
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
}

impl Document {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: Arc::from(text.into()),
            version: DocumentVersion(0),
            selections: vec![Selection::caret(ByteOffset(0))],
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn version(&self) -> DocumentVersion {
        self.version
    }

    #[must_use]
    pub fn selections(&self) -> &[Selection] {
        &self.selections
    }

    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            version: self.version,
            text: Arc::clone(&self.text),
        }
    }

    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.text.len()
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.text.bytes().filter(|byte| *byte == b'\n').count() + 1
    }

    /// Converts a UTF-8 byte offset to a logical line and byte column.
    ///
    /// # Errors
    ///
    /// Returns an error when the offset is outside the document or is not a
    /// UTF-8 character boundary.
    pub fn offset_to_point(&self, offset: ByteOffset) -> Result<TextPoint, DocumentError> {
        validate_boundary(&self.text, offset, "position")?;
        let prefix = &self.text[..offset.0];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
        let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
        Ok(TextPoint {
            line,
            byte_column: offset.0 - line_start,
        })
    }

    /// Converts a logical line and byte column to a UTF-8 byte offset.
    ///
    /// # Errors
    ///
    /// Returns an error when the line or column is outside the document or the
    /// resulting offset is not a UTF-8 character boundary.
    pub fn point_to_offset(&self, point: TextPoint) -> Result<ByteOffset, DocumentError> {
        let mut line = 0;
        let mut line_start = 0;
        for (index, byte) in self.text.bytes().enumerate() {
            if line == point.line {
                break;
            }
            if byte == b'\n' {
                line += 1;
                line_start = index + 1;
            }
        }

        if line != point.line {
            return Err(DocumentError::LineOutOfBounds {
                line: point.line,
                line_count: self.line_count(),
            });
        }

        let line_end = self.text[line_start..]
            .find('\n')
            .map_or(self.text.len(), |relative| line_start + relative);
        let offset = line_start.saturating_add(point.byte_column);
        if offset > line_end {
            return Err(DocumentError::ColumnOutOfBounds {
                point,
                line_byte_len: line_end - line_start,
            });
        }
        validate_boundary(&self.text, ByteOffset(offset), "point")?;
        Ok(ByteOffset(offset))
    }

    #[must_use]
    pub fn find_all(&self, needle: &str) -> Vec<Range<ByteOffset>> {
        if needle.is_empty() {
            return Vec::new();
        }
        self.text
            .match_indices(needle)
            .map(|(start, found)| ByteOffset(start)..ByteOffset(start + found.len()))
            .collect()
    }

    /// Applies all edits atomically or leaves the document unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error for stale versions, invalid UTF-8 boundaries,
    /// overlapping edits, invalid selections, or version exhaustion.
    pub fn apply(
        &mut self,
        mut transaction: Transaction,
    ) -> Result<DocumentVersion, DocumentError> {
        if transaction.base_version != self.version {
            return Err(DocumentError::VersionConflict {
                expected: self.version,
                actual: transaction.base_version,
            });
        }

        transaction.edits.sort_by_key(|edit| edit.range.start);
        validate_edits(&self.text, &transaction.edits)?;
        validate_selections(&self.text, &transaction.selections_before)?;
        if !transaction.selections_before.is_empty()
            && transaction.selections_before != self.selections
        {
            return Err(DocumentError::SelectionConflict);
        }
        let next_version = self.next_version()?;

        let mut next = self.text.to_string();
        for edit in transaction.edits.iter().rev() {
            next.replace_range(edit.range.start.0..edit.range.end.0, &edit.insert);
        }

        let next_selections = if transaction.selections_after.is_empty() {
            self.selections
                .iter()
                .copied()
                .map(|selection| transform_selection(selection, &transaction.edits))
                .collect()
        } else {
            validate_selections(&next, &transaction.selections_after)?;
            transaction.selections_after.clone()
        };

        let before = Arc::clone(&self.text);
        let after: Arc<str> = Arc::from(next);
        let history = HistoryEntry {
            before,
            after: Arc::clone(&after),
            selections_before: self.selections.clone(),
            selections_after: next_selections.clone(),
            origin: transaction.origin,
        };

        self.text = after;
        self.selections = next_selections;
        self.version = next_version;
        self.undo.push(history);
        self.redo.clear();
        Ok(self.version)
    }

    /// Restores the previous committed state, if one exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the monotonic version counter is exhausted.
    pub fn undo(&mut self) -> Result<Option<DocumentVersion>, DocumentError> {
        if self.undo.is_empty() {
            return Ok(None);
        }
        let next_version = self.next_version()?;
        let Some(entry) = self.undo.pop() else {
            unreachable!("undo history was checked immediately before pop");
        };
        self.text = Arc::clone(&entry.before);
        self.selections.clone_from(&entry.selections_before);
        self.version = next_version;
        self.redo.push(entry);
        Ok(Some(self.version))
    }

    /// Restores the next state from redo history, if one exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the monotonic version counter is exhausted.
    pub fn redo(&mut self) -> Result<Option<DocumentVersion>, DocumentError> {
        if self.redo.is_empty() {
            return Ok(None);
        }
        let next_version = self.next_version()?;
        let Some(entry) = self.redo.pop() else {
            unreachable!("redo history was checked immediately before pop");
        };
        self.text = Arc::clone(&entry.after);
        self.selections.clone_from(&entry.selections_after);
        self.version = next_version;
        self.undo.push(entry);
        Ok(Some(self.version))
    }

    #[must_use]
    pub fn last_edit_origin(&self) -> Option<&EditOrigin> {
        self.undo.last().map(|entry| &entry.origin)
    }

    fn next_version(&self) -> Result<DocumentVersion, DocumentError> {
        self.version
            .0
            .checked_add(1)
            .map(DocumentVersion)
            .ok_or(DocumentError::VersionExhausted)
    }
}

/// Transforms an anchor through edits expressed in the same source coordinates.
#[must_use]
pub fn transform_anchor(anchor: Anchor, edits: &[TextEdit]) -> Anchor {
    let mut cumulative_delta: i128 = 0;
    for edit in edits {
        let start = edit.range.start.0;
        let end = edit.range.end.0;
        let removed = end - start;
        let inserted = edit.insert.len();
        let offset = anchor.offset.0;

        if offset < start {
            break;
        }
        if offset > end {
            cumulative_delta += usize_to_i128(inserted) - usize_to_i128(removed);
            continue;
        }

        if start == end && offset == start {
            if anchor.affinity == Affinity::Left {
                return Anchor {
                    offset: ByteOffset(add_signed(start, cumulative_delta)),
                    affinity: anchor.affinity,
                };
            }
            cumulative_delta += usize_to_i128(inserted);
            continue;
        }

        let replacement_start = add_signed(start, cumulative_delta);
        let transformed = match anchor.affinity {
            Affinity::Left => replacement_start,
            Affinity::Right => replacement_start + inserted,
        };
        return Anchor {
            offset: ByteOffset(transformed),
            affinity: anchor.affinity,
        };
    }

    Anchor {
        offset: ByteOffset(add_signed(anchor.offset.0, cumulative_delta)),
        affinity: anchor.affinity,
    }
}

#[must_use]
pub fn transform_selection(selection: Selection, edits: &[TextEdit]) -> Selection {
    Selection {
        anchor: transform_anchor(selection.anchor, edits),
        head: transform_anchor(selection.head, edits),
    }
}

fn usize_to_i128(value: usize) -> i128 {
    i128::try_from(value).expect("usize must fit in i128 on supported targets")
}

fn add_signed(value: usize, delta: i128) -> usize {
    let adjusted = usize_to_i128(value) + delta;
    usize::try_from(adjusted).expect("validated edits must produce a valid byte offset")
}

fn validate_edits(text: &str, edits: &[TextEdit]) -> Result<(), DocumentError> {
    let mut previous_end = ByteOffset(0);
    for (index, edit) in edits.iter().enumerate() {
        if edit.range.start > edit.range.end {
            return Err(DocumentError::ReversedRange {
                start: edit.range.start,
                end: edit.range.end,
            });
        }
        validate_boundary(text, edit.range.start, "edit start")?;
        validate_boundary(text, edit.range.end, "edit end")?;
        if index > 0 && edit.range.start < previous_end {
            return Err(DocumentError::OverlappingEdits {
                previous_end,
                next_start: edit.range.start,
            });
        }
        previous_end = edit.range.end;
    }
    Ok(())
}

fn validate_selections(text: &str, selections: &[Selection]) -> Result<(), DocumentError> {
    for selection in selections {
        validate_boundary(text, selection.anchor.offset, "selection anchor")?;
        validate_boundary(text, selection.head.offset, "selection head")?;
    }
    Ok(())
}

fn validate_boundary(
    text: &str,
    offset: ByteOffset,
    context: &'static str,
) -> Result<(), DocumentError> {
    if offset.0 > text.len() {
        return Err(DocumentError::OffsetOutOfBounds {
            offset,
            byte_len: text.len(),
            context,
        });
    }
    if !text.is_char_boundary(offset.0) {
        return Err(DocumentError::NotCharBoundary { offset, context });
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentError {
    VersionConflict {
        expected: DocumentVersion,
        actual: DocumentVersion,
    },
    SelectionConflict,
    VersionExhausted,
    OffsetOutOfBounds {
        offset: ByteOffset,
        byte_len: usize,
        context: &'static str,
    },
    NotCharBoundary {
        offset: ByteOffset,
        context: &'static str,
    },
    ReversedRange {
        start: ByteOffset,
        end: ByteOffset,
    },
    OverlappingEdits {
        previous_end: ByteOffset,
        next_start: ByteOffset,
    },
    LineOutOfBounds {
        line: usize,
        line_count: usize,
    },
    ColumnOutOfBounds {
        point: TextPoint,
        line_byte_len: usize,
    },
}

impl fmt::Display for DocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VersionConflict { expected, actual } => write!(
                formatter,
                "transaction version {actual:?} does not match document version {expected:?}"
            ),
            Self::SelectionConflict => formatter.write_str(
                "transaction selection state does not match the current document selection",
            ),
            Self::VersionExhausted => formatter.write_str("document version counter exhausted"),
            Self::OffsetOutOfBounds {
                offset,
                byte_len,
                context,
            } => write!(
                formatter,
                "{context} offset {} exceeds document length {byte_len}",
                offset.0
            ),
            Self::NotCharBoundary { offset, context } => write!(
                formatter,
                "{context} offset {} is not a UTF-8 character boundary",
                offset.0
            ),
            Self::ReversedRange { start, end } => {
                write!(formatter, "edit range {}..{} is reversed", start.0, end.0)
            }
            Self::OverlappingEdits {
                previous_end,
                next_start,
            } => write!(
                formatter,
                "edit beginning at {} overlaps an edit ending at {}",
                next_start.0, previous_end.0
            ),
            Self::LineOutOfBounds { line, line_count } => {
                write!(formatter, "line {line} is outside {line_count} lines")
            }
            Self::ColumnOutOfBounds {
                point,
                line_byte_len,
            } => write!(
                formatter,
                "byte column {} is outside line {} with byte length {line_byte_len}",
                point.byte_column, point.line
            ),
        }
    }
}

impl std::error::Error for DocumentError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_transaction(document: &Document) -> Transaction {
        Transaction::new(document.version(), EditOrigin::User)
    }

    #[test]
    fn applies_atomic_multi_edits_in_source_coordinates() {
        let mut document = Document::new("alpha beta gamma");
        let transaction = user_transaction(&document)
            .with_edit(TextEdit::replace(ByteOffset(0)..ByteOffset(5), "A"))
            .with_edit(TextEdit::replace(ByteOffset(11)..ByteOffset(16), "G"));

        document.apply(transaction).unwrap();

        assert_eq!(document.text(), "A beta G");
        assert_eq!(document.version(), DocumentVersion(1));
    }

    #[test]
    fn rejects_stale_transactions_without_mutation() {
        let mut document = Document::new("one");
        let stale = Transaction::new(DocumentVersion(99), EditOrigin::User)
            .with_edit(TextEdit::insert(ByteOffset(0), "x"));

        assert!(matches!(
            document.apply(stale),
            Err(DocumentError::VersionConflict { .. })
        ));
        assert_eq!(document.text(), "one");
        assert_eq!(document.version(), DocumentVersion(0));
    }

    #[test]
    fn rejects_overlaps_and_non_utf8_boundaries() {
        let mut document = Document::new("a💡b");
        let overlap = user_transaction(&document)
            .with_edit(TextEdit::delete(ByteOffset(0)..ByteOffset(2)))
            .with_edit(TextEdit::delete(ByteOffset(1)..ByteOffset(5)));
        assert!(matches!(
            document.apply(overlap),
            Err(DocumentError::NotCharBoundary { .. } | DocumentError::OverlappingEdits { .. })
        ));

        let invalid =
            user_transaction(&document).with_edit(TextEdit::insert(ByteOffset(2), "invalid"));
        assert!(matches!(
            document.apply(invalid),
            Err(DocumentError::NotCharBoundary { .. })
        ));
        assert_eq!(document.text(), "a💡b");
    }

    #[test]
    fn converts_unicode_points_without_confusing_bytes_and_characters() {
        let document = Document::new("a💡b\nçd");
        assert_eq!(
            document.offset_to_point(ByteOffset(5)).unwrap(),
            TextPoint {
                line: 0,
                byte_column: 5
            }
        );
        assert_eq!(
            document
                .point_to_offset(TextPoint {
                    line: 1,
                    byte_column: 2
                })
                .unwrap(),
            ByteOffset(9)
        );
    }

    #[test]
    fn transforms_anchor_affinity_at_insertions() {
        let edit = TextEdit::insert(ByteOffset(2), "XYZ");
        let left = Anchor {
            offset: ByteOffset(2),
            affinity: Affinity::Left,
        };
        let right = Anchor {
            offset: ByteOffset(2),
            affinity: Affinity::Right,
        };

        assert_eq!(
            transform_anchor(left, std::slice::from_ref(&edit)).offset,
            ByteOffset(2)
        );
        assert_eq!(transform_anchor(right, &[edit]).offset, ByteOffset(5));
    }

    #[test]
    fn right_affinity_follows_all_insertions_at_the_same_offset() {
        let edits = [
            TextEdit::insert(ByteOffset(2), "A"),
            TextEdit::insert(ByteOffset(2), "BC"),
        ];
        let right = Anchor {
            offset: ByteOffset(2),
            affinity: Affinity::Right,
        };

        assert_eq!(transform_anchor(right, &edits).offset, ByteOffset(5));
    }

    #[test]
    fn rejects_mismatched_selection_state_without_mutation() {
        let mut document = Document::new("text");
        let before = document.snapshot();
        let mut transaction =
            user_transaction(&document).with_edit(TextEdit::insert(ByteOffset(4), " changed"));
        transaction.selections_before = vec![Selection::caret(ByteOffset(2))];

        assert_eq!(
            document.apply(transaction),
            Err(DocumentError::SelectionConflict)
        );
        assert_eq!(document.snapshot(), before);
    }

    #[test]
    fn undo_and_redo_keep_versions_monotonic() {
        let mut document = Document::new("start");
        let transaction =
            user_transaction(&document).with_edit(TextEdit::insert(ByteOffset(5), " end"));
        document.apply(transaction).unwrap();
        assert_eq!(document.version(), DocumentVersion(1));

        assert_eq!(document.undo().unwrap(), Some(DocumentVersion(2)));
        assert_eq!(document.text(), "start");
        assert_eq!(document.redo().unwrap(), Some(DocumentVersion(3)));
        assert_eq!(document.text(), "start end");
    }

    #[test]
    fn exact_search_returns_byte_ranges() {
        let document = Document::new("one 💡 one 💡");
        assert_eq!(
            document.find_all("💡"),
            vec![ByteOffset(4)..ByteOffset(8), ByteOffset(13)..ByteOffset(17)]
        );
    }

    #[test]
    fn randomized_edits_all_undo_to_exact_original() {
        let original = "αlpha\nβeta\n💡";
        let mut document = Document::new(original);
        let mut state = 0x4d59_5df4_d0f3_3173_u64;
        let edit_count = 500;

        for _ in 0..edit_count {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let boundaries: Vec<_> = document
                .text()
                .char_indices()
                .map(|(index, _)| index)
                .chain(std::iter::once(document.byte_len()))
                .collect();
            let boundary_count = u64::try_from(boundaries.len()).unwrap();
            let boundary_index = usize::try_from(state % boundary_count).unwrap();
            let boundary = boundaries[boundary_index];
            let insert = match state % 4 {
                0 => "x",
                1 => "💡",
                2 => "\n",
                _ => "é",
            };
            let transaction = user_transaction(&document)
                .with_edit(TextEdit::insert(ByteOffset(boundary), insert));
            document.apply(transaction).unwrap();
        }

        for _ in 0..edit_count {
            assert!(document.undo().unwrap().is_some());
        }
        assert_eq!(document.text(), original);
        assert!(document.undo().unwrap().is_none());
    }

    #[test]
    fn edit_latency_budget_handles_one_thousand_transactions() {
        let started = std::time::Instant::now();
        let mut document = Document::new(String::new());
        for index in 0..1_000 {
            let transaction = Transaction::new(document.version(), EditOrigin::User).with_edit(
                TextEdit::insert(ByteOffset(document.text().len()), format!("{index}\n")),
            );
            document.apply(transaction).unwrap();
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "1,000 edits exceeded the two-second debug-build budget"
        );
    }
}
