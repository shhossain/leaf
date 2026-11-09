//! A read-only document: the file's bytes plus a sparse index of line starts.
//!
//! The index stores the start of every `STRIDE`th line, so a 1GB log with 20M
//! lines costs ~2.5MB of index instead of 160MB. Finding any line is one
//! lookup plus at most `STRIDE` newline searches.
//!
//! Small files are held in memory. Big ones stay on disk and are read in
//! blocks on demand ([`BlockFile`]); the index can be extended in place when
//! such a file grows, which is how logs are tailed.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fs::File;
use std::ops::Range;
use std::os::unix::fs::FileExt as _;
use std::sync::{Arc, Mutex};

const STRIDE: usize = 64;
/// Bytes compared to tell "text was appended" from "file was rewritten".
const TAIL_SAMPLE: usize = 64;
const BLOCK: usize = 256 * 1024;
/// Blocks kept in memory per file (16 MB).
const CACHED_BLOCKS: usize = 64;
/// Sequential read size when indexing or searching a file on disk.
pub const SCAN_CHUNK: usize = 4 * 1024 * 1024;
/// Visible tab width in columns.
pub const TAB_WIDTH: usize = 4;
/// Lines longer than this are cut for display (minified JSON, logs).
pub const MAX_DISPLAY_BYTES: usize = 4_000;

pub enum Bytes {
    Owned(Vec<u8>),
    File(BlockFile),
}

/// A file read on demand in 256 KB blocks, at most 16 MB of them cached.
/// Unlike a memory map, a file that shrinks underneath us (log rotation,
/// `: > file`) just reads short — the missing tail shows as empty — instead
/// of killing the process with SIGBUS.
pub struct BlockFile {
    file: File,
    /// Length when indexed; reads never go past it.
    len: usize,
    cache: Mutex<VecDeque<(usize, Arc<[u8]>)>>,
}

impl BlockFile {
    pub fn new(file: File, len: usize) -> Self {
        Self { file, len, cache: Mutex::new(VecDeque::with_capacity(CACHED_BLOCKS)) }
    }

    fn read_into(&self, buf: &mut [u8], offset: usize) -> usize {
        let mut filled = 0;
        while filled < buf.len() {
            match self.file.read_at(&mut buf[filled..], (offset + filled) as u64) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        filled
    }

    fn block(&self, ix: usize) -> Arc<[u8]> {
        let lock = || self.cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        {
            let mut cache = lock();
            if let Some(pos) = cache.iter().position(|(b, _)| *b == ix) {
                let entry = cache.remove(pos).expect("found");
                let data = entry.1.clone();
                cache.push_front(entry);
                return data;
            }
        }
        let start = ix * BLOCK;
        let mut buf = vec![0; BLOCK.min(self.len.saturating_sub(start))];
        let filled = self.read_into(&mut buf, start);
        buf.truncate(filled);
        let data: Arc<[u8]> = buf.into();
        let mut cache = lock();
        cache.push_front((ix, data.clone()));
        cache.truncate(CACHED_BLOCKS);
        data
    }

    fn read(&self, range: Range<usize>) -> Vec<u8> {
        let mut out = Vec::with_capacity(range.len());
        let mut pos = range.start;
        while pos < range.end {
            let block = self.block(pos / BLOCK);
            let offset = pos % BLOCK;
            if offset >= block.len() {
                break; // the file shrank
            }
            let take = (range.end - pos).min(block.len() - offset);
            out.extend_from_slice(&block[offset..offset + take]);
            pos += take;
        }
        out
    }
}

impl Bytes {
    fn len(&self) -> usize {
        match self {
            Bytes::Owned(v) => v.len(),
            Bytes::File(f) => f.len,
        }
    }

    /// Feeds `from..len` to `f` in large sequential chunks (no caching).
    fn scan(&self, from: usize, mut f: impl FnMut(&[u8])) {
        match self {
            Bytes::Owned(v) => f(&v[from.min(v.len())..]),
            Bytes::File(file) => {
                let mut buf = vec![0; SCAN_CHUNK];
                let mut pos = from;
                while pos < file.len {
                    let want = SCAN_CHUNK.min(file.len - pos);
                    let got = file.read_into(&mut buf[..want], pos);
                    if got == 0 {
                        break;
                    }
                    f(&buf[..got]);
                    pos += got;
                }
            }
        }
    }
}

/// Line bookkeeping, resumable so a growing file is only scanned once.
#[derive(Clone)]
struct LineIndex {
    /// Byte offset where line `k * STRIDE` starts.
    checkpoints: Vec<usize>,
    /// Newlines seen so far (= index of the last, unfinished line).
    newlines: usize,
    /// Where the last line starts.
    last_start: usize,
    longest_line: usize,
    longest_len: usize,
    crlf: usize,
    previous_byte: u8,
    scanned: usize,
    /// The last few bytes indexed; a file that only grew still ends with them.
    tail: Vec<u8>,
}

impl LineIndex {
    fn new() -> Self {
        Self {
            checkpoints: vec![0],
            newlines: 0,
            last_start: 0,
            longest_line: 0,
            longest_len: 0,
            crlf: 0,
            previous_byte: 0,
            scanned: 0,
            tail: Vec::new(),
        }
    }

