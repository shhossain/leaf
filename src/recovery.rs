//! Unsaved edits survive crashes, logout and quitting from the Dock: while a
//! file has unsaved changes its text is copied to
//! `~/Library/Application Support/Leaf/Recovery`, and offered back (as unsaved
//! changes) the next time the file is opened. Saving or discarding removes it.

use std::path::{Path, PathBuf};

fn dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("Leaf").join("Recovery"))
}

/// Stable name for a file's recovery copy (FNV-1a of the path).
fn key(path: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn files(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let dir = dir()?;
    let key = key(path);
    Some((dir.join(format!("{key}.txt")), dir.join(format!("{key}.path"))))
}

pub fn save(path: &Path, text: &str) -> std::io::Result<()> {
    let (text_file, path_file) = files(path).ok_or_else(|| std::io::Error::other("no data folder"))?;
    std::fs::create_dir_all(text_file.parent().expect("has a parent"))?;
    crate::doc::write_atomic(&text_file, text.as_bytes())?;
    // The original path, so a stray copy can be traced back by hand.
    std::fs::write(path_file, path.as_os_str().as_encoded_bytes())
}

pub fn load(path: &Path) -> Option<String> {
    let (text_file, path_file) = files(path)?;
    // Guard against hash collisions: the stored path must match.
    let stored = std::fs::read(path_file).ok()?;
    if stored != path.as_os_str().as_encoded_bytes() {
        return None;
    }
    std::fs::read_to_string(text_file).ok()
}

pub fn discard(path: &Path) {
    if let Some((text_file, path_file)) = files(path) {
        let _ = std::fs::remove_file(text_file);
        let _ = std::fs::remove_file(path_file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_stable_and_distinct() {
        assert_eq!(key(Path::new("/a/b.txt")), key(Path::new("/a/b.txt")));
        assert_ne!(key(Path::new("/a/b.txt")), key(Path::new("/a/c.txt")));
        assert_eq!(key(Path::new("")).len(), 16);
    }
}
