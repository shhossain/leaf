//! Leaf: a lightweight, viewer-first text editor for macOS with lazy tabs.

// The syntax palette in style.rs is one `serde_json::json!` table.
#![recursion_limit = "256"]

mod board;
mod doc;
mod encoding;
mod file_list;
mod folders;
mod highlight;
mod notes;
mod recovery;
mod session;
mod style;
mod text_index;
mod viewer;
mod workspace;

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;

use futures::{StreamExt as _, channel::mpsc};
use gpui_kit::*;

use workspace::Workspace;

actions!(
    leaf,
    [Quit, OpenFile, Save, CloseTab, ReopenClosed, ToggleEdit, NextTab, PrevTab, ToggleSidebar, FocusSearch, NewNote, NewBoard, CompareWithPrevious, AddFolder, TogglePin, DailyNote, RevealInFinder, CopyPath,
        ToggleSolidBackground, CompareWithCommit]
);

/// ⌘1–⌘8 open the file at that position in the sidebar; ⌘9 opens the last
/// one (`usize::MAX`), as in browsers.
#[derive(Action, Clone, PartialEq, serde::Deserialize)]
#[action(namespace = leaf, no_json)]
pub struct SelectTab(pub usize);

fn main() {
    let app = gpui_kit::application().with_assets(LeafAssets);

    // Finder "Open With", double-clicks, and drops on the Dock icon arrive as
    // file:// URLs — possibly before the window exists, so queue them.
    let (open_tx, mut open_rx) = mpsc::unbounded::<Vec<PathBuf>>();
    app.on_open_urls(move |urls| {
        let paths: Vec<PathBuf> = urls.iter().filter_map(|url| file_url_to_path(url).or_else(|| leaf_url_to_path(url))).collect();
        if !paths.is_empty() {
            let _ = open_tx.unbounded_send(paths);
        }
    });

    let cli_paths = cli_paths(std::env::args_os().skip(1));

    app.run(move |cx| {
        gpui_kit::init(cx);
        workspace::init(cx);
        viewer::init(cx);
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.set_menus(menus());

        let bounds = Bounds::centered(None, size(px(1100.), px(760.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            // The sidebar is translucent over this blur, like Notes.
            window_background: WindowBackgroundAppearance::Blurred,
            // Like Notes: no title bar; traffic lights sit inside the toolbar.
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: Some(point(px(18.), px(20.))),
            }),
            ..Default::default()
        };
        let (_, workspace) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Workspace::new(window, cx))
        })
        .expect("failed to open the main window");

        // Phase 0: one window; closing it quits. (Reopen-on-Dock-click comes later.)
        cx.on_window_closed(|cx, _| cx.quit()).detach();

        // Previous tabs come back cold (nothing is read until shown); files
        // passed on the command line open on top and become active.
        workspace.update(cx, |ws, cx| {
            ws.restore(session::load(), cx);
            ws.open_paths(cli_paths, cx);
        });
        cx.spawn(async move |cx| {
            while let Some(paths) = open_rx.next().await {
                cx.update(|cx| workspace.update(cx, |ws, cx| ws.open_paths(paths, cx)));
            }
        })
        .detach();

        cx.activate(true);
    });
}

// The component library's default icons plus the few extra Lucide icons Leaf
// uses, instead of embedding all ~1,800 (`AllAssets`, ~1 MB).
gpui_kit::assets::icon_assets!(ExtraIcons, [BookOpen, Check, Code, FileText, Folder, PanelLeft, Pencil, Search, SquarePen]);

struct LeafAssets;

impl AssetSource for LeafAssets {
    fn load(&self, path: &str) -> Result<Option<std::borrow::Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut all = gpui_kit::assets::Assets.list(path)?;
        all.extend(ExtraIcons.list(path)?);
        Ok(all)
    }
}

fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Leaf".into(),
            items: vec![MenuItem::action("Quit Leaf", Quit)],
            disabled: false,
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New Note", NewNote),
                MenuItem::action("New Board", NewBoard),
                MenuItem::action("Today's Note", DailyNote),
                MenuItem::action("Open…", OpenFile),
                MenuItem::action("Add Folder to Sidebar…", AddFolder),
                MenuItem::action("Quick Open…", workspace::QuickOpen),
                MenuItem::separator(),
                MenuItem::action("Pin / Unpin File", TogglePin),
                MenuItem::action("Reveal in Finder", RevealInFinder),
                MenuItem::action("Copy Path", CopyPath),
                MenuItem::action("Save", Save),
                MenuItem::separator(),
                MenuItem::action("Close Tab", CloseTab),
                MenuItem::action("Reopen Closed Tab", ReopenClosed),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Toggle Edit Mode", ToggleEdit),
                MenuItem::action("Page / Source Lines", viewer::TogglePreview),
                MenuItem::action("Show/Hide Sidebar", ToggleSidebar),
                MenuItem::action("Search in Sidebar", FocusSearch),
                MenuItem::separator(),
                MenuItem::action("Compare with Previous File  (or ⌥-click a file)", CompareWithPrevious),
                MenuItem::action("Compare with Last Commit", CompareWithCommit),
                MenuItem::action("Go to Heading…", workspace::GoToHeading),
                MenuItem::separator(),
                MenuItem::action("Translucent / Solid Window", ToggleSolidBackground),
                MenuItem::separator(),
                MenuItem::action("Next Tab", NextTab),
                MenuItem::action("Previous Tab", PrevTab),
            ],
            disabled: false,
        },
    ]
}

/// File arguments, without flags. macOS settings passed as arguments
/// (`-NSRequiresAquaSystemAppearance YES`) take a value, which is skipped too.
fn cli_paths(args: impl Iterator<Item = OsString>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut skip_value = false;
    for arg in args {
        let text = arg.to_string_lossy();
        if std::mem::take(&mut skip_value) {
            continue;
        }
        if text.starts_with('-') {
            skip_value = text.starts_with("-NS") || text.starts_with("-Apple");
            continue;
        }
        paths.push(PathBuf::from(arg));
    }
    paths
}

/// `leaf://open?path=%2Fa%2Fb.md&line=42` → `/a/b.md:42` (the `leaf` command).
fn leaf_url_to_path(url: &str) -> Option<PathBuf> {
    let query = url.strip_prefix("leaf://open?")?;
    let (mut path, mut line) = (None, None);
    for pair in query.split('&') {
        match pair.split_once('=') {
            Some(("path", value)) => path = Some(percent_decode(value)?),
            Some(("line", value)) => line = value.parse::<usize>().ok(),
            _ => {}
        }
    }
    let mut path = path?;
    if let Some(line) = line {
        path.extend_from_slice(format!(":{line}").as_bytes());
    }
    Some(PathBuf::from(OsString::from_vec(path)))
}

/// `file:///Users/me/My%20Notes.txt` → `/Users/me/My Notes.txt`.
fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    Some(PathBuf::from(OsString::from_vec(percent_decode(rest)?)))
}

fn percent_decode(rest: &str) -> Option<Vec<u8>> {
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::cli_paths;
    use std::path::PathBuf;

    #[test]
    fn cli_skips_flags_and_macos_settings() {
        let args = ["-NSRequiresAquaSystemAppearance", "YES", "a.txt", "--verbose", "b.md"].map(Into::into);
        assert_eq!(cli_paths(args.into_iter()), [PathBuf::from("a.txt"), PathBuf::from("b.md")]);
    }

    #[test]
    fn leaf_urls_carry_path_and_line() {
        assert_eq!(super::leaf_url_to_path("leaf://open?path=%2Fa%20b%2Fc.md&line=42"), Some(PathBuf::from("/a b/c.md:42")));
        assert_eq!(super::leaf_url_to_path("leaf://open?path=%2Fx.txt"), Some(PathBuf::from("/x.txt")));
        assert_eq!(super::file_url_to_path("file:///a%20b.txt"), Some(PathBuf::from("/a b.txt")));
    }
}