    fn feed(&mut self, chunk: &[u8]) {
        for p in memchr::memchr_iter(b'\n', chunk) {
            let nl = self.scanned + p;
            let before = if p > 0 { chunk[p - 1] } else { self.previous_byte };
            if before == b'\r' {
                self.crlf += 1;
            }
            if nl - self.last_start > self.longest_len {
                (self.longest_line, self.longest_len) = (self.newlines, nl - self.last_start);
            }
            self.newlines += 1;
            self.last_start = nl + 1;
            if self.newlines % STRIDE == 0 {
                self.checkpoints.push(self.last_start);
            }
        }
        if let Some(&last) = chunk.last() {
            self.previous_byte = last;
        }
        self.scanned += chunk.len();
        let keep = TAIL_SAMPLE.min(chunk.len());
        self.tail.extend_from_slice(&chunk[chunk.len() - keep..]);
        let excess = self.tail.len().saturating_sub(TAIL_SAMPLE);
        self.tail.drain(..excess);
    }
}

pub struct Document {
    bytes: Bytes,
    index: LineIndex,
    /// Every line ends in `\r\n`; saving converts new lines back to CRLF.
    pub crlf: bool,
    /// Some lines end in `\r\n` and others in `\n`; saving leaves them as they are.
    pub mixed_eol: bool,
    pub lossy: bool,
}

impl Document {
    pub fn new(bytes: Bytes, check_utf8: bool) -> Self {
        let mut index = LineIndex::new();
        bytes.scan(0, |chunk| index.feed(chunk));
        let lossy = check_utf8 && matches!(&bytes, Bytes::Owned(v) if std::str::from_utf8(v).is_err());
        Self::from_parts(bytes, index, lossy)
    }

    /// A document read from disk on demand rather than held in memory.
    pub fn open_file(file: File, len: usize) -> Self {
        Self::new(Bytes::File(BlockFile::new(file, len)), false)
    }

    fn from_parts(bytes: Bytes, index: LineIndex, lossy: bool) -> Self {
        // Judge line endings over the whole file: a file that starts CRLF and
        // continues LF must not be converted wholesale on save.
        let crlf = index.crlf > 0 && index.crlf == index.newlines;
        let mixed_eol = index.crlf > 0 && index.crlf < index.newlines;
        Self { bytes, index, crlf, mixed_eol, lossy }
    }

    /// The same file after it grew to `new_len` (a log being written to),
    /// scanning only the new bytes. `None` for in-memory documents.
    pub fn extended(&self, new_len: usize) -> Option<Self> {
        let Bytes::File(file) = &self.bytes else { return None };
        if new_len <= file.len {
            return None;
        }
        let bytes = Bytes::File(BlockFile::new(file.file.try_clone().ok()?, new_len));
        let mut index = self.index.clone();
        bytes.scan(index.scanned, |chunk| index.feed(chunk));
        Some(Self::from_parts(bytes, index, self.lossy))
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// The document's last bytes as indexed (up to 64). If the file on disk
    /// still has these at the same place, it was only appended to.
    pub fn tail_sample(&self) -> &[u8] {
        &self.index.tail
    }

    #[cfg(test)]
    pub fn is_on_disk(&self) -> bool {
        matches!(self.bytes, Bytes::File(_))
    }

    /// The whole text, if it's held in memory.
    pub fn as_slice(&self) -> Option<&[u8]> {
        match &self.bytes {
            Bytes::Owned(v) => Some(v),
            Bytes::File(_) => None,
        }
    }

    /// Bytes in `range` (clamped; shorter if a file on disk shrank).
    pub fn slice(&self, range: Range<usize>) -> Cow<'_, [u8]> {
        let range = range.start.min(self.len())..range.end.min(self.len());
        match &self.bytes {
            Bytes::Owned(v) => Cow::Borrowed(&v[range]),
            Bytes::File(f) => Cow::Owned(f.read(range)),
        }
    }

    pub fn text(&self, range: Range<usize>) -> Cow<'_, str> {
        match self.slice(range) {
            Cow::Borrowed(bytes) => String::from_utf8_lossy(bytes),
            Cow::Owned(bytes) => Cow::Owned(String::from_utf8_lossy(&bytes).into_owned()),
        }
    }

