//! The window, laid out like Apple Notes: a sidebar listing open files
//! (grouped by date, with a first-line preview) beside exactly one document.
//!
//! Tabs are lazy. Opening 500 files only records 500 paths; a file is read
//! from disk the first time its tab is shown. Files open in the lightweight
//! [`Viewer`]; ⌘E swaps in the full editor for that tab only. At most
//! `MAX_LIVE` tabs stay in memory; older clean ones fall back to `Cold`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Editor, EditorState, Input, InputEvent, InputState, Position, TabSize},
    theme::Theme,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::doc::{self, DiskStamp, Loaded};
use crate::encoding::{self, TextFormat};
use crate::file_list::{self, FileMeta};
use crate::session::{self, Session, SessionTab};
use crate::text_index::{Bytes, Document};
use crate::viewer::Viewer;
use crate::{
    CloseTab, FocusSearch, NextTab, OpenFile, PrevTab, Quit, ReopenClosed, Save, ToggleEdit, ToggleSidebar,
};

/// Tabs kept in memory at once. Everything else is just a path.
const MAX_LIVE: usize = 12;
/// Closed tabs remembered for ⌘⇧T.
const MAX_CLOSED: usize = 50;
const SIDEBAR_WIDTH: f32 = 290.;
const TOOLBAR_HEIGHT: f32 = 52.;
/// Room for the traffic lights, which sit inside the toolbar like in Notes.
const TRAFFIC_LIGHTS_WIDTH: f32 = 78.;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-o", OpenFile, None),
        KeyBinding::new("cmd-e", ToggleEdit, Some("Workspace")),
        KeyBinding::new("cmd-s", Save, Some("Workspace")),
        KeyBinding::new("cmd-w", CloseTab, Some("Workspace")),
        KeyBinding::new("cmd-shift-t", ReopenClosed, Some("Workspace")),
        KeyBinding::new("ctrl-tab", NextTab, Some("Workspace")),
        KeyBinding::new("ctrl-shift-tab", PrevTab, Some("Workspace")),
        KeyBinding::new("cmd-}", NextTab, Some("Workspace")),
        KeyBinding::new("cmd-{", PrevTab, Some("Workspace")),
        KeyBinding::new("cmd-alt-s", ToggleSidebar, Some("Workspace")),
        KeyBinding::new("cmd-alt-f", FocusSearch, Some("Workspace")),
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
        /// The document the editor was opened from (for the status bar).
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
    /// Encoding, BOM and line endings to write back on save.
    format: TextFormat,
    /// The file as last read or written by us.
    stamp: Option<DiskStamp>,
    /// Set when the file changed or vanished on disk while we had unsaved edits.
    conflict: Option<&'static str>,
    /// Date and first line for the sidebar; read from the first 2KB only.
    meta: Option<FileMeta>,
    peeking: bool,
}

impl DocTab {
    fn new(id: u64, path: PathBuf) -> Self {
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self {
            id,
            path,
            title: title.into(),
            state: TabState::Cold,
            dirty: false,
            top_line: 0,
            format: TextFormat::default(),
            stamp: None,
            conflict: None,
            meta: None,
            peeking: false,
        }
    }

    fn modified(&self) -> Option<SystemTime> {
        self.meta.as_ref().and_then(|m| m.modified)
    }

