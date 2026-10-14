//! Safe file loading, conflict-aware saving, and crash recovery for Vanta.
//!
//! The APIs in this crate deliberately keep filesystem state separate from the
//! document engine. A loaded fingerprint is an optimistic concurrency token:
//! saving with it refuses to overwrite content changed by another process.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";
const RECOVERY_MAGIC: &[u8; 8] = b"VREC0001";
const NONE_LENGTH: u32 = u32::MAX;

/// The byte encoding used when the file was loaded.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextEncoding {
    #[default]
    Utf8,
    Utf8WithBom,
}

/// The line-ending pattern observed in a file.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LineEndings {
    #[default]
    None,
    Lf,
    Crlf,
    Cr,
    Mixed,
}

/// Text-format properties that must survive a round trip.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TextFormat {
    pub encoding: TextEncoding,
    pub line_endings: LineEndings,
    pub has_final_line_ending: bool,
}

/// A fingerprint of the exact bytes read from disk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileState {
    byte_len: u64,
    modified: Option<SystemTime>,
    content_hash: u64,
}

impl FileState {
    #[must_use]
    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    #[must_use]
    pub const fn modified(&self) -> Option<SystemTime> {
        self.modified
    }

    #[must_use]
    pub const fn content_hash(&self) -> u64 {
        self.content_hash
    }
}

/// Decoded text and the disk state against which later saves should compare.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedText {
    pub text: String,
    pub format: TextFormat,
    pub state: FileState,
}

