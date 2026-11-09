//! What the Notes-style sidebar shows for each open file: modified date and
//! first line. Only the first 2KB of a file is read, so cold tabs stay cheap.

use std::io::Read as _;
use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, Datelike, Local};

use crate::encoding;

const PEEK_BYTES: usize = 2048;
const SNIPPET_CHARS: usize = 80;

#[derive(Clone, Debug, Default)]
pub struct FileMeta {
    pub modified: Option<SystemTime>,
    /// First non-empty line, Markdown heading marks stripped.
    pub snippet: String,
}

pub fn peek(path: &Path) -> FileMeta {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let mut buf = Vec::with_capacity(PEEK_BYTES);
    let read = std::fs::File::open(path).and_then(|f| f.take(PEEK_BYTES as u64).read_to_end(&mut buf));
    if read.is_err() {
        return FileMeta { modified, snippet: "Can't read file".into() };
    }
    if !encoding::has_bom(&buf) && buf.contains(&0) {
        return FileMeta { modified, snippet: "Binary file".into() };
    }
    // Don't let a character cut in half at the 2KB mark trigger encoding detection.
    if let Err(err) = std::str::from_utf8(&buf) {
        if err.error_len().is_none() {
            buf.truncate(err.valid_up_to());
        }
    }
    let text = String::from_utf8_lossy(&encoding::decode(buf).utf8).into_owned();
    FileMeta { modified, snippet: first_line(&text) }
}

fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .unwrap_or("No additional text");
    match line.char_indices().nth(SNIPPET_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

pub fn local(time: SystemTime) -> DateTime<Local> {
    DateTime::<Local>::from(time)
}

/// Section a file falls under, like Notes: Today, Yesterday, Previous 7 Days,
/// Previous 30 Days, then month names this year, then years.
pub fn group_label(modified: DateTime<Local>, now: DateTime<Local>) -> String {
    let days = (now.date_naive() - modified.date_naive()).num_days();
    match days {
        ..=0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=7 => "Previous 7 Days".into(),
        8..=30 => "Previous 30 Days".into(),
        _ if modified.year() == now.year() => modified.format("%B").to_string(),
        _ => modified.year().to_string(),
    }
}

/// Short date on a row: `3:12 PM`, `Yesterday`, `Wednesday`, or `10/26/26`.
pub fn row_date(modified: DateTime<Local>, now: DateTime<Local>) -> String {
    match (now.date_naive() - modified.date_naive()).num_days() {
        ..=0 => modified.format("%-I:%M %p").to_string(),
        1 => "Yesterday".into(),
        2..=6 => modified.format("%A").to_string(),
        _ => modified.format("%-m/%-d/%y").to_string(),
    }
}

/// Header above a prose file: `November 9, 2026 at 3:12 PM`.
pub fn full_date(modified: DateTime<Local>) -> String {
    modified.format("%B %-d, %Y at %-I:%M %p").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, 12, 0).unwrap()
    }

    #[test]
    fn groups_like_notes() {
        let now = at(2026, 11, 9, 15);
        assert_eq!(group_label(at(2026, 11, 9, 1), now), "Today");
        assert_eq!(group_label(at(2026, 11, 8, 23), now), "Yesterday");
        assert_eq!(group_label(at(2026, 11, 4, 1), now), "Previous 7 Days");
        assert_eq!(group_label(at(2026, 10, 26, 1), now), "Previous 30 Days");
        assert_eq!(group_label(at(2026, 3, 1, 1), now), "March");
        assert_eq!(group_label(at(2024, 3, 1, 1), now), "2024");
    }

    #[test]
    fn short_dates() {
        let now = at(2026, 11, 9, 15);
        assert_eq!(row_date(at(2026, 11, 9, 15), now), "3:12 PM");
        assert_eq!(row_date(at(2026, 11, 8, 9), now), "Yesterday");
        assert_eq!(row_date(at(2026, 11, 4, 9), now), "Wednesday");
        assert_eq!(row_date(at(2026, 10, 26, 9), now), "10/26/26");
        assert_eq!(full_date(at(2026, 11, 4, 1)), "November 4, 2026 at 1:12 AM");
    }

    #[test]
    fn snippet_is_first_meaningful_line() {
        assert_eq!(first_line("\n\n# Title here\nbody"), "Title here");
        assert_eq!(first_line("   \n"), "No additional text");
        assert!(first_line(&"x".repeat(200)).ends_with('…'));
    }
}
