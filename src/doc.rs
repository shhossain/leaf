//! Reading files from disk. Runs on a background thread, never on the UI thread.

use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

use crate::text_index::{Bytes, Document};

/// Files above this are memory-mapped instead of read into memory.
pub const MAP_ABOVE_BYTES: u64 = 64 * 1024 * 1024;
/// The full editor holds ~12× the file size; above this, tabs stay view-only.
pub const MAX_EDIT_BYTES: usize = 16 * 1024 * 1024;

pub enum Loaded {
    Doc(Arc<Document>),
    Binary { bytes: u64 },
}

pub fn load(path: &Path) -> std::io::Result<Loaded> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mapped = len > MAP_ABOVE_BYTES;
    let bytes = if mapped {
        // SAFETY: the map is read-only. If another process truncates the file
        // while it is open, reads past the new end fault — the same trade-off
        // every mmap-based viewer (less, ripgrep) accepts.
        Bytes::Mapped(unsafe { memmap2::Mmap::map(&file)? })
    } else {
        let mut data = Vec::with_capacity(len as usize);
        file.read_to_end(&mut data)?;
        Bytes::Owned(data)
    };
    // A NUL byte in the first 8KB is the usual "this is binary" heuristic.
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Ok(Loaded::Binary { bytes: len });
    }
    Ok(Loaded::Doc(Arc::new(Document::new(bytes, !mapped))))
}

/// The token the editor's highlighter resolves to a grammar: the extension
/// (`rs`, `py`), or the whole file name for extensionless files (`Makefile`).
pub fn language_token(path: &Path) -> String {
    match path.extension() {
        Some(ext) => ext.to_string_lossy().to_lowercase(),
        None => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "text".into()),
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}