/// Loads a valid UTF-8 text file without silently replacing invalid bytes.
///
/// # Errors
///
/// Returns an error for filesystem failures, NUL-containing files, and invalid
/// UTF-8. UTF-16 and legacy encodings are intentionally not guessed yet.
pub fn load_text(path: &Path) -> Result<LoadedText, FileIoError> {
    let bytes = fs::read(path).map_err(|source| FileIoError::Io {
        operation: "read",
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = fs::metadata(path).map_err(|source| FileIoError::Io {
        operation: "read metadata for",
        path: path.to_path_buf(),
        source,
    })?;

    if let Some(offset) = bytes.iter().position(|byte| *byte == 0) {
        return Err(FileIoError::BinaryContent { offset });
    }

    let (encoding, text_bytes) = if bytes.starts_with(UTF8_BOM) {
        (TextEncoding::Utf8WithBom, &bytes[UTF8_BOM.len()..])
    } else {
        (TextEncoding::Utf8, bytes.as_slice())
    };
    let text = std::str::from_utf8(text_bytes)
        .map_err(|error| FileIoError::InvalidUtf8 {
            valid_up_to: error.valid_up_to(),
        })?
        .to_owned();
    let format = detect_format(&text, encoding);
    let byte_len = u64::try_from(bytes.len()).map_err(|_| FileIoError::FileTooLarge)?;

    Ok(LoadedText {
        text,
        format,
        state: FileState {
            byte_len,
            modified: metadata.modified().ok(),
            content_hash: stable_hash(&bytes),
        },
    })
}

/// Returns the current fingerprint of a file without decoding it.
///
/// # Errors
///
/// Returns an error if the file cannot be read or its length is unsupported.
pub fn current_file_state(path: &Path) -> Result<FileState, FileIoError> {
    let bytes = fs::read(path).map_err(|source| FileIoError::Io {
        operation: "read",
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = fs::metadata(path).map_err(|source| FileIoError::Io {
        operation: "read metadata for",
        path: path.to_path_buf(),
        source,
    })?;
    Ok(FileState {
        byte_len: u64::try_from(bytes.len()).map_err(|_| FileIoError::FileTooLarge)?,
        modified: metadata.modified().ok(),
        content_hash: stable_hash(&bytes),
    })
}

/// Saves text through a temporary sibling file and refuses stale overwrites.
///
/// When `expected` is present, the destination's bytes must still match the
/// state captured by [`load_text`]. A successful save returns the new state.
///
/// # Errors
///
/// Returns [`FileIoError::ExternalChange`] when the destination changed, plus
/// errors for unsupported content sizes and filesystem failures.
pub fn save_text_atomic(
    path: &Path,
    text: &str,
    format: TextFormat,
    expected: Option<&FileState>,
) -> Result<FileState, FileIoError> {
    if let Some(expected) = expected {
        verify_expected_state(path, expected)?;
    }

    let bytes = encode_text(text, format.encoding);
    atomic_write(path, &bytes, true, expected)?;
    current_file_state(path)
}

fn verify_expected_state(path: &Path, expected: &FileState) -> Result<(), FileIoError> {
    match current_file_state(path) {
        Ok(actual) if &actual == expected => Ok(()),
        Ok(actual) => Err(FileIoError::ExternalChange {
            expected: expected.clone(),
            actual: Some(actual),
        }),
        Err(FileIoError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            Err(FileIoError::ExternalChange {
                expected: expected.clone(),
                actual: None,
            })
        }
        Err(error) => Err(error),
    }
}

fn encode_text(text: &str, encoding: TextEncoding) -> Vec<u8> {
    let bom_len = usize::from(encoding == TextEncoding::Utf8WithBom) * UTF8_BOM.len();
    let mut bytes = Vec::with_capacity(text.len() + bom_len);
    if encoding == TextEncoding::Utf8WithBom {
        bytes.extend_from_slice(UTF8_BOM);
    }
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

fn detect_format(text: &str, encoding: TextEncoding) -> TextFormat {
    let bytes = text.as_bytes();
    let mut lf = 0_usize;
    let mut crlf = 0_usize;
    let mut cr = 0_usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                crlf += 1;
                index += 2;
            }
            b'\r' => {
                cr += 1;
                index += 1;
            }
            b'\n' => {
                lf += 1;
                index += 1;
            }
            _ => index += 1,
        }
    }

    let kinds = usize::from(lf > 0) + usize::from(crlf > 0) + usize::from(cr > 0);
    let line_endings = match (kinds, lf, crlf, cr) {
        (0, _, _, _) => LineEndings::None,
        (1, 1.., _, _) => LineEndings::Lf,
        (1, _, 1.., _) => LineEndings::Crlf,
        (1, _, _, 1..) => LineEndings::Cr,
        _ => LineEndings::Mixed,
    };
    let has_final_line_ending = text.ends_with('\n') || text.ends_with('\r');

    TextFormat {
        encoding,
        line_endings,
        has_final_line_ending,
    }
}

fn atomic_write(
    path: &Path,
    bytes: &[u8],
    preserve_permissions: bool,
    expected: Option<&FileState>,
) -> Result<(), FileIoError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| FileIoError::InvalidDestination {
            path: path.to_path_buf(),
        })?;
    let original_permissions = if preserve_permissions {
        fs::metadata(path)
            .ok()
            .map(|metadata| metadata.permissions())
    } else {
        None
    };

    let mut last_collision = None;
    for attempt in 0..100_u32 {
        let temporary = parent.join(format!(
            ".{}.vanta-{}-{attempt}.tmp",
            file_name.to_string_lossy(),
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(mut file) => {
                let mut guard = TemporaryFileGuard::new(temporary.clone());
                file.write_all(bytes).map_err(|source| FileIoError::Io {
                    operation: "write temporary file for",
                    path: path.to_path_buf(),
                    source,
                })?;
                file.sync_all().map_err(|source| FileIoError::Io {
                    operation: "flush temporary file for",
                    path: path.to_path_buf(),
                    source,
                })?;
                drop(file);

                if let Some(permissions) = original_permissions {
                    fs::set_permissions(&temporary, permissions).map_err(|source| {
                        FileIoError::Io {
                            operation: "preserve permissions for",
                            path: path.to_path_buf(),
                            source,
                        }
                    })?;
                }

                // Recheck after the potentially slow temporary write so an
                // external edit observed during that interval is not replaced.
                if let Some(expected) = expected {
                    verify_expected_state(path, expected)?;
                }

                replace_file(&temporary, path).map_err(|source| FileIoError::Io {
                    operation: "replace",
                    path: path.to_path_buf(),
                    source,
                })?;
                guard.disarm();
                sync_directory(parent)?;
                return Ok(());
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                last_collision = Some(source);
            }
            Err(source) => {
                return Err(FileIoError::Io {
                    operation: "create temporary file for",
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
    }

    Err(FileIoError::Io {
        operation: "create unique temporary file for",
        path: path.to_path_buf(),
        source: last_collision.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::AlreadyExists, "temporary file collision")
        }),
    })
}

#[cfg(unix)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    // Rust's standard-library rename cannot replace an existing Windows file.
    // The Windows platform adapter will replace this with ReplaceFileW before
    // Windows becomes a supported release target.
    if destination.exists() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic replacement on Windows requires the platform adapter",
        ));
    }
    fs::rename(source, destination)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), FileIoError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| FileIoError::Io {
            operation: "flush directory",
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), FileIoError> {
    Ok(())
}

