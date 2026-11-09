//! Open tabs persisted across launches, in
//! `~/Library/Application Support/Leaf/session.json`. Only paths and scroll
//! positions are stored; restored tabs stay cold until shown.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct Session {
    pub tabs: Vec<SessionTab>,
    pub active: Option<usize>,
    /// Watched folders shown in the sidebar.
    #[serde(default)]
    pub folders: Vec<PathBuf>,
    /// The folder the sidebar shows (`None`: open files).
    #[serde(default)]
    pub scope: Option<usize>,
    /// The shown file when it came from a watched folder (not in `tabs`).
    #[serde(default)]
    pub folder_file: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct SessionTab {
    pub path: PathBuf,
    #[serde(default)]
    pub top_line: usize,
}

fn session_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("Leaf").join("session.json"))
}

pub fn load() -> Session {
    session_path().map(|path| load_from(&path)).unwrap_or_default()
}

pub fn save(session: &Session) {
    if let Some(path) = session_path() {
        if let Err(err) = save_to(&path, session) {
            eprintln!("[leaf] could not save session: {err}");
        }
    }
}

fn load_from(path: &Path) -> Session {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Writes atomically (temp file + rename) so a crash never leaves half a file.
fn save_to(path: &Path, session: &Session) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(session)?)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_garbage() {
        let dir = std::env::temp_dir().join(format!("leaf-session-test-{}", std::process::id()));
        let path = dir.join("session.json");
        let session = Session {
            tabs: vec![SessionTab { path: "/tmp/a.txt".into(), top_line: 42 }],
            active: Some(0),
            folders: vec!["/tmp/notes".into()],
            scope: Some(0),
            folder_file: Some("/tmp/notes/a.md".into()),
        };
        save_to(&path, &session).unwrap();
        assert_eq!(load_from(&path), session);
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(load_from(&path), Session::default());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
