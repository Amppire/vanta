//! Folder indexing, quick-open ranking, and streaming search.

use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug)]
pub struct CancellationToken(Arc<AtomicBool>);

impl Default for CancellationToken {
    fn default() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
    files: Vec<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchMatch {
    pub path: PathBuf,
    pub line: usize,
    pub byte_column: usize,
    pub preview: String,
}

impl Workspace {
    /// Indexes regular files without following symlinks.
    ///
    /// # Errors
    ///
    /// Returns an error if the root cannot be resolved or a directory cannot be read.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into().canonicalize()?;
        let mut files = Vec::new();
        visit(&root, &root, &mut files)?;
        files.sort();
        Ok(Self { root, files })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    #[must_use]
    pub fn quick_open(&self, query: &str, limit: usize) -> Vec<PathBuf> {
        let mut matches: Vec<_> = self
            .files
            .iter()
            .filter_map(|path| {
                let relative = path.strip_prefix(&self.root).ok()?;
                fuzzy_score(&relative.to_string_lossy(), query)
                    .map(|score| (score, relative.to_path_buf()))
            })
            .collect();
        matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score.cmp(left_score).then_with(|| left.cmp(right))
        });
        matches
            .into_iter()
            .take(limit)
            .map(|(_, path)| path)
            .collect()
    }

    /// Streams literal, case-sensitive matches and checks cancellation between lines.
    ///
    /// # Errors
    ///
    /// Returns an error only when the indexed workspace cannot be traversed.
    pub fn search(
        &self,
        needle: &str,
        cancellation: &CancellationToken,
        mut emit: impl FnMut(SearchMatch),
    ) -> io::Result<()> {
        if needle.is_empty() {
            return Ok(());
        }
        for path in &self.files {
            if cancellation.is_cancelled() {
                break;
            }
            let Ok(file) = fs::File::open(path) else {
                continue;
            };
            for (line_index, line) in BufReader::new(file).lines().enumerate() {
                if cancellation.is_cancelled() {
                    return Ok(());
                }
                let Ok(line) = line else { break };
                for (column, _) in line.match_indices(needle) {
                    emit(SearchMatch {
                        path: path.strip_prefix(&self.root).unwrap_or(path).to_path_buf(),
                        line: line_index + 1,
                        byte_column: column,
                        preview: line.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

fn visit(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            if !is_ignored(root, &path) {
                visit(root, &path, files)?;
            }
        } else if kind.is_file() && !is_ignored(root, &path) {
            files.push(path);
        }
    }
    Ok(())
}

fn is_ignored(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(|relative| {
        relative.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some(".git" | "target" | ".DS_Store")
            )
        })
    })
}

fn fuzzy_score(candidate: &str, query: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let candidate = candidate.to_ascii_lowercase();
    let query = query.to_ascii_lowercase();
    let mut score = 0_i64;
    let mut search_from = 0;
    let mut previous = None;
    for needle in query.chars() {
        let (offset, _) = candidate[search_from..]
            .char_indices()
            .find(|(_, value)| *value == needle)?;
        let index = search_from + offset;
        score += if previous == Some(index.saturating_sub(1)) {
            12
        } else {
            3
        };
        if index == 0
            || matches!(
                candidate.as_bytes().get(index.wrapping_sub(1)),
                Some(b'/' | b'_' | b'-')
            )
        {
            score += 8;
        }
        previous = Some(index);
        search_from = index + needle.len_utf8();
    }
    Some(score - i64::try_from(candidate.len()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn fixture() -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("vanta-workspace-{id}-{sequence}"));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n// main\n").unwrap();
        fs::write(root.join("notes.md"), "main idea\n").unwrap();
        fs::write(root.join(".git/config"), "main secret\n").unwrap();
        root
    }

    #[test]
    fn indexes_without_internal_directories_and_ranks_paths() {
        let root = fixture();
        let workspace = Workspace::open(&root).unwrap();
        assert_eq!(workspace.files().len(), 2);
        assert_eq!(
            workspace.quick_open("smr", 1),
            [PathBuf::from("src/main.rs")]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn search_streams_matches_and_honors_cancellation() {
        let root = fixture();
        let workspace = Workspace::open(&root).unwrap();
        let token = CancellationToken::default();
        let mut matches = Vec::new();
        workspace
            .search("main", &token, |found| {
                matches.push(found);
                token.cancel();
            })
            .unwrap();
        assert_eq!(matches.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn indexing_and_search_stay_within_reference_budget() {
        let root = fixture();
        for index in 0..500 {
            fs::write(
                root.join("src").join(format!("generated-{index}.txt")),
                "alpha beta gamma\nneedle\n",
            )
            .unwrap();
        }
        let started = std::time::Instant::now();
        let workspace = Workspace::open(&root).unwrap();
        let mut count = 0;
        workspace
            .search("needle", &CancellationToken::default(), |_| count += 1)
            .unwrap();
        assert_eq!(count, 500);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "500-file index and search exceeded the five-second debug-build budget"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