struct TemporaryFileGuard {
    path: Option<PathBuf>,
}

impl TemporaryFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for TemporaryFileGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

/// One recoverable dirty document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryRecord {
    pub document_id: String,
    pub source_path: Option<String>,
    pub version: u64,
    pub encoding: TextEncoding,
    pub text: String,
}

/// A directory of independently checksummed recovery records.
#[derive(Clone, Debug)]
pub struct RecoveryJournal {
    directory: PathBuf,
}

impl RecoveryJournal {
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// Atomically writes the latest recovery record for a document.
    ///
    /// # Errors
    ///
    /// Returns an error when fields exceed the journal format or filesystem
    /// operations fail.
    pub fn write(&self, record: &RecoveryRecord) -> Result<PathBuf, FileIoError> {
        fs::create_dir_all(&self.directory).map_err(|source| FileIoError::Io {
            operation: "create recovery directory",
            path: self.directory.clone(),
            source,
        })?;
        let bytes = encode_recovery(record)?;
        let path = self.path_for(&record.document_id);
        atomic_write(&path, &bytes, false, None)?;
        Ok(path)
    }

    /// Reads and validates one recovery record.
    ///
    /// # Errors
    ///
    /// Returns an error for filesystem failures, corruption, unsupported
    /// versions, invalid UTF-8, or mismatched checksums.
    pub fn read(&self, path: &Path) -> Result<RecoveryRecord, FileIoError> {
        let bytes = fs::read(path).map_err(|source| FileIoError::Io {
            operation: "read recovery record",
            path: path.to_path_buf(),
            source,
        })?;
        decode_recovery(&bytes)
    }

    /// Lists record paths without interpreting potentially corrupt contents.
    ///
    /// # Errors
    ///
    /// Returns an error when the recovery directory cannot be read. A missing
    /// directory is treated as an empty journal.
    pub fn entries(&self) -> Result<Vec<PathBuf>, FileIoError> {
        let iterator = match fs::read_dir(&self.directory) {
            Ok(iterator) => iterator,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(FileIoError::Io {
                    operation: "list recovery directory",
                    path: self.directory.clone(),
                    source,
                });
            }
        };
        let mut entries = Vec::new();
        for entry in iterator {
            let entry = entry.map_err(|source| FileIoError::Io {
                operation: "read recovery directory entry",
                path: self.directory.clone(),
                source,
            })?;
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "vrec")
            {
                entries.push(entry.path());
            }
        }
        entries.sort();
        Ok(entries)
    }

    /// Deletes the recovery record for a document after a successful save or
    /// explicit user discard.
    ///
    /// # Errors
    ///
    /// Returns an error when deletion fails for a reason other than absence.
    pub fn remove(&self, document_id: &str) -> Result<(), FileIoError> {
        let path = self.path_for(document_id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(FileIoError::Io {
                operation: "remove recovery record",
                path,
                source,
            }),
        }
    }

    fn path_for(&self, document_id: &str) -> PathBuf {
        self.directory
            .join(format!("{:016x}.vrec", stable_hash(document_id.as_bytes())))
    }
}

fn encode_recovery(record: &RecoveryRecord) -> Result<Vec<u8>, FileIoError> {
    let document_id = record.document_id.as_bytes();
    let source_path = record.source_path.as_deref().map(str::as_bytes);
    let text = record.text.as_bytes();
    let document_id_len =
        u32::try_from(document_id.len()).map_err(|_| FileIoError::RecoveryFieldTooLarge)?;
    let source_path_len = source_path
        .map(|path| u32::try_from(path.len()).map_err(|_| FileIoError::RecoveryFieldTooLarge))
        .transpose()?
        .unwrap_or(NONE_LENGTH);
    let text_len = u64::try_from(text.len()).map_err(|_| FileIoError::RecoveryFieldTooLarge)?;

    let mut payload = Vec::new();
    payload.extend_from_slice(&record.version.to_le_bytes());
    payload.push(match record.encoding {
        TextEncoding::Utf8 => 0,
        TextEncoding::Utf8WithBom => 1,
    });
    payload.extend_from_slice(&document_id_len.to_le_bytes());
    payload.extend_from_slice(&source_path_len.to_le_bytes());
    payload.extend_from_slice(&text_len.to_le_bytes());
    payload.extend_from_slice(document_id);
    if let Some(source_path) = source_path {
        payload.extend_from_slice(source_path);
    }
    payload.extend_from_slice(text);

    let mut bytes = Vec::with_capacity(RECOVERY_MAGIC.len() + payload.len() + 8);
    bytes.extend_from_slice(RECOVERY_MAGIC);
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(&stable_hash(&payload).to_le_bytes());
    Ok(bytes)
}

