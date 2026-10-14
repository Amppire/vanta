use std::{
    fmt, fs,
    fs::OpenOptions,
    io::{self, Write},
    ops::Range,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

pub struct Document {
    path: Option<PathBuf>,
    text: String,
    saved_bytes: Vec<u8>,
    dirty: bool,
}

#[derive(Debug)]
pub enum SaveError {
    Untitled,
    ChangedOnDisk,
    Io(io::Error),
}

impl fmt::Display for SaveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Untitled => formatter.write_str("open a file before saving"),
            Self::ChangedOnDisk => formatter.write_str("file changed on disk; save was cancelled"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl From<io::Error> for SaveError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl Document {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let bytes = fs::read(&path)?;
        let text = String::from_utf8(bytes.clone())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "file is not valid UTF-8"))?;
        Ok(Self {
            path: Some(path),
            text,
            saved_bytes: bytes,
            dirty: false,
        })
    }

    pub fn untitled() -> Self {
        Self {
            path: None,
            text: String::new(),
            saved_bytes: Vec::new(),
            dirty: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn replace(&mut self, range: Range<usize>, replacement: &str) {
        assert!(range.start <= range.end);
        assert!(self.text.is_char_boundary(range.start));
        assert!(self.text.is_char_boundary(range.end));
        self.text.replace_range(range, replacement);
        self.dirty = self.text.as_bytes() != self.saved_bytes;
    }

    pub fn save(&mut self) -> Result<(), SaveError> {
        let path = self.path.as_deref().ok_or(SaveError::Untitled)?;
        if fs::read(path)? != self.saved_bytes {
            return Err(SaveError::ChangedOnDisk);
        }

        let bytes = self.text.as_bytes();
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let permissions = fs::metadata(path)?.permissions();
        let mut temp = None;

        for _ in 0..100 {
            let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(".{name}.vanta-{}-{id}", std::process::id()));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(file) => {
                    temp = Some((candidate, file));
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }

        let (temp_path, mut file) = temp.ok_or_else(|| {
            io::Error::new(io::ErrorKind::AlreadyExists, "could not create save file")
        })?;
        let result = (|| -> io::Result<()> {
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::set_permissions(&temp_path, permissions)?;
            fs::rename(&temp_path, path)?;
            sync_parent(parent)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result?;

        self.saved_bytes = bytes.to_vec();
        self.dirty = false;
        Ok(())
    }
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> io::Result<()> {
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_file(name: &str, contents: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "vanta-test-{}-{}",
            std::process::id(),
            TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn saves_atomically_and_rejects_external_changes() {
        let path = test_file("note.txt", "one");
        let mut document = Document::open(path.clone()).unwrap();
        document.replace(0..3, "two");
        document.save().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");

        document.replace(0..3, "three");
        fs::write(&path, "external").unwrap();
        assert!(matches!(document.save(), Err(SaveError::ChangedOnDisk)));
        assert_eq!(fs::read_to_string(&path).unwrap(), "external");

        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