    pub fn line_count(&self) -> usize {
        self.index.newlines + 1
    }

    /// Index of the longest line; the viewer sizes its horizontal scroll by it.
    pub fn longest_line(&self) -> usize {
        if self.len() - self.index.last_start > self.index.longest_len {
            self.index.newlines
        } else {
            self.index.longest_line
        }
    }

    /// Position of the next `\n` at or after `from`, or the document length.
    fn next_newline(&self, from: usize) -> usize {
        let len = self.len();
        match &self.bytes {
            Bytes::Owned(v) => memchr::memchr(b'\n', &v[from.min(len)..]).map_or(len, |p| from + p),
            Bytes::File(f) => {
                let mut pos = from;
                while pos < len {
                    let block = f.block(pos / BLOCK);
                    let offset = pos % BLOCK;
                    if offset >= block.len() {
                        return len;
                    }
                    if let Some(p) = memchr::memchr(b'\n', &block[offset..]) {
                        return pos + p;
                    }
                    pos += block.len() - offset;
                }
                len
            }
        }
    }

    fn byte_at(&self, at: usize) -> Option<u8> {
        self.slice(at..at + 1).first().copied()
    }

    /// Byte ranges of `lines`, each without its `\n` / `\r\n`.
    pub fn line_ranges(&self, lines: Range<usize>) -> Vec<Range<usize>> {
        let len = self.len();
        let end_line = lines.end.min(self.line_count());
        if lines.start >= end_line {
            return Vec::new();
        }
        let mut start = self.index.checkpoints[lines.start / STRIDE];
        for _ in 0..lines.start % STRIDE {
            start = (self.next_newline(start) + 1).min(len);
        }
        let mut out = Vec::with_capacity(end_line - lines.start);
        for _ in lines.start..end_line {
            let nl = self.next_newline(start);
            let end = if nl > start && self.byte_at(nl - 1) == Some(b'\r') { nl - 1 } else { nl };
            out.push(start..end);
            start = (nl + 1).min(len);
        }
        out
    }

    pub fn line_range(&self, line: usize) -> Range<usize> {
        self.line_ranges(line..line + 1).pop().unwrap_or(self.len()..self.len())
    }

    /// The line containing byte `offset`.
    pub fn line_of_offset(&self, offset: usize) -> usize {
        let offset = offset.min(self.len());
        let k = self.index.checkpoints.partition_point(|&s| s <= offset).saturating_sub(1);
        let from = self.index.checkpoints[k];
        k * STRIDE + memchr::memchr_iter(b'\n', &self.slice(from..offset)).count()
    }
}

/// A line as drawn: tabs expanded, invalid UTF-8 replaced, very long lines cut.
pub struct DisplayLine {
    pub text: String,
    /// `(source index, display index, width)` of each expanded tab.
    tabs: Vec<(usize, usize, usize)>,
    source_len: usize,
}

impl DisplayLine {
    pub fn new(raw: &[u8]) -> Self {
        let decoded = String::from_utf8_lossy(raw);
        let mut text = String::with_capacity(decoded.len().min(MAX_DISPLAY_BYTES + 4));
        let mut tabs = Vec::new();
        let mut column = 0;
        for (ix, ch) in decoded.char_indices() {
            if text.len() >= MAX_DISPLAY_BYTES {
                text.push('…');
                break;
            }
            if ch == '\t' {
                let width = TAB_WIDTH - column % TAB_WIDTH;
                tabs.push((ix, text.len(), width));
                text.extend(std::iter::repeat_n(' ', width));
                column += width;
            } else {
                text.push(ch);
                column += 1;
            }
        }
        Self { text, tabs, source_len: decoded.len() }
    }

    pub fn source_to_display(&self, source: usize) -> usize {
        let extra: usize = self.tabs.iter().take_while(|t| t.0 < source).map(|t| t.2 - 1).sum();
        (source + extra).min(self.text.len())
    }

