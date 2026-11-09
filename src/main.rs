//! Leaf: a lightweight, viewer-first text editor for macOS with lazy tabs.

mod doc;
mod encoding;
mod highlight;
mod session;
mod text_index;
mod viewer;
mod workspace;

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;

use futures::{StreamExt as _, channel::mpsc};
use gpui_kit::*;

use workspace::Workspace;

actions!(leaf, [Quit, OpenFile, Save, CloseTab, ReopenClosed, ToggleEdit, NextTab, PrevTab]);

fn main() {
    let app = gpui_kit::application().with_assets(gpui_kit::assets::Assets);

    // Finder "Open With", double-clicks, and drops on the Dock icon arrive as
    // file:// URLs — possibly before the window exists, so queue them.
    let (open_tx, mut open_rx) = mpsc::unbounded::<Vec<PathBuf>>();
    app.on_open_urls(move |urls| {
        let paths: Vec<PathBuf> = urls.iter().filter_map(|url| file_url_to_path(url)).collect();
        if !paths.is_empty() {
            let _ = open_tx.unbounded_send(paths);
        }
    });

    let cli_paths: Vec<PathBuf> = std::env::args_os()
        .skip(1)
        .filter(|arg| !arg.to_string_lossy().starts_with('-'))
        .map(PathBuf::from)
        .collect();

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
                MenuItem::action("Open…", OpenFile),
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
                MenuItem::action("Markdown Preview", viewer::TogglePreview),
                MenuItem::separator(),
                MenuItem::action("Next Tab", NextTab),
                MenuItem::action("Previous Tab", PrevTab),
            ],
            disabled: false,
        },
    ]
}

/// `file:///Users/me/My%20Notes.txt` → `/Users/me/My Notes.txt`.
fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
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
    Some(PathBuf::from(OsString::from_vec(out)))
}
