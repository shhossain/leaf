//! Watched folders: a folder added to the sidebar lists the text files inside
//! it, like a Notes folder. Listing a folder only reads directory entries;
//! files are peeked (first 2 KB) for the sidebar and read in full only when
//! opened. Folders are rescanned while shown, so new and changed files appear.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Stop listing after this many files (a home folder or a huge repo).
pub const MAX_FILES: usize = 5000;
/// How deep below the folder to look.
const MAX_DEPTH: usize = 4;
/// Build output and dependency folders: big, generated, never what you want to read.
const SKIP_DIRS: &[&str] = &[
    "node_modules", "target", "build", "dist", "vendor", "__pycache__", "Pods", "DerivedData", "venv", "out",
];
/// Folders macOS shows as single files (apps, bundles).
const PACKAGE_EXTS: &[&str] = &["app", "bundle", "framework", "xcassets", "xcodeproj", "photoslibrary", "musiclibrary"];
/// Known binary formats, skipped without opening them.
const BINARY_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "heic", "tiff", "bmp", "ico", "icns", "psd", "sketch", "pdf", "zip", "gz", "tgz",
    "bz2", "xz", "7z", "rar", "dmg", "pkg", "iso", "exe", "dll", "so", "dylib", "a", "o", "class", "jar", "wasm", "mp3",
    "mp4", "mov", "m4a", "aac", "wav", "flac", "avi", "mkv", "ttf", "otf", "woff", "woff2", "sqlite", "db", "bin", "pyc",
    "rlib", "rmeta", "docx", "xlsx", "pptx", "pages", "numbers", "key",
];

#[derive(Clone, Debug, PartialEq)]
pub struct Scanned {
    pub path: PathBuf,
    pub modified: Option<SystemTime>,
    pub len: u64,
}

pub struct Scan {
    pub files: Vec<Scanned>,
    /// More than `MAX_FILES` were found; the rest are not listed.
    pub truncated: bool,
}

/// Lists the files under `root` that may be text. Hidden files and folders,
/// symlinks, build folders and known binary formats are skipped.
pub fn scan(root: &Path) -> Scan {
    let mut files = Vec::new();
    let mut truncated = false;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let Ok(kind) = entry.file_type() else { continue }; // doesn't follow symlinks
            let path = entry.path();
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if kind.is_dir() {
                if depth < MAX_DEPTH && !SKIP_DIRS.contains(&name.as_ref()) && !PACKAGE_EXTS.contains(&ext.as_str()) {
                    stack.push((path, depth + 1));
                }
            } else if kind.is_file() && !BINARY_EXTS.contains(&ext.as_str()) {
                if files.len() == MAX_FILES {
                    truncated = true;
                    break;
                }
                let meta = entry.metadata().ok();
                files.push(Scanned {
                    path,
                    modified: meta.as_ref().and_then(|m| m.modified().ok()),
                    len: meta.map_or(0, |m| m.len()),
                });
            }
        }
        if truncated {
            break;
        }
    }
    Scan { files, truncated }
}

/// Where a file sits inside the watched folder, for its sidebar row:
/// the folder's own name for top-level files, else `sub/dir`.
pub fn subfolder(root: &Path, path: &Path) -> String {
    let parent = path.parent().unwrap_or(root);
    match parent.strip_prefix(root) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().into_owned(),
        _ => root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_text_files_and_skips_the_rest() {
        let root = std::env::temp_dir().join(format!("leaf-folder-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["notes/deep", "node_modules/pkg", ".git", "Thing.app"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in ["a.md", "notes/b.txt", "notes/deep/c.rs", "photo.PNG", ".hidden", "node_modules/pkg/x.js", ".git/HEAD", "Thing.app/y"] {
            std::fs::write(root.join(file), "text").unwrap();
        }
        let mut found: Vec<String> = scan(&root)
            .files
            .iter()
            .map(|f| f.path.strip_prefix(&root).unwrap().to_string_lossy().into_owned())
            .collect();
        found.sort();
        assert_eq!(found, ["a.md", "notes/b.txt", "notes/deep/c.rs"]);
        assert_eq!(subfolder(&root, &root.join("notes/deep/c.rs")), "notes/deep");
        assert_eq!(subfolder(&root, &root.join("a.md")), root.file_name().unwrap().to_string_lossy());
        std::fs::remove_dir_all(root).unwrap();
    }
}