fn decode_recovery(bytes: &[u8]) -> Result<RecoveryRecord, FileIoError> {
    let minimum = RECOVERY_MAGIC.len() + 8 + 1 + 4 + 4 + 8 + 8;
    if bytes.len() < minimum || !bytes.starts_with(RECOVERY_MAGIC) {
        return Err(FileIoError::CorruptRecovery(
            "missing header or truncated record",
        ));
    }
    let checksum_start = bytes.len() - 8;
    let payload = &bytes[RECOVERY_MAGIC.len()..checksum_start];
    let stored_checksum = u64::from_le_bytes(
        bytes[checksum_start..]
            .try_into()
            .map_err(|_| FileIoError::CorruptRecovery("invalid checksum"))?,
    );
    if stable_hash(payload) != stored_checksum {
        return Err(FileIoError::CorruptRecovery("checksum mismatch"));
    }

    let mut cursor = RecoveryCursor::new(payload);
    let version = cursor.u64()?;
    let encoding = match cursor.u8()? {
        0 => TextEncoding::Utf8,
        1 => TextEncoding::Utf8WithBom,
        _ => return Err(FileIoError::CorruptRecovery("unknown encoding")),
    };
    let document_id_len = cursor.u32()?;
    let source_path_len = cursor.u32()?;
    let text_len = cursor.u64()?;
    let document_id = cursor.utf8(
        usize::try_from(document_id_len)
            .map_err(|_| FileIoError::CorruptRecovery("document id length overflow"))?,
    )?;
    let source_path = if source_path_len == NONE_LENGTH {
        None
    } else {
        Some(
            cursor.utf8(
                usize::try_from(source_path_len)
                    .map_err(|_| FileIoError::CorruptRecovery("source path length overflow"))?,
            )?,
        )
    };
    let text = cursor.utf8(
        usize::try_from(text_len)
            .map_err(|_| FileIoError::CorruptRecovery("text length overflow"))?,
    )?;
    if !cursor.is_empty() {
        return Err(FileIoError::CorruptRecovery("unexpected trailing payload"));
    }

    Ok(RecoveryRecord {
        document_id,
        source_path,
        version,
        encoding,
        text,
    })
}

struct RecoveryCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> RecoveryCursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], FileIoError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(FileIoError::CorruptRecovery("field length overflow"))?;
        let field = self
            .bytes
            .get(self.offset..end)
            .ok_or(FileIoError::CorruptRecovery("truncated field"))?;
        self.offset = end;
        Ok(field)
    }

    fn u8(&mut self) -> Result<u8, FileIoError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, FileIoError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(
            |_| FileIoError::CorruptRecovery("invalid u32"),
        )?))
    }

    fn u64(&mut self) -> Result<u64, FileIoError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().map_err(
            |_| FileIoError::CorruptRecovery("invalid u64"),
        )?))
    }

    fn utf8(&mut self, length: usize) -> Result<String, FileIoError> {
        let bytes = self.take(length)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| FileIoError::CorruptRecovery("invalid UTF-8 field"))
    }

    const fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[derive(Debug)]
pub enum FileIoError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    InvalidUtf8 {
        valid_up_to: usize,
    },
    BinaryContent {
        offset: usize,
    },
    ExternalChange {
        expected: FileState,
        actual: Option<FileState>,
    },
    InvalidDestination {
        path: PathBuf,
    },
    FileTooLarge,
    RecoveryFieldTooLarge,
    CorruptRecovery(&'static str),
}

impl fmt::Display for FileIoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "failed to {operation} {}: {source}",
                path.display()
            ),
            Self::InvalidUtf8 { valid_up_to } => {
                write!(formatter, "file is not valid UTF-8 near byte {valid_up_to}")
            }
            Self::BinaryContent { offset } => {
                write!(formatter, "file contains a NUL byte at offset {offset}")
            }
            Self::ExternalChange { .. } => {
                formatter.write_str("file changed on disk since it was loaded")
            }
            Self::InvalidDestination { path } => write!(
                formatter,
                "{} is not a valid file destination",
                path.display()
            ),
            Self::FileTooLarge => {
                formatter.write_str("file length is unsupported on this platform")
            }
            Self::RecoveryFieldTooLarge => {
                formatter.write_str("recovery record field is too large")
            }
            Self::CorruptRecovery(reason) => {
                write!(formatter, "recovery record is corrupt: {reason}")
            }
        }
    }
}

