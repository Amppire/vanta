use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;

use vanta_document::{ByteOffset, Document};

fn main() {
    if let Err(error) = run() {
        eprintln!("vanta: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let Some(command) = arguments.next() else {
        return Err("usage: vanta-cli inspect <path>".into());
    };
    if command != "inspect" {
        return Err("only the `inspect` command is available in v0.1".into());
    }
    let Some(path) = arguments.next() else {
        return Err("usage: vanta-cli inspect <path>".into());
    };
    if arguments.next().is_some() {
        return Err("inspect accepts exactly one path".into());
    }

    inspect(Path::new(&path))
}

fn inspect(path: &Path) -> Result<(), Box<dyn Error>> {
    let bytes = fs::read(path)?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        format!(
            "{} is not valid UTF-8 (first invalid byte near {}): {error}",
            path.display(),
            error.valid_up_to()
        )
    })?;
    let document = Document::new(text);
    let end = document.offset_to_point(ByteOffset(document.byte_len()))?;

    println!("path: {}", path.display());
    println!("bytes: {}", document.byte_len());
    println!("lines: {}", document.line_count());
    println!("version: {}", document.version().0);
    println!("end: {}:{}", end.line, end.byte_column);
    Ok(())
}
