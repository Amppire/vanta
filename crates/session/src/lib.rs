//! Small, dependency-free session records with partial corruption recovery.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Session {
    pub workspace: Option<PathBuf>,
    pub documents: Vec<DocumentState>,
    pub active: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentState {
    pub path: PathBuf,
    pub line: usize,
    pub byte_column: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LoadReport {
    pub session: Session,
    pub discarded_records: usize,
}

impl Session {
    /// Saves through a sibling temporary file and rename.
    ///
    /// # Errors
    ///
    /// Returns an error if the temporary record cannot be written or renamed.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let temporary = path.with_extension("tmp");
        let mut output = String::from("vanta-session-1\n");
        if let Some(workspace) = &self.workspace {
            writeln!(output, "workspace\t{}", encode(workspace))
                .expect("writing to a string cannot fail");
        }
        writeln!(output, "active\t{}", self.active).expect("writing to a string cannot fail");
        for document in &self.documents {
            writeln!(
                output,
                "document\t{}\t{}\t{}",
                encode(&document.path),
                document.line,
                document.byte_column
            )
            .expect("writing to a string cannot fail");
        }
        fs::write(&temporary, output)?;
        fs::rename(temporary, path)
    }

    /// Loads all valid records. Invalid lines are counted and ignored.
    ///
    /// # Errors
    ///
    /// Returns an error if the session file cannot be read.
    pub fn load(path: &Path) -> io::Result<LoadReport> {
        let input = fs::read_to_string(path)?;
        let mut report = LoadReport::default();
        for (index, line) in input.lines().enumerate() {
            if index == 0 {
                if line != "vanta-session-1" {
                    report.discarded_records += 1;
                }
                continue;
            }
            let fields: Vec<_> = line.split('\t').collect();
            match fields.as_slice() {
                ["workspace", path] => match decode(path) {
                    Some(path) => report.session.workspace = Some(path),
                    None => report.discarded_records += 1,
                },
                ["active", active] => match active.parse() {
                    Ok(active) => report.session.active = active,
                    Err(_) => report.discarded_records += 1,
                },
                ["document", path, line, column] => {
                    match (decode(path), line.parse(), column.parse()) {
                        (Some(path), Ok(line), Ok(byte_column)) => {
                            report.session.documents.push(DocumentState {
                                path,
                                line,
                                byte_column,
                            });
                        }
                        _ => report.discarded_records += 1,
                    }
                }
                _ => report.discarded_records += 1,
            }
        }
        if report.session.active >= report.session.documents.len() {
            report.session.active = report.session.documents.len().saturating_sub(1);
        }
        Ok(report)
    }
}

fn encode(path: &Path) -> String {
    path.to_string_lossy()
        .as_bytes()
        .iter()
        .fold(String::new(), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a string cannot fail");
            output
        })
}

fn decode(value: &str) -> Option<PathBuf> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<_>> = (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).ok())
        .collect();
    String::from_utf8(bytes?).ok().map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn path() -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("vanta-session-{id}"))
    }

    #[test]
    fn round_trips_session() {
        let path = path();
        let session = Session {
            workspace: Some(PathBuf::from("/work")),
            documents: vec![DocumentState {
                path: PathBuf::from("/work/a.rs"),
                line: 7,
                byte_column: 3,
            }],
            active: 0,
        };
        session.save(&path).unwrap();
        assert_eq!(Session::load(&path).unwrap().session, session);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn corrupt_records_do_not_hide_valid_documents() {
        let path = path();
        fs::write(
            &path,
            "broken-header\ndocument\t2f612e7273\t4\t2\ndocument\tnot-hex\tx\t2\nunknown\n",
        )
        .unwrap();
        let report = Session::load(&path).unwrap();
        assert_eq!(report.session.documents.len(), 1);
        assert_eq!(report.discarded_records, 3);
        fs::remove_file(path).unwrap();
    }
}
