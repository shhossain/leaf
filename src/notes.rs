//! Notes-style files: ⌘N creates a Markdown file in `~/Documents/Leaf Notes`,
//! which is saved automatically and named after its first line.

use std::path::{Path, PathBuf};

pub const DEFAULT_NAME: &str = "New Note";
const EXTENSION: &str = "md";
const MAX_NAME_CHARS: usize = 60;

pub fn notes_dir() -> Option<PathBuf> {
    Some(dirs::document_dir()?.join("Leaf Notes"))
}

/// Files in the notes folder get auto-save and auto-naming.
pub fn is_note(path: &Path) -> bool {
    notes_dir().is_some_and(|dir| path.parent() == Some(dir.as_path()))
}

/// Creates an empty `New Note.md` (or `New Note 2.md`, …) and returns its path.
pub fn create_new_note() -> std::io::Result<PathBuf> {
    let dir = notes_dir().ok_or_else(|| std::io::Error::other("no Documents folder"))?;
    std::fs::create_dir_all(&dir)?;
    let path = free_path(&dir, DEFAULT_NAME);
    std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
    Ok(path)
}

/// A file name from the note's first line, like Notes' titles: Markdown
/// markers stripped, characters Finder dislikes removed, length capped.
pub fn name_from_text(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = line.trim_start_matches(['#', '-', '*', '>', '+']).trim();
    let cleaned: String = line
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { ' ' } else { c })
        .collect();
    let mut name = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((cut, _)) = name.char_indices().nth(MAX_NAME_CHARS) {
        name.truncate(cut);
    }
    let name = name.trim_end_matches(['.', ' ']).to_string();
    if name.is_empty() { DEFAULT_NAME.into() } else { name }
}

/// Where a note should move so its name matches `stem`, or `None` if it
/// already does (as `stem.md` or a numbered `stem N.md`).
pub fn renamed_path(current: &Path, stem: &str) -> Option<PathBuf> {
    let current_stem = current.file_stem()?.to_string_lossy();
    let numbered = current_stem
        .strip_prefix(stem)
        .and_then(|rest| rest.strip_prefix(' '))
        .is_some_and(|n| n.parse::<u32>().is_ok());
    if current_stem == stem || numbered {
        return None;
    }
    Some(free_path(current.parent()?, stem))
}

/// `dir/stem.md`, or `dir/stem 2.md`, `dir/stem 3.md`, … if taken.
fn free_path(dir: &Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{EXTENSION}"));
    if !first.exists() {
        return first;
    }
    (2..).map(|n| dir.join(format!("{stem} {n}.{EXTENSION}"))).find(|p| !p.exists()).expect("unbounded")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_the_first_line() {
        assert_eq!(name_from_text("\n\n# Shopping list\n- milk"), "Shopping list");
        assert_eq!(name_from_text("Plans: Q4/Q1?  draft"), "Plans Q4 Q1 draft");
        assert_eq!(name_from_text("   \n"), DEFAULT_NAME);
        assert_eq!(name_from_text("ends with dots..."), "ends with dots");
        assert_eq!(name_from_text(&"x".repeat(100)).chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn renames_only_when_the_title_changed() {
        let dir = std::env::temp_dir().join(format!("leaf-notes-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let note = dir.join("New Note.md");
        std::fs::write(&note, "").unwrap();
        assert_eq!(renamed_path(&note, "New Note"), None);
        assert_eq!(renamed_path(&dir.join("Ideas 2.md"), "Ideas"), None);
        assert_eq!(renamed_path(&note, "Ideas"), Some(dir.join("Ideas.md")));
        std::fs::write(dir.join("Ideas.md"), "").unwrap();
        assert_eq!(renamed_path(&note, "Ideas"), Some(dir.join("Ideas 2.md")));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
