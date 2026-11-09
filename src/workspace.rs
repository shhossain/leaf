//! The window: a tab bar over exactly one visible document.
//!
//! Tabs are lazy. Opening 500 files only records 500 paths; a file is read
//! from disk the first time its tab is shown. Files open in the lightweight
//! [`Viewer`]; ⌘E swaps in the full editor for that tab only. At most
//! `MAX_LIVE` tabs stay in memory; older clean ones fall back to `Cold`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::component::{
    ActiveTheme as _, h_flex,
    input::{Editor, EditorState, InputEvent, Position},
    tab::{Tab, TabBar},
    theme::Theme,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::doc::{self, Loaded};
use crate::text_index::{Bytes, Document};
use crate::viewer::Viewer;
use crate::{CloseTab, NextTab, OpenFile, PrevTab, Save, ToggleEdit};

/// Tabs kept in memory at once. Everything else is just a path.
const MAX_LIVE: usize = 12;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-o", OpenFile, None),
        KeyBinding::new("cmd-e", ToggleEdit, Some("Workspace")),
        KeyBinding::new("cmd-s", Save, Some("Workspace")),
        KeyBinding::new("cmd-w", CloseTab, Some("Workspace")),
        KeyBinding::new("ctrl-tab", NextTab, Some("Workspace")),
        KeyBinding::new("ctrl-shift-tab", PrevTab, Some("Workspace")),
        KeyBinding::new("cmd-}", NextTab, Some("Workspace")),
        KeyBinding::new("cmd-{", PrevTab, Some("Workspace")),
    ]);
}

enum TabState {
    /// Only the path is known. Costs a few hundred bytes.
    Cold,
    Loading,
    Viewing {
        viewer: Entity<Viewer>,
        _changes: Subscription,
    },
    Editing {
        editor: Entity<EditorState>,
        /// The document the editor was opened from; reused if no edits are made.
        doc: Arc<Document>,
        _changes: Subscription,
    },
    /// Shown instead of a document: binary or unreadable.
    Message(SharedString),
}

struct DocTab {
    id: u64,
    path: PathBuf,
    title: SharedString,
    state: TabState,
    dirty: bool,
    /// First visible line when the tab was evicted; restored on reload.
    top_line: usize,
}

impl DocTab {
    fn new(id: u64, path: PathBuf) -> Self {
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self { id, path, title: title.into(), state: TabState::Cold, dirty: false, top_line: 0 }
    }

    fn doc<'a>(&'a self, cx: &'a App) -> Option<&'a Arc<Document>> {
        match &self.state {
            TabState::Viewing { viewer, .. } => Some(viewer.read(cx).doc()),
            TabState::Editing { doc, .. } => Some(doc),
            _ => None,
        }
    }

    fn is_live(&self) -> bool {
        matches!(self.state, TabState::Viewing { .. } | TabState::Editing { .. })
    }
}

