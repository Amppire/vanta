use std::env;
use std::error::Error;
use std::path::Path;

use vanta_document::{ByteOffset, Document};
use vanta_file_io::load_text;

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
    let loaded = load_text(path)?;
    let document = Document::new(loaded.text);
    let end = document.offset_to_point(ByteOffset(document.byte_len()))?;

    println!("path: {}", path.display());
    println!("bytes: {}", document.byte_len());
    println!("lines: {}", document.line_count());
    println!("version: {}", document.version().0);
    println!("end: {}:{}", end.line, end.byte_column);
    println!("encoding: {:?}", loaded.format.encoding);
    println!("line endings: {:?}", loaded.format.line_endings);
    println!("final line ending: {}", loaded.format.has_final_line_ending);
    Ok(())
}
