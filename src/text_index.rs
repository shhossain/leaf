//! A read-only document: the file's bytes plus a sparse index of line starts.
//!
//! The index stores the start of every `STRIDE`th line, so a 1GB log with 20M
//! lines costs ~2.5MB of index instead of 160MB. Finding any line is one
//! binary-search-free lookup plus at most `STRIDE` `memchr` calls.

use std::borrow::Cow;
use std::ops::Range;

const STRIDE: usize = 64;
/// Visible tab width in columns.
pub const TAB_WIDTH: usize = 4;
/// Lines longer than this are cut for display (minified JSON, logs).
pub const MAX_DISPLAY_BYTES: usize = 4_000;

pub enum Bytes {
    Owned(Vec<u8>),
    Mapped(memmap2::Mmap),
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Owned(v) => v,
            Bytes::Mapped(m) => m,
        }
    }
}

pub struct Document {
    bytes: Bytes,
    /// Byte offset where line `k * STRIDE` starts.
    checkpoints: Vec<usize>,
    line_count: usize,
    longest_line: usize,
    /// Every line ends in `\r\n`; saving converts new lines back to CRLF.
    pub crlf: bool,
    /// Some lines end in `\r\n` and others in `\n`; saving leaves them as they are.
    pub mixed_eol: bool,
    pub lossy: bool,
}

impl Document {
    pub fn new(bytes: Bytes, check_utf8: bool) -> Self {
        let data: &[u8] = &bytes;
        let mut checkpoints = vec![0];
        let (mut line, mut start) = (0usize, 0usize);
        let (mut longest_line, mut longest_len) = (0usize, 0usize);
        for nl in memchr::memchr_iter(b'\n', data) {
            if nl - start > longest_len {
                (longest_line, longest_len) = (line, nl - start);
            }
            line += 1;
            start = nl + 1;
            if line % STRIDE == 0 {
                checkpoints.push(start);
            }
        }
        if data.len() - start > longest_len {
            longest_line = line;
        }
        // Judge line endings over the whole file: a file that starts CRLF and
        // continues LF must not be converted wholesale on save.
        let crlf_count = memchr::memmem::find_iter(data, b"\r\n").count();
        let crlf = crlf_count > 0 && crlf_count == line;
        let mixed_eol = crlf_count > 0 && crlf_count < line;
        let lossy = check_utf8 && std::str::from_utf8(data).is_err();
        Self { bytes, checkpoints, line_count: line + 1, longest_line, crlf, mixed_eol, lossy }
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn line_count(&self) -> usize {
        self.line_count
    }

    /// Index of the longest line; the viewer sizes its horizontal scroll by it.
    pub fn longest_line(&self) -> usize {
        self.longest_line
    }

    /// Byte ranges of `lines`, each without its `\n` / `\r\n`.
    pub fn line_ranges(&self, lines: Range<usize>) -> Vec<Range<usize>> {
        let data: &[u8] = &self.bytes;
        let end_line = lines.end.min(self.line_count);
        if lines.start >= end_line {
            return Vec::new();
        }
        let next_start = |from: usize| memchr::memchr(b'\n', &data[from..]).map_or(data.len(), |p| from + p + 1);
        let mut start = self.checkpoints[lines.start / STRIDE];
        for _ in 0..lines.start % STRIDE {
            start = next_start(start);
        }
        let mut out = Vec::with_capacity(end_line - lines.start);
        for _ in lines.start..end_line {
            let nl = memchr::memchr(b'\n', &data[start..]).map_or(data.len(), |p| start + p);
            let end = if nl > start && data[nl - 1] == b'\r' { nl - 1 } else { nl };
            out.push(start..end);
            start = (nl + 1).min(data.len());
        }
        out
    }

    pub fn line_range(&self, line: usize) -> Range<usize> {
        self.line_ranges(line..line + 1).pop().unwrap_or(self.len()..self.len())
    }

    /// The line containing byte `offset`.
    pub fn line_of_offset(&self, offset: usize) -> usize {
        let offset = offset.min(self.len());
        let k = self.checkpoints.partition_point(|&s| s <= offset).saturating_sub(1);
        let from = self.checkpoints[k];
        k * STRIDE + memchr::memchr_iter(b'\n', &self.bytes[from..offset]).count()
    }

    pub fn text(&self, range: Range<usize>) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.bytes[range])
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
