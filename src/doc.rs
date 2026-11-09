//! Reading files from disk. Runs on a background thread, never on the UI thread.

use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use crate::encoding::{self, TextFormat};
use crate::text_index::{Bytes, Document};

/// Files above this are memory-mapped instead of read into memory.
pub const MAP_ABOVE_BYTES: u64 = 64 * 1024 * 1024;
/// The full editor holds ~12× the file size; above this, tabs stay view-only.
pub const MAX_EDIT_BYTES: usize = 16 * 1024 * 1024;

/// What the file looked like on disk when we last read or wrote it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiskStamp {
    modified: Option<SystemTime>,
    len: u64,
}

/// `None` if the file no longer exists.
pub fn stamp(path: &Path) -> Option<DiskStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(DiskStamp { modified: meta.modified().ok(), len: meta.len() })
}

pub enum Loaded {
    Doc { doc: Arc<Document>, format: TextFormat, stamp: DiskStamp },
    Binary { bytes: u64 },
}

pub fn load(path: &Path) -> std::io::Result<Loaded> {
    let mut file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    let stamp = DiskStamp { modified: meta.modified().ok(), len: meta.len() };
    let looks_binary = |bytes: &[u8]| {
        // A NUL byte in the first 8KB is the usual heuristic; UTF-16 (with a BOM) is full of them.
        !encoding::has_bom(bytes) && bytes[..bytes.len().min(8192)].contains(&0)
    };

    if meta.len() > MAP_ABOVE_BYTES {
        // SAFETY: the map is read-only. If another process truncates the file
        // while it is open, reads past the new end fault — the same trade-off
        // every mmap-based viewer (less, ripgrep) accepts. Huge files are
        // shown as (lossy) UTF-8 without encoding detection.
        let bytes = Bytes::Mapped(unsafe { memmap2::Mmap::map(&file)? });
        if looks_binary(&bytes) {
            return Ok(Loaded::Binary { bytes: meta.len() });
        }
        let doc = Document::new(bytes, false);
        let format = TextFormat { crlf: doc.crlf, ..TextFormat::default() };
        return Ok(Loaded::Doc { doc: Arc::new(doc), format, stamp });
    }

    let mut data = Vec::with_capacity(meta.len() as usize);
    file.read_to_end(&mut data)?;
    if looks_binary(&data) {
        return Ok(Loaded::Binary { bytes: meta.len() });
    }
    let decoded = encoding::decode(data);
    let mut doc = Document::new(Bytes::Owned(decoded.utf8), false);
    doc.lossy = decoded.lossy;
    let format = TextFormat { encoding: decoded.encoding, bom: decoded.bom, crlf: doc.crlf };
    Ok(Loaded::Doc { doc: Arc::new(doc), format, stamp })
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

/// Does the file indent with tabs? Decides what the Tab key inserts when editing.
pub fn indents_with_tabs(doc: &Document) -> bool {
    let head = &doc.bytes()[..doc.len().min(64 * 1024)];
    let tabbed = memchr::memmem::find_iter(head, b"\n\t").count();
    let spaced = memchr::memmem::find_iter(head, b"\n  ").count();
    tabbed > spaced || head.first() == Some(&b'\t')
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