pub struct Workspace {
    tabs: Vec<DocTab>,
    active: Option<u64>,
    /// Live tab ids, most recently shown first.
    recent: VecDeque<u64>,
    next_id: u64,
    focus: FocusHandle,
    tab_scroll: ScrollHandle,
    title: SharedString,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Theme::sync_system_appearance(Some(window), cx);
        cx.observe_window_appearance(window, |_, window, cx| {
            Theme::sync_system_appearance(Some(window), cx);
        })
        .detach();
        Self {
            tabs: Vec::new(),
            active: None,
            recent: VecDeque::new(),
            next_id: 1,
            focus,
            tab_scroll: ScrollHandle::new(),
            title: SharedString::default(),
        }
    }

    /// Adds tabs without reading anything; the first path becomes active.
    pub fn open_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let mut first = None;
        for path in paths {
            let path = std::fs::canonicalize(&path).unwrap_or(path);
            if path.is_dir() {
                continue;
            }
            let id = match self.tabs.iter().find(|t| t.path == path) {
                Some(tab) => tab.id,
                None => {
                    let id = self.next_id;
                    self.next_id += 1;
                    self.tabs.push(DocTab::new(id, path));
                    id
                }
            };
            first.get_or_insert(id);
        }
        if let Some(id) = first {
            // Loading and focusing happen in `render`, which has a window.
            self.active = Some(id);
        }
        cx.notify();
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    fn active_index(&self) -> Option<usize> {
        self.active.and_then(|id| self.index_of(id))
    }

    fn active_tab(&self) -> Option<&DocTab> {
        self.active_index().map(|ix| &self.tabs[ix])
    }

    fn focus_tab(&self, ix: usize, window: &mut Window, cx: &mut App) {
        match &self.tabs[ix].state {
            TabState::Viewing { viewer, .. } => window.focus(&viewer.focus_handle(cx), cx),
            TabState::Editing { editor, .. } => editor.update(cx, |state, cx| state.focus(window, cx)),
            _ => window.focus(&self.focus, cx),
        }
    }

    fn activate(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.active = Some(id);
        if let Some(ix) = self.index_of(id) {
            if self.tabs[ix].is_live() {
                self.focus_tab(ix, window, cx);
                self.touch(id, cx);
            }
        }
        cx.notify();
    }

    fn activate_index(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(ix) {
            self.activate(tab.id, window, cx);
        }
    }

    /// Starts reading the active tab if it has never been loaded (or was evicted).
    fn load_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active_index() else { return };
        let tab = &mut self.tabs[ix];
        if !matches!(tab.state, TabState::Cold) {
            return;
        }
        tab.state = TabState::Loading;
        let (id, path) = (tab.id, tab.path.clone());
        let started = Instant::now();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { doc::load(&path) }).await;
            this.update_in(cx, |this, window, cx| this.finish_load(id, result, started, window, cx))
                .ok();
        })
        .detach();
    }

    fn finish_load(
        &mut self,
        id: u64,
        result: std::io::Result<Loaded>,
        started: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.index_of(id) else { return }; // closed while loading
        let read_time = started.elapsed();
        self.tabs[ix].state = match result {
            Ok(Loaded::Doc(doc)) => {
                let line = self.tabs[ix].top_line;
                self.viewing(ix, doc, line, cx)
            }
            Ok(Loaded::Binary { bytes }) => TabState::Message(
                format!("Binary file ({}), not shown.", doc::human_size(bytes)).into(),
            ),
            Err(err) => TabState::Message(format!("Could not open this file: {err}").into()),
        };
        if self.tabs[ix].is_live() {
            self.touch(id, cx);
            if self.active == Some(id) {
                self.focus_tab(ix, window, cx);
            }
        }
        let tab = &self.tabs[ix];
        if let Some(doc) = tab.doc(cx) {
            eprintln!(
                "[leaf] {} · {} · {} lines · read+index {:?} · ready {:?}",
                tab.title,
                doc::human_size(doc.len() as u64),
                doc.line_count(),
                read_time,
                started.elapsed()
            );
        }
        cx.notify();
    }

    fn viewing(&self, ix: usize, doc: Arc<Document>, top_line: usize, cx: &mut Context<Self>) -> TabState {
        let path = self.tabs[ix].path.clone();
        let viewer = cx.new(|cx| Viewer::new(doc, &path, cx));
        if top_line > 0 {
            viewer.update(cx, |viewer, cx| viewer.scroll_to_line(top_line, cx));
        }
        // Repaint the status bar when the viewer's selection or progress changes.
        let changes = cx.observe(&viewer, |_, _, cx| cx.notify());
        TabState::Viewing { viewer, _changes: changes }
    }

    /// Marks `id` most recently used and evicts tabs beyond `MAX_LIVE`.
    fn touch(&mut self, id: u64, cx: &mut Context<Self>) {
        self.recent.retain(|&x| x != id);
        self.recent.push_front(id);
        let active = self.active;
        let mut kept = 0;
        let mut survivors = VecDeque::with_capacity(self.recent.len());
        for id in std::mem::take(&mut self.recent) {
            let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) else { continue };
            if !tab.is_live() {
                continue;
            }
            // Unsaved edits and the visible tab are never evicted.
            if kept < MAX_LIVE || tab.dirty || Some(id) == active {
                kept += 1;
                survivors.push_back(id);
                continue;
            }
            tab.top_line = match &tab.state {
                TabState::Viewing { viewer, .. } => viewer.read(cx).top_line(),
                TabState::Editing { editor, .. } => editor.read(cx).cursor_position().line as usize,
                _ => 0,
            };
            tab.state = TabState::Cold;
        }
        self.recent = survivors;
    }

    fn set_dirty(&mut self, id: u64, dirty: bool, cx: &mut Context<Self>) {
        if let Some(ix) = self.index_of(id) {
            if self.tabs[ix].dirty != dirty {
                self.tabs[ix].dirty = dirty;
                cx.notify();
            }
        }
    }

    /// Switches the tab between the viewer and the full editor.
    fn toggle_edit(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.tabs[ix].id;
        match &self.tabs[ix].state {
            TabState::Viewing { viewer, .. } => {
                let (doc, top) = {
                    let viewer = viewer.read(cx);
                    (viewer.doc().clone(), viewer.top_line())
                };
                if doc.len() > doc::MAX_EDIT_BYTES {
                    let _ = window.prompt(
                        PromptLevel::Info,
                        "This file is too large to edit in Leaf",
                        Some(&format!(
                            "Editing is available for files up to {}.",
                            doc::human_size(doc::MAX_EDIT_BYTES as u64)
                        )),
                        &["OK"],
                        cx,
                    );
                    return;
                }
                let text = doc.text(0..doc.len()).into_owned();
                let language = doc::language_token(&self.tabs[ix].path);
                let editor = cx.new(|cx| EditorState::new(window, cx).language(language).default_value(text));
                let changes = cx.subscribe(&editor, move |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.set_dirty(id, true, cx);
                    }
                });
                // Keep the reader's place: the text reaches the editor on its first paint.
                let target = editor.clone();
                cx.on_next_frame(window, move |_, window, cx| {
                    target.update(cx, |state, cx| {
                        state.set_cursor_position(Position::new(top as u32, 0), window, cx);
                        state.focus(window, cx);
                    });
                });
                self.tabs[ix].state = TabState::Editing { editor, doc, _changes: changes };
            }
            TabState::Editing { editor, doc, .. } => {
                let top = editor.read(cx).cursor_position().line as usize;
                let doc = if self.tabs[ix].dirty {
                    let text = editor.read(cx).text().to_string();
                    Arc::new(Document::new(Bytes::Owned(text.into_bytes()), false))
                } else {
                    doc.clone()
                };
                self.tabs[ix].state = self.viewing(ix, doc, top.saturating_sub(5), cx);
                self.focus_tab(ix, window, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    /// Writes the tab to disk. Returns false if it could not be saved.
    fn save_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(ix) = self.index_of(id) else { return false };
        if !self.tabs[ix].dirty {
            return true;
        }
        let bytes: Vec<u8> = match &self.tabs[ix].state {
            TabState::Editing { editor, .. } => editor.read(cx).text().to_string().into_bytes(),
            TabState::Viewing { viewer, .. } => viewer.read(cx).doc().bytes().to_vec(),
            _ => return false,
        };
        match std::fs::write(&self.tabs[ix].path, bytes) {
            Ok(()) => {
                self.set_dirty(id, false, cx);
                true
            }
            Err(err) => {
                let _ = window.prompt(
                    PromptLevel::Critical,
                    &format!("Could not save {}", self.tabs[ix].title),
                    Some(&err.to_string()),
                    &["OK"],
                    cx,
                );
                false
            }
        }
    }

    fn close_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(id) else { return };
        if !self.tabs[ix].dirty {
            self.remove_tab(id, window, cx);
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Save changes to {}?", self.tabs[ix].title),
            Some("Your changes will be lost if you don't save them."),
            &["Save", "Don't Save", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else { return };
            this.update_in(cx, |this, window, cx| match choice {
                0 if this.save_tab(id, window, cx) => this.remove_tab(id, window, cx),
                1 => this.remove_tab(id, window, cx),
                _ => {}
            })
            .ok();
        })
        .detach();
    }

    fn remove_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(id) else { return };
        self.tabs.remove(ix);
        self.recent.retain(|&x| x != id);
        if self.active == Some(id) {
            match self.tabs.get(ix).or_else(|| self.tabs.last()) {
                Some(next) => self.activate(next.id, window, cx),
                None => {
                    self.active = None;
                    window.focus(&self.focus, cx);
                }
            }
        }
        cx.notify();
    }

    fn cycle(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.tabs.len() as isize;
        if n == 0 {
            return;
        }
        let ix = self.active_index().unwrap_or(0) as isize;
        self.activate_index((ix + delta).rem_euclid(n) as usize, window, cx);
    }

    // ── actions ────────────────────────────────────────────────────────────

    fn on_open(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                this.update(cx, |this, cx| this.open_paths(paths, cx)).ok();
            }
        })
        .detach();
    }

    fn on_toggle_edit(&mut self, _: &ToggleEdit, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.active_index() {
            self.toggle_edit(ix, window, cx);
        }
    }

    fn on_save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.active {
            self.save_tab(id, window, cx);
        }
    }

    fn on_close(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        match self.active {
            Some(id) => self.close_tab(id, window, cx),
            None => window.remove_window(),
        }
    }

    fn on_next(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle(1, window, cx);
    }

    fn on_prev(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle(-1, window, cx);
    }

    // ── rendering ──────────────────────────────────────────────────────────

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().secondary;
        TabBar::new("tabs")
            .track_scroll(&self.tab_scroll)
            .selected_index(self.active_index().unwrap_or(0))
            .on_click(cx.listener(|this, ix: &usize, window, cx| this.activate_index(*ix, window, cx)))
            .children(self.tabs.iter().map(|tab| {
                let id = tab.id;
                let label = if tab.dirty { format!("● {}", tab.title) } else { tab.title.to_string() };
                Tab::new().label(label).suffix(
                    div()
                        .id(("close-tab", id as usize))
                        .px_1()
                        .rounded_sm()
                        .text_xs()
                        .text_color(muted)
                        .hover(move |style| style.bg(hover))
                        .child("✕")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_tab(id, window, cx);
                        })),
                )
            }))
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let centered = |text: SharedString, cx: &mut Context<Self>| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(text)
                .into_any_element()
        };
        match self.active_tab() {
            None => centered("Drop files here, or press ⌘O".into(), cx),
            Some(tab) => match &tab.state {
                TabState::Viewing { viewer, .. } => viewer.clone().into_any_element(),
                TabState::Editing { editor, .. } => {
                    Editor::new(editor).bordered(false).h(relative(1.)).into_any_element()
                }
                TabState::Message(message) => centered(message.clone(), cx),
                TabState::Cold | TabState::Loading => div().size_full().into_any_element(),
            },
        }
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (mut mode, mut mode_color, mut details) = ("", theme.muted_foreground, Vec::new());
        if let Some(tab) = self.active_tab() {
            if let TabState::Editing { .. } = tab.state {
                (mode, mode_color) = ("EDIT", theme.warning);
            } else if tab.is_live() {
                mode = "VIEW";
            }
            if let TabState::Viewing { viewer, .. } = &tab.state {
                let status = viewer.read(cx).status();
                if !status.is_empty() {
                    details.push(status);
                }
            }
            if let Some(doc) = tab.doc(cx) {
                details.push(format!("{} lines", doc.line_count()));
                details.push(doc::human_size(doc.len() as u64));
                details.push(if doc.lossy { "UTF-8 (lossy)".into() } else { "UTF-8".into() });
                if doc.crlf {
                    details.push("CRLF".into());
                }
            }
            details.push(format!("{}/{} loaded", self.recent.len(), self.tabs.len()));
        }
        let path = self.active_tab().map(|t| t.path.display().to_string()).unwrap_or_default();
        h_flex()
            .flex_none()
            .justify_between()
            .gap_4()
            .px_3()
            .py_1()
            .text_xs()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .text_color(theme.muted_foreground)
            .child(
                h_flex()
                    .gap_3()
                    .min_w_0()
                    .child(
                        div()
                            .id("mode")
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(mode_color)
                            .child(mode)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.on_toggle_edit(&ToggleEdit, window, cx)
                            })),
                    )
                    .child(div().truncate().child(path)),
            )
            .child(div().flex_none().child(details.join("  ·  ")))
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.load_active(window, cx);

        let title: SharedString = match self.active_tab() {
            Some(tab) if tab.dirty => format!("● {}", tab.title).into(),
            Some(tab) => tab.title.clone(),
            None => "Leaf".into(),
        };
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }

        v_flex()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(Self::on_toggle_edit))
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(Self::on_next))
            .on_action(cx.listener(Self::on_prev))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.open_paths(paths.paths().to_vec(), cx)
            }))
            .when(!self.tabs.is_empty(), |this| this.child(self.render_tab_bar(cx)))
            .child(div().flex_1().min_h_0().child(self.render_body(cx)))
            .child(self.render_status_bar(cx))
    }
}