    fn folder(&self) -> String {
        self.path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".into())
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

    /// Where the reader is: the viewer's top line or the editor's cursor line.
    fn current_line(&self, cx: &App) -> usize {
        match &self.state {
            TabState::Viewing { viewer, .. } => viewer.read(cx).top_line(),
            TabState::Editing { editor, .. } => editor.read(cx).cursor_position().line as usize,
            _ => self.top_line,
        }
    }
}

pub struct Workspace {
    tabs: Vec<DocTab>,
    active: Option<u64>,
    /// Live tab ids, most recently shown first.
    recent: VecDeque<u64>,
    /// Recently closed `(path, line)`, newest last.
    closed: Vec<(PathBuf, usize)>,
    next_id: u64,
    focus: FocusHandle,
    sidebar_open: bool,
    search: Entity<InputState>,
    _search_events: Subscription,
    title: SharedString,
    _session_save: Option<Task<()>>,
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
        // Coming back to the app is when files have usually changed underneath us.
        cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                if let Some(id) = this.active {
                    this.check_disk(id, window, cx);
                }
            }
        })
        .detach();
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| this.confirm_quit(window, cx)).unwrap_or(true)
        });
        cx.on_app_quit(|this, cx| {
            session::save(&this.session(cx));
            async {}
        })
        .detach();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let search_events = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            tabs: Vec::new(),
            active: None,
            recent: VecDeque::new(),
            closed: Vec::new(),
            next_id: 1,
            focus,
            sidebar_open: true,
            search,
            _search_events: search_events,
            title: SharedString::default(),
            _session_save: None,
        }
    }

    /// Adds tabs without reading anything; the first path becomes active.
    /// Returns the id of that tab.
    pub fn open_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) -> Option<u64> {
        let mut first = None;
        for path in paths {
            let path = std::fs::canonicalize(&path).unwrap_or(path);
            if path.is_dir() {
                continue;
            }
            let id = match self.tabs.iter().find(|t| t.path == path) {
                Some(tab) => tab.id,
                None => self.push_tab(path),
            };
            first.get_or_insert(id);
        }
        if let Some(id) = first {
            // Loading and focusing happen in `render`, which has a window.
            self.active = Some(id);
            self.schedule_session_save(cx);
        }
        self.peek_missing(cx);
        cx.notify();
        first
    }

    /// Reads dates and first lines for sidebar rows that don't have them yet.
    fn peek_missing(&mut self, cx: &mut Context<Self>) {
        let todo: Vec<(u64, PathBuf)> = self
            .tabs
            .iter_mut()
            .filter(|t| t.meta.is_none() && !t.peeking)
            .map(|t| {
                t.peeking = true;
                (t.id, t.path.clone())
            })
            .collect();
        if todo.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let metas = cx
                .background_executor()
                .spawn(async move { todo.into_iter().map(|(id, path)| (id, file_list::peek(&path))).collect::<Vec<_>>() })
                .await;
            this.update(cx, |this, cx| {
                for (id, meta) in metas {
                    if let Some(ix) = this.index_of(id) {
                        (this.tabs[ix].meta, this.tabs[ix].peeking) = (Some(meta), false);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Re-reads one row's date and first line (after a save or reload).
    fn refresh_meta(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(ix) = self.index_of(id) {
            if !self.tabs[ix].peeking {
                self.tabs[ix].meta = None;
            }
        }
        self.peek_missing(cx);
    }

    /// Tab indices in sidebar order: most recently modified first, like Notes.
    fn display_order(&self) -> Vec<usize> {
        let now = SystemTime::now();
        let mut order: Vec<usize> = (0..self.tabs.len()).collect();
        order.sort_by_key(|&ix| std::cmp::Reverse(self.tabs[ix].modified().unwrap_or(now)));
        order
    }

    fn push_tab(&mut self, path: PathBuf) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(DocTab::new(id, path));
        id
    }

    // ── session ────────────────────────────────────────────────────────────

    pub fn restore(&mut self, session: Session, cx: &mut Context<Self>) {
        let mut active = None;
        for (ix, saved) in session.tabs.into_iter().enumerate() {
            if !saved.path.is_file() || self.tabs.iter().any(|t| t.path == saved.path) {
                continue;
            }
            let id = self.push_tab(saved.path);
            self.tabs.last_mut().expect("just pushed").top_line = saved.top_line;
            if session.active == Some(ix) {
                active = Some(id);
            }
        }
        self.active = active.or(self.tabs.first().map(|t| t.id));
        self.peek_missing(cx);
        cx.notify();
    }

    fn session(&self, cx: &App) -> Session {
        Session {
            tabs: self
                .tabs
                .iter()
                .map(|t| SessionTab { path: t.path.clone(), top_line: t.current_line(cx) })
                .collect(),
            active: self.active_index(),
        }
    }

    /// Saves the session a moment after the last change, not on every click.
    fn schedule_session_save(&mut self, cx: &mut Context<Self>) {
        self._session_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            this.update(cx, |this, cx| session::save(&this.session(cx))).ok();
        }));
    }

    // ── tabs ───────────────────────────────────────────────────────────────

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
                self.check_disk(id, window, cx);
            }
        }
        self.schedule_session_save(cx);
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
            Ok(Loaded::Doc { doc, format, stamp }) => {
                let tab = &mut self.tabs[ix];
                (tab.format, tab.stamp, tab.conflict) = (format, Some(stamp), None);
                let line = tab.top_line;
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
                "[leaf] {} · {} · {} lines · {} · read+index {:?} · ready {:?}",
                tab.title,
                doc::human_size(doc.len() as u64),
                doc.line_count(),
                tab.format.label(),
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
            tab.top_line = tab.current_line(cx);
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

    // ── external changes ───────────────────────────────────────────────────

    /// Compares the file on disk with what we last saw. Clean tabs reload in
    /// place (keeping their position); tabs with unsaved edits are flagged.
    fn check_disk(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(id) else { return };
        let tab = &self.tabs[ix];
        if !tab.is_live() {
            return; // cold tabs read the current file when shown anyway
        }
        let (path, known) = (tab.path.clone(), tab.stamp);
        cx.spawn_in(window, async move |this, cx| {
            let now = cx.background_executor().spawn(async move { doc::stamp(&path) }).await;
            this.update(cx, |this, cx| this.on_disk_state(id, known, now, cx)).ok();
        })
        .detach();
    }

    fn on_disk_state(&mut self, id: u64, known: Option<DiskStamp>, now: Option<DiskStamp>, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(id) else { return };
        // Ignore stale answers (we saved or reloaded in the meantime).
        if now == known || self.tabs[ix].stamp != known || !self.tabs[ix].is_live() {
            return;
        }
        let tab = &mut self.tabs[ix];
        match now {
            None => tab.conflict = Some("deleted on disk"),
            Some(_) if tab.dirty => tab.conflict = Some("changed on disk"),
            Some(_) => {
                tab.top_line = tab.current_line(cx);
                tab.state = TabState::Cold; // `render` reloads it
                self.recent.retain(|&x| x != id);
                self.refresh_meta(id, cx);
            }
        }
        cx.notify();
    }

    // ── editing & saving ───────────────────────────────────────────────────

    /// Switches the tab between the viewer and the full editor.
    fn toggle_edit(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.tabs[ix].id;
        match &self.tabs[ix].state {
            TabState::Viewing { viewer, .. } => {
                let (doc, top, at) =
                    viewer.update(cx, |viewer, _| (viewer.doc().clone(), viewer.top_line(), viewer.take_edit_at()));
                // A double-click puts the cursor where you clicked; ⌘E keeps the reading position.
                let cursor = match at {
                    Some(pos) => {
                        let raw = &doc.bytes()[doc.line_range(pos.line)];
                        let before = String::from_utf8_lossy(&raw[..pos.col.min(raw.len())]);
                        Position::new(pos.line as u32, before.encode_utf16().count() as u32)
                    }
                    None => Position::new(top as u32, 0),
                };
                let refusal = if doc.len() > doc::MAX_EDIT_BYTES {
                    Some(format!("Editing is available for files up to {}.", doc::human_size(doc::MAX_EDIT_BYTES as u64)))
                } else if doc.lossy {
                    Some(format!(
                        "It contains bytes that aren't valid {}. Saving would replace them, so editing is off to protect the file.",
                        self.tabs[ix].format.encoding.name()
                    ))
                } else {
                    None
                };
                if let Some(detail) = refusal {
                    let _ = window.prompt(PromptLevel::Info, "This file can't be edited in Leaf", Some(&detail), &["OK"], cx);
                    return;
                }
                let text = doc.text(0..doc.len()).into_owned();
                let language = doc::language_token(&self.tabs[ix].path);
                let tab_size = TabSize { tab_size: crate::text_index::TAB_WIDTH, hard_tabs: doc::indents_with_tabs(&doc) };
                let editor = cx.new(|cx| {
                    EditorState::new(window, cx).language(language).tab_size(tab_size).default_value(text)
                });
                let changes = cx.subscribe(&editor, move |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.set_dirty(id, true, cx);
                    }
                });
                // Keep the reader's place: the text reaches the editor on its first paint.
                let target = editor.clone();
                cx.on_next_frame(window, move |_, window, cx| {
                    target.update(cx, |state, cx| {
                        state.set_cursor_position(cursor, window, cx);
                        state.focus(window, cx);
                    });
                });
                self.tabs[ix].state = TabState::Editing { editor, doc, _changes: changes };
            }
            TabState::Editing { editor, .. } => {
                let top = editor.read(cx).cursor_position().line as usize;
                // Rebuild from the editor even when clean: edits may already be saved.
                let text = editor.read(cx).text().to_string();
                let doc = Arc::new(Document::new(Bytes::Owned(text.into_bytes()), false));
                self.tabs[ix].state = self.viewing(ix, doc, top.saturating_sub(5), cx);
                self.focus_tab(ix, window, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    /// Writes the tab to disk in its original encoding, BOM and line endings.
    /// Returns false if it could not be saved.
    fn save_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(ix) = self.index_of(id) else { return false };
        let tab = &self.tabs[ix];
        if !tab.dirty {
            return true;
        }
        if let Some(conflict) = tab.conflict {
            // Never silently replace someone else's change; ask, then save again.
            let answer = window.prompt(
                PromptLevel::Warning,
                &format!("{} was {conflict} since Leaf opened it.", tab.title),
                Some("Saving replaces the version on disk with yours."),
                &["Overwrite", "Cancel"],
                cx,
            );
            cx.spawn_in(window, async move |this, cx| {
                if answer.await == Ok(0) {
                    this.update_in(cx, |this, window, cx| {
                        if let Some(ix) = this.index_of(id) {
                            this.tabs[ix].conflict = None;
                            this.save_tab(id, window, cx);
                        }
                    })
                    .ok();
                }
            })
            .detach();
            return false;
        }
        let text: String = match &tab.state {
            TabState::Editing { editor, .. } => editor.read(cx).text().to_string(),
            TabState::Viewing { viewer, .. } => viewer.read(cx).doc().text(0..viewer.read(cx).doc().len()).into_owned(),
            _ => return false,
        };
        let (bytes, unmappable) = encoding::encode(&text, tab.format);
        let failure = if unmappable {
            Some(format!(
                "Some characters can't be represented in {}. Remove them, or save a copy as UTF-8.",
                tab.format.encoding.name()
            ))
        } else {
            std::fs::write(&tab.path, bytes).err().map(|err| err.to_string())
        };
        if let Some(detail) = failure {
            let title = format!("Could not save {}", tab.title);
            let _ = window.prompt(PromptLevel::Critical, &title, Some(&detail), &["OK"], cx);
            return false;
        }
        let tab = &mut self.tabs[ix];
        (tab.stamp, tab.conflict) = (doc::stamp(&tab.path), None);
        self.set_dirty(id, false, cx);
        self.refresh_meta(id, cx);
        true
    }

    // ── closing & quitting ─────────────────────────────────────────────────

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
        // The row below in the sidebar takes its place (or the one above).
        let order = self.display_order();
        let pos = order.iter().position(|&o| o == ix).unwrap_or(0);
        let next = order.get(pos + 1).or(pos.checked_sub(1).and_then(|p| order.get(p))).map(|&o| self.tabs[o].id);
        let line = self.tabs[ix].current_line(cx);
        let tab = self.tabs.remove(ix);
        self.closed.push((tab.path, line));
        if self.closed.len() > MAX_CLOSED {
            self.closed.remove(0);
        }
        self.recent.retain(|&x| x != id);
        if self.active == Some(id) {
            match next {
                Some(next) => self.activate(next, window, cx),
                None => {
                    self.active = None;
                    window.focus(&self.focus, cx);
                }
            }
        }
        self.schedule_session_save(cx);
        cx.notify();
    }

    /// Saves the session and returns true if it's fine to quit now. With
    /// unsaved tabs it asks first and quits by itself if the user agrees.
    fn confirm_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        session::save(&self.session(cx));
        let dirty: Vec<u64> = self.tabs.iter().filter(|t| t.dirty).map(|t| t.id).collect();
        if dirty.is_empty() {
            return true;
        }
        let message = match dirty.len() {
            1 => "You have unsaved changes in 1 file.".to_string(),
            n => format!("You have unsaved changes in {n} files."),
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some("Save them before quitting?"),
            &["Save All", "Don't Save", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else { return };
            this.update_in(cx, |this, window, cx| {
                let quit = match choice {
                    0 => dirty.iter().all(|&id| this.save_tab(id, window, cx)),
                    1 => true,
                    _ => false,
                };
                if quit {
                    cx.quit();
                }
            })
            .ok();
        })
        .detach();
        false
    }

    fn cycle(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let order = self.display_order();
        let n = order.len() as isize;
        if n == 0 {
            return;
        }
        let pos = self.active_index().and_then(|a| order.iter().position(|&o| o == a)).unwrap_or(0) as isize;
        self.activate_index(order[(pos + delta).rem_euclid(n) as usize], window, cx);
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

    fn on_reopen_closed(&mut self, _: &ReopenClosed, _: &mut Window, cx: &mut Context<Self>) {
        while let Some((path, line)) = self.closed.pop() {
            if !path.is_file() || self.tabs.iter().any(|t| t.path == path) {
                continue;
            }
            if let Some(ix) = self.open_paths(vec![path], cx).and_then(|id| self.index_of(id)) {
                self.tabs[ix].top_line = line;
            }
            return;
        }
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

    fn on_quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_quit(window, cx) {
            cx.quit();
        }
    }

    fn on_next(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle(1, window, cx);
    }

    fn on_prev(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle(-1, window, cx);
    }

    fn on_toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = true;
        self.search.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    // ── rendering ──────────────────────────────────────────────────────────

    /// An empty strip that drags the window, since the title bar is hidden.
    fn drag_area() -> Div {
        div().h_full().on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
    }

    fn sidebar_button(&self, cx: &mut Context<Self>) -> Button {
        let tooltip = if self.sidebar_open { "Hide Sidebar (⌥⌘S)" } else { "Show Sidebar (⌥⌘S)" };
        Button::new("toggle-sidebar")
            .ghost()
            .icon(IconName::PanelLeft)
            .tooltip(tooltip)
            .on_click(cx.listener(|this, _, window, cx| this.on_toggle_sidebar(&ToggleSidebar, window, cx)))
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let dark = theme.mode.is_dark();
        let (muted, border, sidebar_bg, hover_bg) = (theme.muted_foreground, theme.border, theme.sidebar, theme.secondary);
        let separator = border.opacity(0.6);
        // Notes' highlight: a warm yellow, with text readable on it in both modes.
        let (selected_bg, selected_fg): (Hsla, Hsla) =
            if dark { (rgb(0x9a7b2c).into(), white()) } else { (rgb(0xf6d26b).into(), black()) };
        let now = chrono::Local::now();
        let query = self.search.read(cx).value().to_lowercase();

        let mut rows: Vec<AnyElement> = Vec::new();
        let mut current_group: Option<String> = None;
        // Thin inset lines between rows, hidden next to the highlighted one (like Notes).
        let mut previous_selected: Option<bool> = None;
        for ix in self.display_order() {
            let tab = &self.tabs[ix];
            let folder = tab.folder();
            let snippet = tab.meta.as_ref().map(|m| m.snippet.as_str()).unwrap_or("");
            if !query.is_empty()
                && ![tab.title.as_ref(), folder.as_str(), snippet].iter().any(|s| s.to_lowercase().contains(&query))
            {
                continue;
            }
            let modified = tab.modified().map(file_list::local);
            let group = modified.map(|m| file_list::group_label(m, now)).unwrap_or_else(|| "Today".into());
            if current_group.as_ref() != Some(&group) {
                rows.push(
                    div()
                        .px_3()
                        .pt(if current_group.is_none() { px(4.) } else { px(18.) })
                        .pb_1p5()
                        .text_lg()
                        .font_weight(FontWeight::BOLD)
                        .child(group.clone())
                        .into_any_element(),
                );
                current_group = Some(group);
                previous_selected = None;
            }

            let id = tab.id;
            let selected = Some(id) == self.active;
            if previous_selected == Some(false) && !selected {
                rows.push(div().mx_3().h(px(1.)).bg(separator).into_any_element());
            } else if previous_selected.is_some() {
                rows.push(div().h(px(1.)).into_any_element());
            }
            previous_selected = Some(selected);
            let row_group: SharedString = format!("row-{id}").into();
            let sub_color = if selected { selected_fg.opacity(0.75) } else { muted };
            let date = modified.map(|m| file_list::row_date(m, now)).unwrap_or_default();
            let title = if tab.dirty { format!("● {}", tab.title) } else { tab.title.to_string() };
            rows.push(
                v_flex()
                    .id(("file", id as usize))
                    .group(row_group.clone())
                    .relative()
                    .gap_0p5()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .when(selected, |row| row.bg(selected_bg).text_color(selected_fg))
                    .when(!selected, |row| row.hover(move |style| style.bg(hover_bg)))
                    .on_click(cx.listener(move |this, _, window, cx| this.activate(id, window, cx)))
                    .child(div().pr_5().font_weight(FontWeight::BOLD).truncate().child(title))
                    .child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .min_w_0()
                            .child(div().flex_none().child(date))
                            .child(div().min_w_0().truncate().text_color(sub_color).child(snippet.to_string())),
                    )
                    .child(
                        h_flex()
                            .gap_1p5()
                            .text_xs()
                            .text_color(sub_color)
                            .child(Icon::new(IconName::Folder).xsmall())
                            .child(div().truncate().child(folder)),
                    )
                    .child(
                        div()
                            .id(("close-file", id as usize))
                            .absolute()
                            .top_2()
                            .right_2()
                            .px_1()
                            .rounded_sm()
                            .text_xs()
                            .invisible()
                            .group_hover(row_group, |style| style.visible())
                            .hover(move |style| style.bg(hover_bg))
                            .child("✕")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(id, window, cx);
                            })),
                    )
                    .into_any_element(),
            );
        }
        let empty = rows.is_empty();

        v_flex()
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            // Translucent over the window's blur, like a macOS sidebar.
            .bg(sidebar_bg.opacity(0.62))
            .border_r_1()
            .border_color(border)
            .child(
                h_flex()
                    .h(px(TOOLBAR_HEIGHT))
                    .flex_none()
                    .pr_2()
                    .child(Self::drag_area().w(px(TRAFFIC_LIGHTS_WIDTH)))
                    .child(Self::drag_area().flex_1())
                    .child(self.sidebar_button(cx)),
            )
            .child(div().px_3().pb_2().child(Input::new(&self.search).small().prefix(Icon::new(IconName::Search).xsmall())))
            .child(
                div()
                    .id("file-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    .pb_3()
                    .children(rows)
                    .when(empty, |list| {
                        let text = if query.is_empty() { "No Open Files" } else { "No Matches" };
                        list.child(div().pt_10().w_full().text_center().text_color(muted).child(text))
                    }),
            )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let tab = self.active_tab();
        let title = tab.map(|t| t.title.clone()).unwrap_or_else(|| "Leaf".into());
        // Short like Notes' "92 notes": folder name plus file facts. The full
        // path is in the window title tooltip and the sidebar row.
        let subtitle = tab.map(|t| t.folder()).unwrap_or_else(|| format!("{} open", self.tabs.len()));
        let editing = tab.is_some_and(|t| matches!(t.state, TabState::Editing { .. }));
        let viewer = tab.and_then(|t| match &t.state {
            TabState::Viewing { viewer, .. } => Some(viewer.clone()),
            _ => None,
        });
        let (details, warning) = self.file_details(cx);
        let subtitle = std::iter::once(subtitle).chain(details).collect::<Vec<_>>().join("  ·  ");
        let danger = cx.theme().danger;
        let capsule_bg = cx.theme().secondary.opacity(0.7);
        let page_toggle = viewer.as_ref().filter(|v| v.read(cx).can_toggle_page()).map(|v| {
            let showing_page = v.read(cx).page_kind().is_some();
            let (icon, tooltip) = if showing_page {
                (IconName::Code, "Show Source Lines (⇧⌘V)")
            } else {
                (IconName::BookOpen, "Show as Page (⇧⌘V)")
            };
            let viewer = v.clone();
            Button::new("page-toggle").ghost().icon(icon).tooltip(tooltip).on_click(move |_, _, cx| {
                viewer.update(cx, |viewer, cx| viewer.toggle_page(cx));
            })
        });

        h_flex()
            .h(px(TOOLBAR_HEIGHT))
            .flex_none()
            .pr_3()
            .gap_1()
            .border_b_1()
            .border_color(border)
            .when(!self.sidebar_open, |bar| {
                bar.child(Self::drag_area().w(px(TRAFFIC_LIGHTS_WIDTH))).child(self.sidebar_button(cx))
            })
            .child(
                v_flex()
                    .min_w_0()
                    .pl_4()
                    .child(div().font_weight(FontWeight::BOLD).truncate().child(title))
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .min_w_0()
                            .when_some(warning, |row, warning| {
                                row.child(div().flex_none().text_color(danger).child(warning))
                            })
                            .child(div().text_color(muted).truncate().child(subtitle)),
                    ),
            )
            .child(Self::drag_area().flex_1())
            .when(tab.is_some_and(|t| t.is_live()), |bar| {
                bar.child(
                    // Grouped in a capsule, like Notes' toolbar buttons.
                    h_flex()
                        .flex_none()
                        .gap_0p5()
                        .p_0p5()
                        .rounded_full()
                        .bg(capsule_bg)
                        .children(page_toggle)
                        .child(
                            Button::new("edit")
                                .ghost()
                                .icon(if editing { IconName::Check } else { IconName::SquarePen })
                                .label(if editing { "Done" } else { "Edit" })
                                .tooltip("Edit (⌘E or double-click)")
                                .on_click(cx.listener(|this, _, window, cx| this.on_toggle_edit(&ToggleEdit, window, cx))),
                        ),
                )
            })
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let centered = |text: SharedString| {
            div().size_full().flex().items_center().justify_center().text_color(muted).child(text).into_any_element()
        };
        let Some(tab) = self.active_tab() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(muted)
                .child(Icon::new(IconName::FileText).size_12().text_color(muted.opacity(0.5)))
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("No File Open"))
                .child(div().text_sm().child("Drop files here, or press ⌘O"))
                .into_any_element();
        };
        match &tab.state {
            TabState::Viewing { viewer, .. } => {
                // Pages (Markdown, prose) get Notes' date line above the text.
                let date = viewer.read(cx).page_kind().and(tab.modified()).map(|m| file_list::full_date(file_list::local(m)));
                v_flex()
                    .size_full()
                    .when_some(date, |page, date| {
                        page.child(div().flex_none().w_full().pt_3().pb_2().text_center().text_sm().text_color(muted).child(date))
                    })
                    .child(div().flex_1().min_h_0().child(viewer.clone()))
                    .into_any_element()
            }
            TabState::Editing { editor, .. } => Editor::new(editor).bordered(false).h(relative(1.)).into_any_element(),
            TabState::Message(message) => centered(message.clone()),
            TabState::Cold | TabState::Loading => div().size_full().into_any_element(),
        }
    }

    /// Line count, encoding and viewer state for the toolbar subtitle, plus a
    /// warning when the file changed on disk under unsaved edits.
    fn file_details(&self, cx: &App) -> (Vec<String>, Option<String>) {
        let Some(tab) = self.active_tab() else { return (Vec::new(), None) };
        let mut details = Vec::new();
        if let TabState::Viewing { viewer, .. } = &tab.state {
            let status = viewer.read(cx).status();
            if !status.is_empty() {
                details.push(status);
            }
        }
        if let Some(doc) = tab.doc(cx) {
            details.push(format!("{} lines", doc.line_count()));
            details.push(doc::human_size(doc.len() as u64));
            let mut encoding = tab.format.label();
            if doc.lossy {
                encoding.push_str(" (lossy)");
            }
            details.push(encoding);
            if tab.format.crlf {
                details.push("CRLF".into());
            }
        }
        (details, tab.conflict.map(|c| format!("⚠ {c}")))
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

        h_flex()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(Self::on_reopen_closed))
            .on_action(cx.listener(Self::on_toggle_edit))
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_next))
            .on_action(cx.listener(Self::on_prev))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_focus_search))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.open_paths(paths.paths().to_vec(), cx);
            }))
            .when(self.sidebar_open, |this| this.child(self.render_sidebar(cx)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .bg(cx.theme().background)
                    .child(self.render_toolbar(cx))
                    .child(div().flex_1().min_h_0().child(self.render_body(cx))),
            )
    }
}
