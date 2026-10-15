//! Optional language intelligence primitives.
//!
//! This crate owns the unsafe boundaries between UTF-8 documents, LSP UTF-16
//! positions, versioned edits, JSON-RPC framing, and explicitly approved native
//! language-server processes. It does not start a server during editor startup.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use vanta_document::{ByteOffset, DocumentVersion, EditOrigin, TextEdit, Transaction};

pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
pub const DEFAULT_RESTART_LIMIT: usize = 3;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Utf16Position {
    pub line: usize,
    pub character: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Utf16Range {
    pub start: Utf16Position,
    pub end: Utf16Position,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionedTextEdit {
    pub range: Utf16Range,
    pub new_text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceEdit {
    pub document_version: DocumentVersion,
    pub edits: Vec<VersionedTextEdit>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub range: Utf16Range,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub source: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LspError {
    LineOutOfBounds,
    CharacterOutOfBounds,
    SplitSurrogate,
    InvalidUtf8Boundary,
    StaleVersion {
        expected: DocumentVersion,
        actual: DocumentVersion,
    },
    InvalidEditOrder,
    WorkspaceNotTrusted,
    ExecutableNotApproved(PathBuf),
    Process(String),
    Protocol(&'static str),
    Json(String),
}

impl fmt::Display for LspError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LineOutOfBounds => formatter.write_str("line is outside the document"),
            Self::CharacterOutOfBounds => {
                formatter.write_str("UTF-16 character is outside the line")
            }
            Self::SplitSurrogate => formatter.write_str("UTF-16 position splits a surrogate pair"),
            Self::InvalidUtf8Boundary => formatter.write_str("offset is not a UTF-8 boundary"),
            Self::StaleVersion { expected, actual } => {
                write!(
                    formatter,
                    "stale document version: expected {}, got {}",
                    expected.0, actual.0
                )
            }
            Self::InvalidEditOrder => {
                formatter.write_str("workspace edits overlap or are unordered")
            }
            Self::WorkspaceNotTrusted => formatter.write_str("workspace is not trusted"),
            Self::ExecutableNotApproved(path) => {
                write!(formatter, "executable is not approved: {}", path.display())
            }
            Self::Process(message) | Self::Json(message) => formatter.write_str(message),
            Self::Protocol(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for LspError {}

#[must_use]
pub fn line_start(text: &str, line: usize) -> Option<usize> {
    if line == 0 {
        return Some(0);
    }
    let mut current = 0;
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            current += 1;
            if current == line {
                return Some(index + 1);
            }
        }
    }
    None
}

/// Converts an LSP UTF-16 position into Vanta's canonical UTF-8 byte offset.
///
/// # Errors
/// Rejects missing lines, columns past the line, and columns inside a surrogate pair.
pub fn utf16_to_offset(text: &str, position: Utf16Position) -> Result<ByteOffset, LspError> {
    let start = line_start(text, position.line).ok_or(LspError::LineOutOfBounds)?;
    let rest = &text[start..];
    let line = rest.split_once('\n').map_or(rest, |(line, _)| line);
    let mut utf16 = 0;
    for (byte, character) in line.char_indices() {
        if utf16 == position.character {
            return Ok(ByteOffset(start + byte));
        }
        let next = utf16 + character.len_utf16();
        if position.character < next {
            return Err(LspError::SplitSurrogate);
        }
        utf16 = next;
    }
    if utf16 == position.character {
        Ok(ByteOffset(start + line.len()))
    } else {
        Err(LspError::CharacterOutOfBounds)
    }
}

/// Converts a canonical UTF-8 offset to an LSP UTF-16 position.
///
/// # Errors
/// Rejects offsets outside the document or inside a UTF-8 code point.
pub fn offset_to_utf16(text: &str, offset: ByteOffset) -> Result<Utf16Position, LspError> {
    if offset.0 > text.len() || !text.is_char_boundary(offset.0) {
        return Err(LspError::InvalidUtf8Boundary);
    }
    let before = &text[..offset.0];
    let line = before.bytes().filter(|byte| *byte == b'\n').count();
    let start = before.rfind('\n').map_or(0, |index| index + 1);
    Ok(Utf16Position {
        line,
        character: text[start..offset.0].encode_utf16().count(),
    })
}

/// Produces one atomic document transaction from a versioned LSP workspace edit.
///
/// # Errors
/// Rejects stale versions, invalid UTF-16 ranges, and overlapping edits.
pub fn workspace_edit_transaction(
    text: &str,
    current_version: DocumentVersion,
    edit: WorkspaceEdit,
    server: &str,
) -> Result<Transaction, LspError> {
    if edit.document_version != current_version {
        return Err(LspError::StaleVersion {
            expected: current_version,
            actual: edit.document_version,
        });
    }
    let mut converted = Vec::with_capacity(edit.edits.len());
    for value in edit.edits {
        let start = utf16_to_offset(text, value.range.start)?;
        let end = utf16_to_offset(text, value.range.end)?;
        if start > end {
            return Err(LspError::InvalidEditOrder);
        }
        converted.push(TextEdit::replace(start..end, value.new_text));
    }
    converted.sort_by_key(|value| value.range.start);
    if converted
        .windows(2)
        .any(|pair| pair[0].range.end > pair[1].range.start)
    {
        return Err(LspError::InvalidEditOrder);
    }
    Ok(converted.into_iter().fold(
        Transaction::new(current_version, EditOrigin::Extension(server.to_owned())),
        Transaction::with_edit,
    ))
}

#[derive(Default)]
pub struct DiagnosticStore {
    documents: BTreeMap<String, (DocumentVersion, Vec<Diagnostic>)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LanguageFeature {
    Completion,
    Hover,
    Definition,
    References,
    DocumentSymbols,
    WorkspaceSymbols,
    Diagnostics,
    Rename,
    Formatting,
    CodeActions,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeatureRequest {
    pub id: u64,
    pub feature: LanguageFeature,
    pub document_version: DocumentVersion,
}

#[derive(Default)]
pub struct RequestTracker {
    next_id: u64,
    pending: BTreeMap<u64, FeatureRequest>,
}

impl RequestTracker {
    pub fn begin(
        &mut self,
        feature: LanguageFeature,
        document_version: DocumentVersion,
    ) -> FeatureRequest {
        self.next_id = self.next_id.saturating_add(1);
        let request = FeatureRequest {
            id: self.next_id,
            feature,
            document_version,
        };
        self.pending.insert(request.id, request.clone());
        request
    }

    pub fn cancel(&mut self, id: u64) -> bool {
        self.pending.remove(&id).is_some()
    }

    /// Accepts a response only when it is pending and targets the current document.
    #[must_use]
    pub fn accept(&mut self, id: u64, current_version: DocumentVersion) -> bool {
        self.take_if_current(id, current_version).is_some()
    }

    #[must_use]
    pub fn take_if_current(
        &mut self,
        id: u64,
        current_version: DocumentVersion,
    ) -> Option<FeatureRequest> {
        self.pending
            .remove(&id)
            .filter(|request| request.document_version == current_version)
    }

    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn cancel_all(&mut self) {
        self.pending.clear();
    }
}

impl DiagnosticStore {
    /// Publishes diagnostics only when they target the current document version.
    pub fn publish(
        &mut self,
        uri: impl Into<String>,
        published: DocumentVersion,
        current: DocumentVersion,
        diagnostics: Vec<Diagnostic>,
    ) -> bool {
        if published != current {
            return false;
        }
        self.documents.insert(uri.into(), (published, diagnostics));
        true
    }

    #[must_use]
    pub fn for_document(&self, uri: &str) -> &[Diagnostic] {
        self.documents
            .get(uri)
            .map_or(&[], |(_, diagnostics)| diagnostics)
    }

    pub fn clear(&mut self) {
        self.documents.clear();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerCommand {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub struct ExecutableApprovals {
    approved: BTreeSet<PathBuf>,
}

impl ExecutableApprovals {
    pub fn approve(&mut self, executable: impl Into<PathBuf>) {
        self.approved.insert(executable.into());
    }

    pub fn revoke(&mut self, executable: &Path) {
        self.approved.remove(executable);
    }

    #[must_use]
    pub fn is_approved(&self, executable: &Path) -> bool {
        self.approved.contains(executable)
    }

    pub fn entries(&self) -> impl Iterator<Item = &Path> {
        self.approved.iter().map(PathBuf::as_path)
    }
}

pub struct BrokeredProcess {
    child: Child,
    started: Instant,
}

impl BrokeredProcess {
    #[must_use]
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn stdin(&mut self) -> Option<&mut ChildStdin> {
        self.child.stdin.as_mut()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Stops the server and waits for it to exit.
    ///
    /// # Errors
    /// Returns an error if termination or collection fails.
    pub fn stop(&mut self) -> Result<(), LspError> {
        if self
            .child
            .try_wait()
            .map_err(|error| process_error(&error))?
            .is_none()
        {
            self.child.kill().map_err(|error| process_error(&error))?;
        }
        self.child.wait().map_err(|error| process_error(&error))?;
        Ok(())
    }
}

impl Drop for BrokeredProcess {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[derive(Default)]
pub struct NativeProcessBroker {
    trusted_workspace: bool,
    approvals: ExecutableApprovals,
}

impl NativeProcessBroker {
    pub const fn set_workspace_trusted(&mut self, trusted: bool) {
        self.trusted_workspace = trusted;
    }

    pub fn approvals_mut(&mut self) -> &mut ExecutableApprovals {
        &mut self.approvals
    }

    /// Starts an explicitly approved language server with a minimal environment.
    ///
    /// # Errors
    /// Rejects untrusted workspaces, unapproved executables, and spawn failures.
    pub fn spawn(&self, command: &ServerCommand) -> Result<BrokeredProcess, LspError> {
        if !self.trusted_workspace {
            return Err(LspError::WorkspaceNotTrusted);
        }
        if !self.approvals.is_approved(&command.executable) {
            return Err(LspError::ExecutableNotApproved(command.executable.clone()));
        }
        let child = Command::new(&command.executable)
            .args(&command.arguments)
            .current_dir(&command.working_directory)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| process_error(&error))?;
        Ok(BrokeredProcess {
            child,
            started: Instant::now(),
        })
    }
}

fn process_error(error: &std::io::Error) -> LspError {
    LspError::Process(error.to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerState {
    Disabled,
    Stopped,
    Running,
    Failed,
}

pub struct ServerSupervisor {
    state: ServerState,
    restart_limit: usize,
    restart_times: VecDeque<Instant>,
}

impl Default for ServerSupervisor {
    fn default() -> Self {
        Self {
            state: ServerState::Disabled,
            restart_limit: DEFAULT_RESTART_LIMIT,
            restart_times: VecDeque::new(),
        }
    }
}

impl ServerSupervisor {
    #[must_use]
    pub const fn state(&self) -> ServerState {
        self.state
    }

    pub const fn enable(&mut self) {
        self.state = ServerState::Stopped;
    }

    /// Disables language intelligence and synchronously terminates an active server.
    ///
    /// # Errors
    /// Returns an error if the active process cannot be terminated or collected.
    pub fn disable(&mut self, process: Option<&mut BrokeredProcess>) -> Result<(), LspError> {
        if let Some(process) = process {
            process.stop()?;
        }
        self.restart_times.clear();
        self.state = ServerState::Disabled;
        Ok(())
    }

    pub const fn mark_running(&mut self) {
        self.state = ServerState::Running;
    }

    #[must_use]
    pub fn allow_restart(&mut self, now: Instant) -> bool {
        let window = Duration::from_secs(60);
        while self
            .restart_times
            .front()
            .is_some_and(|time| now.duration_since(*time) > window)
        {
            self.restart_times.pop_front();
        }
        if self.restart_times.len() >= self.restart_limit {
            self.state = ServerState::Failed;
            return false;
        }
        self.restart_times.push_back(now);
        self.state = ServerState::Stopped;
        true
    }
}

/// Encodes one bounded JSON-RPC message using LSP headers.
///
/// # Errors
/// Returns an error if JSON serialization fails or the message exceeds the bound.
pub fn write_message(writer: &mut impl Write, value: &Value) -> Result<(), LspError> {
    let body = serde_json::to_vec(value).map_err(|error| LspError::Json(error.to_string()))?;
    if body.len() > MAX_MESSAGE_BYTES {
        return Err(LspError::Protocol("message too large"));
    }
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())
        .map_err(|error| process_error(&error))?;
    writer
        .write_all(&body)
        .map_err(|error| process_error(&error))
}

/// Reads one bounded LSP JSON-RPC message.
///
/// # Errors
/// Rejects malformed headers, oversized bodies, truncated messages, and invalid JSON.
pub fn read_message(reader: &mut impl BufRead) -> Result<Value, LspError> {
    let mut length = None;
    loop {
        let mut header = String::new();
        if reader
            .read_line(&mut header)
            .map_err(|error| process_error(&error))?
            == 0
        {
            return Err(LspError::Protocol("unexpected end of headers"));
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| LspError::Protocol("invalid content length"))?,
            );
        }
    }
    let length = length.ok_or(LspError::Protocol("missing content length"))?;
    if length > MAX_MESSAGE_BYTES {
        return Err(LspError::Protocol("message too large"));
    }
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .map_err(|error| process_error(&error))?;
    serde_json::from_slice(&body).map_err(|error| LspError::Json(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::ops::Range;
    use vanta_document::Document;

    #[test]
    fn converts_non_ascii_positions_without_splitting_surrogates() {
        let text = "a😀b\nélan";
        assert_eq!(
            utf16_to_offset(
                text,
                Utf16Position {
                    line: 0,
                    character: 3
                }
            ),
            Ok(ByteOffset(5))
        );
        assert_eq!(
            utf16_to_offset(
                text,
                Utf16Position {
                    line: 0,
                    character: 2
                }
            ),
            Err(LspError::SplitSurrogate)
        );
        assert_eq!(
            offset_to_utf16(text, ByteOffset(9)).unwrap(),
            Utf16Position {
                line: 1,
                character: 1
            }
        );
    }

    #[test]
    fn versioned_workspace_edits_reject_stale_results() {
        let document = Document::new("alpha");
        let edit = WorkspaceEdit {
            document_version: DocumentVersion(9),
            edits: Vec::new(),
        };
        assert!(matches!(
            workspace_edit_transaction(document.text(), document.version(), edit, "test"),
            Err(LspError::StaleVersion { .. })
        ));
    }

    #[test]
    fn diagnostics_discard_stale_publications() {
        let mut store = DiagnosticStore::default();
        assert!(!store.publish(
            "file:///a",
            DocumentVersion(1),
            DocumentVersion(2),
            Vec::new()
        ));
        assert_eq!(store.for_document("file:///a"), []);
    }

    #[test]
    fn feature_requests_are_cancellable_and_version_checked() {
        let mut tracker = RequestTracker::default();
        let completion = tracker.begin(LanguageFeature::Completion, DocumentVersion(4));
        assert!(!tracker.accept(completion.id, DocumentVersion(5)));
        let hover = tracker.begin(LanguageFeature::Hover, DocumentVersion(5));
        assert!(tracker.cancel(hover.id));
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn process_broker_requires_trust_and_approval() {
        let mut broker = NativeProcessBroker::default();
        let command = ServerCommand {
            executable: PathBuf::from("language-server"),
            arguments: Vec::new(),
            working_directory: PathBuf::from("."),
        };
        assert!(matches!(
            broker.spawn(&command),
            Err(LspError::WorkspaceNotTrusted)
        ));
        broker.set_workspace_trusted(true);
        assert!(matches!(
            broker.spawn(&command),
            Err(LspError::ExecutableNotApproved(_))
        ));
    }

    #[test]
    fn json_rpc_framing_round_trips() {
        let value = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        let mut bytes = Vec::new();
        write_message(&mut bytes, &value).unwrap();
        let decoded = read_message(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded, value);
    }

    #[test]
    fn workspace_edit_ranges_are_atomic() {
        let mut document = Document::new("a😀b");
        let edit = WorkspaceEdit {
            document_version: document.version(),
            edits: vec![VersionedTextEdit {
                range: Utf16Range {
                    start: Utf16Position {
                        line: 0,
                        character: 1,
                    },
                    end: Utf16Position {
                        line: 0,
                        character: 3,
                    },
                },
                new_text: "ok".to_owned(),
            }],
        };
        let transaction =
            workspace_edit_transaction(document.text(), document.version(), edit, "fixture")
                .unwrap();
        document.apply(transaction).unwrap();
        assert_eq!(document.text(), "aokb");
    }

    #[test]
    fn utf16_range_type_is_copyable() {
        let range = Utf16Range::default();
        let copied: Range<Utf16Position> = range.start..range.end;
        assert_eq!(copied.start, Utf16Position::default());
    }
}