    pub fn display_to_source(&self, display: usize) -> usize {
        let mut extra = 0;
        for &(src, disp, width) in &self.tabs {
            if display < disp {
                break;
            }
            if display < disp + width {
                // Inside an expanded tab: snap to whichever edge is nearer.
                return if display - disp > width / 2 { src + 1 } else { src };
            }
            extra += width - 1;
        }
        (display - extra).min(self.source_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Document {
        Document::new(Bytes::Owned(text.as_bytes().to_vec()), true)
    }

    #[test]
    fn counts_lines_like_an_editor() {
        assert_eq!(doc("").line_count(), 1);
        assert_eq!(doc("a").line_count(), 1);
        assert_eq!(doc("a\nb\n").line_count(), 3);
    }

    #[test]
    fn finds_lines_across_checkpoints() {
        let text: String = (0..1000).map(|i| format!("line {i}\r\n")).collect();
        let d = doc(&text);
        assert_eq!(d.line_count(), 1001);
        for line in [0, 1, 63, 64, 65, 500, 999] {
            assert_eq!(d.text(d.line_range(line)), format!("line {line}"));
        }
        let ranges = d.line_ranges(62..67);
        assert_eq!(ranges.len(), 5);
        assert_eq!(d.text(ranges[4].clone()), "line 66");
        assert_eq!(d.line_range(1000), d.len()..d.len());
        assert!(d.line_ranges(2000..2010).is_empty());
    }

    #[test]
    fn maps_offsets_back_to_lines() {
        let text: String = (0..300).map(|i| format!("{i}\n")).collect();
        let d = doc(&text);
        for line in [0, 63, 64, 128, 299] {
            let r = d.line_range(line);
            assert_eq!(d.line_of_offset(r.start), line);
            assert_eq!(d.line_of_offset(r.end), line);
        }
    }

    #[test]
    fn line_endings_are_judged_over_the_whole_file() {
        assert!(doc("a\r\nb\r\n").crlf);
        let mixed = doc("a\r\nb\nc\n");
        assert!(!mixed.crlf && mixed.mixed_eol);
        assert!(!doc("a\nb\n").crlf);
    }

    fn file_doc(dir: &std::path::Path, text: &[u8]) -> (std::path::PathBuf, Document) {
        let path = dir.join("doc.txt");
        std::fs::write(&path, text).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        (path.clone(), Document::open_file(file, text.len()))
    }

    #[test]
    fn on_disk_documents_match_in_memory_ones() {
        let dir = std::env::temp_dir().join(format!("leaf-ti-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Long enough to cross several 256 KB blocks and STRIDE checkpoints.
        let text: String = (0..60_000).map(|i| format!("line number {i}\r\n")).collect();
        let (_, on_disk) = file_doc(&dir, text.as_bytes());
        let in_memory = doc(&text);
        assert_eq!(on_disk.line_count(), in_memory.line_count());
        assert!(on_disk.crlf && on_disk.is_on_disk());
        for line in [0, 1, 63, 64, 20_000, 59_999, 60_000] {
            assert_eq!(on_disk.line_range(line), in_memory.line_range(line), "line {line}");
            assert_eq!(on_disk.text(on_disk.line_range(line)), in_memory.text(in_memory.line_range(line)));
        }
        let mid = in_memory.line_range(30_000).start + 3;
        assert_eq!(on_disk.line_of_offset(mid), 30_000);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn growing_files_extend_and_shrinking_files_dont_crash() {
        let dir = std::env::temp_dir().join(format!("leaf-ti-grow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (path, first) = file_doc(&dir, b"one\ntwo\n");
        assert_eq!(first.line_count(), 3);
        std::fs::write(&path, b"one\ntwo\nthree\nfour").unwrap();
        let grown = first.extended(18).expect("grew");
        assert_eq!(grown.line_count(), 4);
        assert_eq!(grown.text(grown.line_range(3)), "four");
        // Truncated underneath us: reads of uncached blocks come back short
        // (cached ones keep their old bytes) — never a SIGBUS like a mmap.
        std::fs::write(&path, b"").unwrap();
        let fresh = Document::open_file(std::fs::File::open(&path).unwrap(), 18);
        assert_eq!(fresh.text(fresh.line_range(0)), "");
        assert_eq!(grown.line_ranges(0..4).len(), 4);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tracks_longest_line() {
        assert_eq!(doc("a\nlongest\nmid\n").longest_line(), 1);
        assert_eq!(doc("a\nb\nfinal line").longest_line(), 2);
    }

    #[test]
    fn expands_tabs_and_maps_both_ways() {
        let line = DisplayLine::new(b"\tab\tc");
        assert_eq!(line.text, "    ab  c");
        assert_eq!(line.source_to_display(0), 0);
        assert_eq!(line.source_to_display(1), 4);
        assert_eq!(line.source_to_display(4), 8);
        assert_eq!(line.display_to_source(4), 1);
        assert_eq!(line.display_to_source(8), 4);
        assert_eq!(line.display_to_source(1), 0);
        assert_eq!(line.display_to_source(3), 1);
    }

    #[test]
    fn cuts_very_long_lines() {
        let raw = vec![b'x'; MAX_DISPLAY_BYTES * 3];
        let line = DisplayLine::new(&raw);
        assert!(line.text.ends_with('…'));
        assert!(line.text.len() <= MAX_DISPLAY_BYTES + 4);
    }
}