impl std::error::Error for FileIoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("vanta-file-io-{}-{sequence}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn loads_bom_and_detects_mixed_line_endings() {
        let directory = TestDirectory::new();
        let path = directory.join("mixed.txt");
        fs::write(&path, b"\xef\xbb\xbfone\r\ntwo\nthree\r").unwrap();

        let loaded = load_text(&path).unwrap();

        assert_eq!(loaded.text, "one\r\ntwo\nthree\r");
        assert_eq!(loaded.format.encoding, TextEncoding::Utf8WithBom);
        assert_eq!(loaded.format.line_endings, LineEndings::Mixed);
        assert!(loaded.format.has_final_line_ending);
    }

    #[test]
    fn rejects_binary_and_invalid_utf8_without_lossy_decoding() {
        let directory = TestDirectory::new();
        let binary = directory.join("binary");
        fs::write(&binary, b"a\0b").unwrap();
        assert!(matches!(
            load_text(&binary),
            Err(FileIoError::BinaryContent { offset: 1 })
        ));

        let invalid = directory.join("invalid");
        fs::write(&invalid, [b'a', 0xff]).unwrap();
        assert!(matches!(
            load_text(&invalid),
            Err(FileIoError::InvalidUtf8 { valid_up_to: 1 })
        ));
    }

    #[test]
    fn atomic_save_round_trips_bom_and_returns_new_state() {
        let directory = TestDirectory::new();
        let path = directory.join("document.txt");
        fs::write(&path, b"\xef\xbb\xbforiginal\r\n").unwrap();
        let loaded = load_text(&path).unwrap();

        let saved =
            save_text_atomic(&path, "changed\r\n", loaded.format, Some(&loaded.state)).unwrap();
        let reloaded = load_text(&path).unwrap();

        assert_eq!(reloaded.text, "changed\r\n");
        assert_eq!(reloaded.format, loaded.format);
        assert_eq!(reloaded.state, saved);
    }

    #[test]
    fn refuses_to_overwrite_an_external_change() {
        let directory = TestDirectory::new();
        let path = directory.join("document.txt");
        fs::write(&path, "original").unwrap();
        let loaded = load_text(&path).unwrap();
        fs::write(&path, "external").unwrap();

        let result = save_text_atomic(&path, "editor", loaded.format, Some(&loaded.state));

        assert!(matches!(result, Err(FileIoError::ExternalChange { .. })));
        assert_eq!(fs::read_to_string(path).unwrap(), "external");
    }

    #[test]
    fn recovery_records_round_trip_and_can_be_removed() {
        let directory = TestDirectory::new();
        let journal = RecoveryJournal::new(directory.join("recovery"));
        let record = RecoveryRecord {
            document_id: "document-1".into(),
            source_path: Some("/workspace/notes.txt".into()),
            version: 42,
            encoding: TextEncoding::Utf8,
            text: "unsaved 💡 text".into(),
        };

        let path = journal.write(&record).unwrap();
        assert_eq!(journal.entries().unwrap(), vec![path.clone()]);
        assert_eq!(journal.read(&path).unwrap(), record);
        journal.remove("document-1").unwrap();
        assert_eq!(journal.entries().unwrap(), Vec::<PathBuf>::new());
    }

    #[test]
    fn recovery_checksum_detects_corruption() {
        let directory = TestDirectory::new();
        let journal = RecoveryJournal::new(directory.join("recovery"));
        let record = RecoveryRecord {
            document_id: "document-2".into(),
            source_path: None,
            version: 7,
            encoding: TextEncoding::Utf8WithBom,
            text: "dirty".into(),
        };
        let path = journal.write(&record).unwrap();
        let mut bytes = fs::read(&path).unwrap();
        let payload_byte = RECOVERY_MAGIC.len() + 3;
        bytes[payload_byte] ^= 0xff;
        fs::write(&path, bytes).unwrap();

        assert!(matches!(
            journal.read(&path),
            Err(FileIoError::CorruptRecovery("checksum mismatch"))
        ));
    }
}
