//! Reading files from disk. Runs on a background thread, never on the UI thread.

use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use crate::encoding::{self, TextFormat};
use crate::text_index::{Bytes, Document};


/// Files above this are read from disk on demand instead of into memory.
pub const MAP_ABOVE_BYTES: u64 = 64 * 1024 * 1024;
/// The full editor holds ~12× the file size; above this, tabs stay view-only.
pub const MAX_EDIT_BYTES: usize = 16 * 1024 * 1024;

/// What the file looked like on disk when we last read or wrote it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiskStamp {
    modified: Option<SystemTime>,
    len: u64,
}

impl DiskStamp {
    pub fn len(&self) -> u64 {
        self.len
    }
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
        // Big files stay on disk and are read in blocks as needed. They are
        // shown as (lossy) UTF-8 without encoding detection.
        let doc = Document::open_file(file, meta.len() as usize);
        if looks_binary(&doc.slice(0..8192)) {
            return Ok(Loaded::Binary { bytes: meta.len() });
        }
        let format = TextFormat { crlf: doc.crlf, ..TextFormat::default() };
        return Ok(Loaded::Doc { doc: Arc::new(doc), format, stamp });
    }

    let mut data = Vec::with_capacity(meta.len() as usize);
    file.read_to_end(&mut data)?;
    if looks_binary(&data) {
        return Ok(Loaded::Binary { bytes: meta.len() });
    }
    // Plain UTF-8 needs no check; anything converted must survive a round trip.
    let original = (encoding::has_bom(&data) || std::str::from_utf8(&data).is_err()).then(|| data.clone());
    let decoded = encoding::decode(data);
    let mut lossy = decoded.lossy;
    if let (Some(original), false) = (original, lossy) {
        let text = std::str::from_utf8(&decoded.utf8).expect("decoder output is UTF-8");
        lossy = !encoding::round_trips(&original, text, decoded.encoding, decoded.bom);
    }
    let mut doc = Document::new(Bytes::Owned(decoded.utf8), false);
    doc.lossy = lossy;
    let format = TextFormat { encoding: decoded.encoding, bom: decoded.bom, crlf: doc.crlf };
    Ok(Loaded::Doc { doc: Arc::new(doc), format, stamp })
}

/// The file at `path` after it grew to `new_len`, if bytes were only added at
/// the end (a log being written). Only the new part is indexed. `None` means
/// the file changed some other way and must be reloaded.
pub fn appended(path: &Path, doc: &Document, new_len: usize, utf8: bool) -> Option<Document> {
    use std::os::unix::fs::FileExt as _;
    let file = std::fs::File::open(path).ok()?;
    let sample = doc.tail_sample();
    let mut on_disk = vec![0; sample.len()];
    file.read_exact_at(&mut on_disk, (doc.len() - sample.len()) as u64).ok()?;
    if on_disk != sample {
        return None;
    }
    match doc.extended(new_len) {
        Some(grown) => Some(grown),
        // An in-memory file that grew becomes an on-disk one (no copying);
        // only plain UTF-8 can be read that way.
        None if utf8 => Some(Document::open_file(file, new_len)),
        None => None,
    }
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
    let head = doc.slice(0..64 * 1024);
    let tabbed = memchr::memmem::find_iter(&head, b"\n\t").count();
    let spaced = memchr::memmem::find_iter(&head, b"\n  ").count();
    tabbed > spaced || head.first() == Some(&b'\t')
}

/// Replaces `path` with `bytes` without ever leaving a half-written file: the
/// data goes to a temporary file in the same folder, is flushed to disk, and
/// is renamed over the original. Symlinks and hard-linked files are written in
/// place instead, so the link (not a copy) keeps pointing at the new contents.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::MetadataExt as _;
    let link_meta = std::fs::symlink_metadata(path).ok();
    let in_place = link_meta.as_ref().is_some_and(|m| m.file_type().is_symlink() || m.nlink() > 1);
    let Some(dir) = path.parent().filter(|_| !in_place) else {
        return std::fs::write(path, bytes);
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.leaf-{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if let Some(meta) = &link_meta {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_files_and_keeps_symlinks() {
        let dir = std::env::temp_dir().join(format!("leaf-write-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "old").unwrap();
        write_atomic(&file, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        let link = dir.join("link.txt");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        write_atomic(&link, b"via link").unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "via link");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "no temp files left behind");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
