//! The read-only viewer: draws only the rows on screen, from a shared
//! [`Document`]. Memory is the file plus a small line index; the full editor
//! is created only when the user switches a tab to edit mode.

use std::collections::{HashSet, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, h_flex,
    input::{Input, InputEvent, InputState},
    scroll::Scrollbar,
    text::{SelectionFormat, TextView, TextViewState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use futures::StreamExt as _;

use crate::highlight::{self, CHUNK, Spans};

#[path = "board_ui.rs"]
mod board_ui;
#[path = "page.rs"]
mod page;
pub use board_ui::BoardEdited;
use board_ui::BoardState;
use crate::text_index::{Bytes, DisplayLine, Document};

actions!(
    viewer,
    [
        LineUp, LineDown, PageUp, PageDown, Top, Bottom, CopySelection, SelectAllText, Find,
        FindNext, FindPrev, GoToLine, Dismiss, TogglePreview, FilterLines, ZoomIn, ZoomOut, ZoomReset, ToggleWrap
    ]
);

/// Text size, in percent, shared by every viewer (⌘+ / ⌘−; kept in the session).
static FONT_SCALE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(100);

pub fn font_scale_percent() -> u32 {
    FONT_SCALE.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_font_scale_percent(percent: u32) {
    FONT_SCALE.store(percent.clamp(60, 250), std::sync::atomic::Ordering::Relaxed);
}

fn font_scale() -> f32 {
    font_scale_percent() as f32 / 100.
}

/// Wrapped view of code and logs (⌥Z) is a page; bigger files keep their lines.
const MAX_WRAP_BYTES: usize = 1024 * 1024;

/// Highlighted chunks kept around for scrolling back and forth.
const CHUNK_CACHE: usize = 8;
const MAX_MATCHES: usize = 100_000;
/// Lines a filter shows at most.
const MAX_FILTER_LINES: usize = 1_000_000;
/// Frames to wait for the line list's first layout before dropping a scroll.
const MAX_SCROLL_RETRIES: u8 = 5;
/// Markdown bigger than this opens as source; rendering it all would be slow.
const MAX_PREVIEW_BYTES: usize = 4 * 1024 * 1024;
/// Plain text above this keeps the line-based view (it's virtualized; prose isn't).
const MAX_PROSE_BYTES: usize = 256 * 1024;

/// Files shown as rendered pages rather than numbered lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RichKind {
    /// `.md`: rendered Markdown.
    Markdown,
    /// `.txt`: Notes-style prose — proportional font, wrapped, no line numbers.
    Prose,
    /// `.json`: pretty-printed (formatted view of minified JSON).
    Json,
    /// `.csv` / `.tsv`: columns.
    Table,
    /// A Kanban board in Markdown (Obsidian Kanban format).
    Board,
}

/// Markdown and plain text: shown and edited as pages, not numbered code.
pub fn is_page_file(path: &Path) -> bool {
    matches!(rich_kind(path), Some(RichKind::Markdown | RichKind::Prose))
}

fn rich_kind(path: &Path) -> Option<RichKind> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase());
    match ext.as_deref() {
        Some("md" | "markdown" | "mdown" | "mkd" | "mdx") => Some(RichKind::Markdown),
        Some("txt" | "text") => Some(RichKind::Prose),
        Some("json" | "geojson" | "jsonc" | "webmanifest") => Some(RichKind::Json),
        Some("csv" | "tsv" | "tab") => Some(RichKind::Table),
        _ => None,
    }
}

/// How Notes shows a note: the first line is the title, and every Return is
/// a line break (plain Markdown would join lines into one paragraph). Fenced
/// code blocks are left alone.
fn note_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    let mut in_fence = false;
    let mut titled = false;
    let lines: Vec<&str> = text.lines().collect();
    for (ix, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        if !titled && !trimmed.is_empty() {
            titled = true;
            if !trimmed.starts_with('#') && !in_fence {
                out.push_str("# ");
                out.push_str(trimmed);
                out.push_str("\n\n");
                continue;
            }
        }
        out.push_str(line);
        let next_has_text = lines.get(ix + 1).is_some_and(|l| !l.trim().is_empty());
        if !in_fence && !trimmed.is_empty() && next_has_text {
            out.push_str("  "); // two trailing spaces: a hard line break
        }
        out.push('\n');
    }
    out
}

/// Escapes plain text so the Markdown renderer shows it verbatim: every ASCII
/// punctuation mark is backslash-escaped, leading indentation becomes
/// non-breaking spaces, and single newlines become hard line breaks.
fn plain_to_markdown(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for (ix, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            out.push('\n');
            continue;
        }
        let body = line.trim_start();
        for ch in line[..line.len() - body.len()].chars() {
            let width = if ch == '\t' { crate::text_index::TAB_WIDTH } else { 1 };
            out.extend(std::iter::repeat_n('\u{a0}', width));
        }
        for ch in body.chars() {
            if ch.is_ascii_punctuation() {
                out.push('\\');
            }
            out.push(ch);
        }
        // A trailing backslash is a hard break only when another line follows.
        let next_has_text = lines.get(ix + 1).is_some_and(|l| !l.trim().is_empty());
        out.push_str(if next_has_text { "\\\n" } else { "\n" });
    }
    out
}

pub fn init(cx: &mut App) {
    board_ui::init(cx);
    let list = Some("Viewer");
    let root = Some("ViewerRoot");
    cx.bind_keys([
        KeyBinding::new("up", LineUp, list),
        KeyBinding::new("down", LineDown, list),
        KeyBinding::new("pageup", PageUp, list),
        KeyBinding::new("pagedown", PageDown, list),
        KeyBinding::new("space", PageDown, list),
        KeyBinding::new("shift-space", PageUp, list),
        KeyBinding::new("home", Top, list),
        KeyBinding::new("cmd-up", Top, list),
        KeyBinding::new("end", Bottom, list),
        KeyBinding::new("cmd-down", Bottom, list),
        KeyBinding::new("cmd-c", CopySelection, list),
        KeyBinding::new("cmd-a", SelectAllText, list),
        KeyBinding::new("escape", Dismiss, list),
        KeyBinding::new("cmd-f", Find, root),
        KeyBinding::new("cmd-g", FindNext, root),
        KeyBinding::new("cmd-shift-g", FindPrev, root),
        KeyBinding::new("cmd-l", GoToLine, root),
        KeyBinding::new("cmd-shift-l", FilterLines, root),
        KeyBinding::new("cmd-shift-v", TogglePreview, root),
        KeyBinding::new("escape", Dismiss, Some("ViewerBar")),
        KeyBinding::new("cmd-=", ZoomIn, root),
        KeyBinding::new("cmd-+", ZoomIn, root),
        KeyBinding::new("cmd--", ZoomOut, root),
        KeyBinding::new("cmd-0", ZoomReset, root),
        KeyBinding::new("alt-z", ToggleWrap, root),
    ]);
}

/// A link in a rendered Markdown page was clicked.
#[derive(Action, Clone, PartialEq, serde::Deserialize)]
#[action(namespace = viewer, no_json)]
pub struct OpenLink(pub String);

/// A file reference was ⌘-clicked (or a relative Markdown link): the
/// workspace opens it, at `line` (1-based) when given.
pub struct OpenLocation {
    pub path: PathBuf,
    pub line: Option<usize>,
}

/// The word or path under byte column `col`: a run without spaces, quotes or
/// brackets, with trailing punctuation removed (`src/a.rs:12:5`, `https://…`).
fn link_token(line: &str, col: usize) -> Option<&str> {
    let is_break = |c: char| c.is_whitespace() || "\"'`()<>[]{},|=;".contains(c);
    let col = col.min(line.len());
    if !line.is_char_boundary(col) {
        return None;
    }
    let start = line[..col].rfind(is_break).map_or(0, |i| i + line[i..].chars().next().map_or(1, char::len_utf8));
    let end = line[col..].find(is_break).map_or(line.len(), |i| col + i);
    let token = line[start..end].trim_end_matches(['.', ':', '!', '?']);
    (!token.is_empty()).then_some(token)
}

/// `a/b.rs:42:7` → (`a/b.rs`, Some(42)).
fn split_reference(token: &str) -> (&str, Option<usize>) {
    let mut path = token;
    let mut line = None;
    for _ in 0..2 {
        match path.rsplit_once(':') {
            Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
                line = tail.parse().ok().or(line);
                path = head;
            }
            _ => break,
        }
    }
    (path, line)
}

/// A position in the source: line, and byte column within that line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

struct FindBar {
    input: Entity<InputState>,
    /// Absolute byte ranges, sorted.
    matches: Arc<Vec<Range<usize>>>,
    current: Option<usize>,
    searching: Option<Task<()>>,
    _events: Subscription,
}

/// "Show only lines matching…": the list shows `lines` instead of every line.
struct FilterBar {
    input: Entity<InputState>,
    /// Matching line numbers; `None` while the query is empty (show all).
    lines: Option<Arc<Vec<usize>>>,
    truncated: bool,
    /// Keep the view at the bottom when new results arrive (log tailing).
    stick_to_bottom: bool,
    running: Option<Task<()>>,
    _events: Subscription,
}

struct GotoBar {
    input: Entity<InputState>,
    _events: Subscription,
}

pub struct Viewer {
    doc: Arc<Document>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    /// Parser state at the start of chunk `k` (line `k * CHUNK`).
    /// The file's highlighting thread (None: plain text).
    highlighter: Option<highlight::Worker>,
    /// `(chunk, final, spans)`; a non-final entry gets replaced by exact colors.
    chunk_cache: VecDeque<(usize, bool, Rc<Vec<Spans>>)>,
    /// Chunks asked for whose final colors haven't arrived.
    pending_chunks: HashSet<usize>,
    /// Bumped when the document is replaced, to drop stale results.
    generation: u64,
    _highlight_results: Option<Task<()>>,
    anchor: Option<Pos>,
    head: Pos,
    dragging: bool,
    find: Option<FindBar>,
    goto: Option<GotoBar>,
    filter: Option<FilterBar>,
    flash_line: Option<usize>,
    /// `(line, center)` to scroll to on the next render.
    pending_scroll: Option<(usize, bool)>,
    pending_scroll_retries: u8,
    // Set in `render`, used by hit-testing.
    row_height: Pixels,
    gutter_width: Pixels,
    font: Font,
    font_size: Pixels,
    char_width: Pixels,
    rich: Option<RichKind>,
    /// Markdown being rendered while the page view (not numbered lines) shows.
    preview: Option<SharedString>,
    /// Folder of the file; relative image paths in Markdown resolve against it.
    base_dir: PathBuf,
    /// While formatted JSON is shown: the file as it is on disk. `doc` then
    /// holds the pretty-printed copy; edits and saves use this one.
    raw: Option<Arc<Document>>,
    _format_task: Option<Task<()>>,
    /// CSV/TSV shown as columns.
    table: Option<TableLayout>,
    /// Where a double-click asked to start editing; read by the workspace.
    edit_at: Option<Pos>,
    /// In the Leaf Notes folder: rendered the way Notes shows notes.
    note: bool,
    /// The file is a Kanban board: shown as columns of cards.
    board: Option<BoardState>,
    /// The rendered page's state and the text it shows.
    page_state: Option<(SharedString, Entity<TextViewState>)>,
    /// Code or log shown wrapped, as a page (⌥Z).
    wrap: bool,
    /// Plain files (logs): tint ERROR / WARN / DEBUG lines.
    log_levels: bool,
    /// (document, words) for the toolbar's word count.
    word_cache: std::cell::Cell<Option<(usize, usize)>>,
}

impl Viewer {
    pub fn new(doc: Arc<Document>, path: &Path, cx: &mut Context<Self>) -> Self {
        let syntax = (doc.len() <= highlight::MAX_HIGHLIGHT_BYTES)
            .then(|| highlight::find_syntax(path, &doc.text(doc.line_range(0))))
            .flatten();
        let log_levels = syntax.is_none() && rich_kind(path).is_none();
        let mut this = Self {
            doc,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            highlighter: None,
            chunk_cache: VecDeque::new(),
            pending_chunks: HashSet::new(),
            generation: 0,
            _highlight_results: None,
            anchor: None,
            head: Pos::default(),
            dragging: false,
            find: None,
            goto: None,
            filter: None,
            flash_line: None,
            pending_scroll: None,
            pending_scroll_retries: 0,
            row_height: px(20.),
            gutter_width: px(60.),
            font: font("Menlo"),
            font_size: px(13.),
            char_width: px(8.),
            rich: rich_kind(path),
            preview: None,
            base_dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
            edit_at: None,
            raw: None,
            _format_task: None,
            table: None,
            note: crate::notes::is_note(path),
            board: None,
            page_state: None,
            wrap: false,
            log_levels,
            word_cache: std::cell::Cell::new(None),
        };
        if this.rich == Some(RichKind::Markdown) {
            this.board = BoardState::detect(&this.doc);
        }
        // Viewer-first: Markdown opens rendered and text reads like Notes;
        // ⌘⇧V switches to numbered source lines.
        if this.board.is_none() {
            this.preview = this.build_preview();
        }
        match this.rich {
            Some(RichKind::Table) => this.table = TableLayout::detect(&this.doc, path),
            // Minified JSON (one huge line) opens formatted; tidy JSON as is.
            Some(RichKind::Json) if this.doc.line_range(this.doc.longest_line()).len() > 1000 => {
                this.show_formatted_json(cx)
            }
            _ => {}
        }
        if let Some(syntax) = syntax {
            let (tx, mut rx) = futures::channel::mpsc::unbounded();
            this.highlighter = Some(highlight::Worker::spawn(this.doc.clone(), syntax, tx));
            this._highlight_results = Some(cx.spawn(async move |viewer, cx| {
                while let Some(result) = rx.next().await {
                    if viewer.update(cx, |viewer, cx| viewer.on_highlighted(result, cx)).is_err() {
                        break;
                    }
                }
            }));
        }
        this
    }

    fn build_preview(&self) -> Option<SharedString> {
        let text = || self.doc.text(0..self.doc.len());
        if self.wrap && self.rich.is_none() {
            return (self.doc.len() <= MAX_WRAP_BYTES).then(|| plain_to_markdown(&text()).into());
        }
        match self.rich? {
            RichKind::Markdown if self.doc.len() <= MAX_PREVIEW_BYTES => Some(if self.note {
                page::taskify(&note_markdown(&text())).into()
            } else {
                page::taskify(&text()).into()
            }),
            RichKind::Prose if self.doc.len() <= MAX_PROSE_BYTES => Some(plain_to_markdown(&text()).into()),
            _ => None,
        }
    }

    /// Showing a rendered page (Markdown or prose) rather than numbered lines.
    pub fn page_kind(&self) -> Option<RichKind> {
        if self.board.as_ref().is_some_and(|b| b.visible) {
            return Some(RichKind::Board);
        }
        match self.rich? {
            RichKind::Markdown | RichKind::Prose => self.preview.as_ref().and(self.rich),
            RichKind::Json => self.raw.as_ref().and(self.rich),
            RichKind::Table => self.table.as_ref().and(self.rich),
            RichKind::Board => None,
        }
    }

    /// "3 columns · 9 cards" for a board, for the toolbar.
    pub fn board_summary(&self) -> Option<String> {
        self.board.as_ref().map(|b| b.summary())
    }

    pub fn can_toggle_page(&self) -> bool {
        self.rich.is_some()
    }

    fn on_toggle_preview(&mut self, _: &TogglePreview, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_page(cx);
    }

    /// Switches between the rendered page and numbered source lines.
    pub fn toggle_page(&mut self, cx: &mut Context<Self>) {
        if let Some(board) = self.board.as_mut() {
            board.visible = !board.visible;
            cx.notify();
            return;
        }
        match self.rich {
            Some(RichKind::Json) => match self.raw.take() {
                Some(raw) => self.swap_doc(raw),
                None => self.show_formatted_json(cx),
            },
            Some(RichKind::Table) => {
                self.table = match self.table.take() {
                    Some(_) => None,
                    None => TableLayout::detect(&self.doc, Path::new(".csv")),
                };
            }
            _ => {
                if self.preview.take().is_none() {
                    self.preview = self.build_preview();
                }
            }
        }
        cx.notify();
    }

    /// Pretty-prints the JSON in the background and shows that instead.
    fn show_formatted_json(&mut self, cx: &mut Context<Self>) {
        let doc = self.doc.clone();
        self._format_task = Some(cx.spawn(async move |this, cx| {
            let pretty = cx.background_executor().spawn(async move { pretty_json(&doc) }).await;
            this.update(cx, |viewer, cx| {
                if let Some(text) = pretty {
                    let raw = viewer.doc.clone();
                    viewer.swap_doc(Arc::new(Document::new(Bytes::Owned(text), false)));
                    viewer.raw = Some(raw);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Shows a different text (formatted / original) from the top.
    fn swap_doc(&mut self, doc: Arc<Document>) {
        self.doc = doc;
        self.generation += 1;
        self.chunk_cache.clear();
        self.pending_chunks.clear();
        if let Some(highlighter) = &self.highlighter {
            highlighter.set_document(self.doc.clone(), self.generation, 0);
        }
        (self.find, self.filter, self.anchor) = (None, None, None);
        self.head = Pos::default();
        self.pending_scroll = Some((0, false));
    }

    fn on_zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        set_font_scale_percent(font_scale_percent() + 10);
        cx.notify();
    }

    fn on_zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        set_font_scale_percent(font_scale_percent().saturating_sub(10));
        cx.notify();
    }

    fn on_zoom_reset(&mut self, _: &ZoomReset, _: &mut Window, cx: &mut Context<Self>) {
        set_font_scale_percent(100);
        cx.notify();
    }

    /// ⌥Z: code and logs wrapped to the window (as a page), or back to lines.
    fn on_toggle_wrap(&mut self, _: &ToggleWrap, _: &mut Window, cx: &mut Context<Self>) {
        if self.rich.is_some() || self.doc.len() > MAX_WRAP_BYTES {
            return; // pages already wrap; huge files keep their lines
        }
        self.wrap = !self.wrap;
        self.preview = if self.wrap { self.build_preview() } else { None };
        cx.notify();
    }

    fn render_preview(&self, state: Entity<TextViewState>, cx: &mut Context<Self>) -> impl IntoElement {
        let base = self.base_dir.clone();
        let size = cx.theme().font_size * font_scale();
        let mono = self.wrap.then(|| cx.theme().mono_font_family.clone());
        div()
            .id("preview")
            .text_size(size)
            .when_some(mono, |el, family| el.font_family(family))
            // Double-click anywhere on the page to start editing, like Notes.
            .on_action(cx.listener(Self::on_open_link))
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                window.focus(&this.focus, cx);
                if event.click_count == 2 {
                    // The page selects the word first; read it on the next frame.
                    cx.spawn_in(window, async move |this, cx| {
                        this.update_in(cx, |this, window, cx| this.edit_at_page_selection(window, cx)).ok();
                    })
                    .detach();
                }
            }))
            .key_context("Viewer")
            .track_focus(&self.focus)
            .flex_1()
            .min_h_0()
            .child(
                TextView::new(&state)
                    .on_link_click(|url, _, window, cx| window.dispatch_action(Box::new(OpenLink(url.to_string())), cx))
                    .selectable(true)
                    .selection_format(SelectionFormat::Plain)
                    .scrollable(true)
                    .image_source(move |uri| {
                        let uri_text = uri.to_string();
                        if uri_text.contains("://") || uri_text.starts_with("data:") {
                            ImageSource::from(uri.clone())
                        } else {
                            ImageSource::from(base.join(uri_text.trim_start_matches("./")))
                        }
                    })
                    .size_full()
                    .px(px(56.))
                    .pb_10(),
            )
    }

    /// The file as it is on disk (not the formatted copy).
    pub fn doc(&self) -> &Arc<Document> {
        self.raw.as_ref().unwrap_or(&self.doc)
    }

    /// Swaps in a newer version of the same file (it grew on disk). If the
    /// reader was at the bottom, the view follows the new lines, like `tail -f`.
    pub fn replace_doc(&mut self, doc: Arc<Document>, cx: &mut Context<Self>) {
        if self.raw.is_some() {
            // Formatted JSON: re-format the new version.
            self.raw = None;
            self.swap_doc(doc);
            self.show_formatted_json(cx);
            return;
        }
        let old_lines = self.doc.line_count();
        let at_bottom = {
            let handle = self.scroll.0.borrow().base_handle.clone();
            -handle.offset().y + self.row_height >= handle.max_offset().y
        };
        self.doc = doc;
        // Highlighting before the old last chunk is still right; redo the rest.
        let last_chunk = (old_lines - 1) / CHUNK;
        self.generation += 1;
        self.chunk_cache.retain(|(chunk, ..)| *chunk < last_chunk);
        self.pending_chunks.clear();
        if let Some(highlighter) = &self.highlighter {
            highlighter.set_document(self.doc.clone(), self.generation, last_chunk + 1);
        }
        if self.preview.is_some() {
            self.preview = self.build_preview();
        }
        self.reparse_board();
        if let Some(filter) = self.filter.as_mut() {
            filter.stick_to_bottom = at_bottom;
            self.run_filter(cx);
        } else if at_bottom {
            // Deferred by the list until it has laid out the new rows.
            self.scroll.scroll_to_item(self.doc.line_count() - 1, ScrollStrategy::Bottom);
        }
        if self.find.is_some() {
            self.run_search(cx);
        }
        cx.notify();
    }

    /// Rows the list shows: every line, or only those a filter matched.
    fn filtered(&self) -> Option<&Arc<Vec<usize>>> {
        self.filter.as_ref().and_then(|f| f.lines.as_ref())
    }

    fn row_count(&self) -> usize {
        self.filtered().map_or(self.doc.line_count(), |lines| lines.len().max(1))
    }

    fn line_of_row(&self, row: usize) -> usize {
        match self.filtered() {
            Some(lines) => lines.get(row).or(lines.last()).copied().unwrap_or(0),
            None => row,
        }
    }

    /// The row showing `line`, or the nearest one after it when filtered.
    fn row_of_line(&self, line: usize) -> usize {
        match self.filtered() {
            Some(lines) => lines.partition_point(|&l| l < line).min(lines.len().saturating_sub(1)),
            None => line,
        }
    }

    pub fn top_line(&self) -> usize {
        let offset = self.scroll.0.borrow().base_handle.offset();
        let row = ((-offset.y / self.row_height).max(0.) as usize).min(self.row_count() - 1);
        self.line_of_row(row)
    }

    pub fn scroll_to_line(&mut self, line: usize, cx: &mut Context<Self>) {
        self.pending_scroll = Some((line, false));
        cx.notify();
    }

    /// Applies a requested scroll once the viewport has been laid out.
    fn apply_pending_scroll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((line, center)) = self.pending_scroll else { return };
        // The line list isn't laid out while a page is showing; the scroll
        // waits until the source lines are shown (it used to re-render every
        // frame here, forever).
        if self.preview.is_some() {
            return;
        }
        let handle = self.scroll.0.borrow().base_handle.clone();
        let viewport = handle.bounds().size.height;
        if viewport <= px(0.) {
            // Not laid out yet: retry on the next frame, but only a few times.
            self.pending_scroll_retries += 1;
            if self.pending_scroll_retries > MAX_SCROLL_RETRIES {
                self.pending_scroll = None;
            } else {
                cx.on_next_frame(window, |_, _, cx| cx.notify());
            }
            return;
        }
        self.pending_scroll_retries = 0;
        let content = self.row_height * self.row_count() as f32;
        let line = self.row_of_line(line);
        let max = (content - viewport).max(px(0.));
        let mut y = self.row_height * line.min(self.row_count() - 1) as f32;
        if center {
            y -= (viewport - self.row_height) / 2.;
        }
        handle.set_offset(point(handle.offset().x, -y.clamp(px(0.), max)));
        self.pending_scroll = None;
    }

    /// One-line summary for the status bar.
    pub fn status(&self, cx: &App) -> String {
        let mut parts = Vec::new();
        if let Some(words) = self.word_status(cx).filter(|_| self.preview.is_some()) {
            parts.push(words);
        }
        if let Some((start, end)) = self.selection() {
            let chars = end.line - start.line + 1;
            parts.push(if start.line == end.line {
                format!("Ln {}, Col {}", end.line + 1, end.col + 1)
            } else {
                format!("{chars} lines selected")
            });
        }
        if !self.pending_chunks.is_empty() {
            parts.push("highlighting…".into());
        }
        if let Some(lines) = self.filtered() {
            parts.push(format!("filtered: {} lines", lines.len()));
        }
        parts.join("  ·  ")
    }

    // ── syntax highlighting ────────────────────────────────────────────────

    /// Cached colors for `chunk`, or `None` (plain text) while the
    /// highlighter thread works on them.
    fn chunk_spans(&mut self, chunk: usize) -> Option<Rc<Vec<Spans>>> {
        let highlighter = self.highlighter.as_ref()?;
        if let Some((_, _, spans)) = self.chunk_cache.iter().find(|(c, ..)| *c == chunk) {
            return Some(spans.clone());
        }
        if self.pending_chunks.insert(chunk) {
            highlighter.request(chunk);
        }
        None
    }

    fn on_highlighted(&mut self, result: highlight::Highlighted, cx: &mut Context<Self>) {
        if result.generation != self.generation {
            return;
        }
        if result.last {
            self.pending_chunks.remove(&result.chunk);
        }
        self.chunk_cache.retain(|(c, ..)| *c != result.chunk);
        self.chunk_cache.push_front((result.chunk, result.last, Rc::new(result.spans)));
        self.chunk_cache.truncate(CHUNK_CACHE);
        cx.notify();
    }

    // ── rows ───────────────────────────────────────────────────────────────

    fn render_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.theme();
        let syntax_theme = theme.highlight_theme.clone();
        let (muted, selection_bg) = (theme.muted_foreground, theme.selection);
        let match_bg = theme.warning.opacity(0.25);
        let current_bg = theme.warning.opacity(0.6);
        let flash_bg = theme.accent;
        let (row_height, gutter_width) = (self.row_height, self.gutter_width);

        let selection = self.selection();
        let find = self.find.as_ref().map(|f| (f.matches.clone(), f.current));
        // A selected word is marked wherever else it appears on screen.
        let word: Option<Vec<u8>> = selection.filter(|(s, e)| s.line == e.line && e.col > s.col + 1).and_then(|(s, e)| {
            let text = self.doc.slice(self.doc.line_range(s.line));
            let word = text.get(s.col..e.col)?.to_vec();
            (word.len() <= 64 && word.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b >= 0x80)).then_some(word)
        });
        let word_bg = theme.selection.opacity(0.45);
        let dark = theme.mode.is_dark();
        let level_colors = [
            if dark { rgb(0xf28b82) } else { rgb(0xc5221f) },
            if dark { rgb(0xf0cf7a) } else { rgb(0x8a6212) },
        ];
        let lines: Vec<(usize, Range<usize>)> = match self.filtered() {
            Some(filtered) => filtered
                .get(range.start.min(filtered.len())..range.end.min(filtered.len()))
                .unwrap_or_default()
                .iter()
                .map(|&line| (line, self.doc.line_range(line)))
                .collect(),
            None => range.clone().zip(self.doc.line_ranges(range.clone())).collect(),
        };
        let mut rows = Vec::with_capacity(lines.len());

        for (line, source) in lines {
            let display = DisplayLine::new(&self.doc.slice(source.clone()));
            let to_display = |r: Range<usize>| display.source_to_display(r.start)..display.source_to_display(r.end);

            let mut colors = Vec::new();
            if let Some(spans) = self.chunk_spans(line / CHUNK) {
                for (span, name) in spans.get(line % CHUNK).into_iter().flatten() {
                    if let Some(style) = syntax_theme.style(name) {
                        colors.push((to_display(span.clone()), style));
                    }
                }
            }

            if self.log_levels && colors.is_empty() {
                if let Some(level) = log_level(&self.doc.slice(source.start..(source.start + 256).min(source.end))) {
                    let color = match level {
                        LogLevel::Error => level_colors[0].into(),
                        LogLevel::Warn => level_colors[1].into(),
                        LogLevel::Debug => muted,
                    };
                    colors.push((0..display.text.len(), HighlightStyle { color: Some(color), ..Default::default() }));
                }
            }

            let mut backgrounds = Vec::new();
            if let Some(word) = &word {
                let text = self.doc.slice(source.clone());
                let is_word = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
                for at in memchr::memmem::find_iter(&text, word) {
                    let selected_here = selection.is_some_and(|(s, _)| s.line == line && s.col == at);
                    if !selected_here && !is_word(at.checked_sub(1).and_then(|i| text.get(i))) && !is_word(text.get(at + word.len())) {
                        backgrounds.push((to_display(at..at + word.len()), word_bg));
                    }
                }
            }
            if let Some((matches, current)) = &find {
                let first = matches.partition_point(|m| m.end <= source.start);
                for (ix, m) in matches.iter().enumerate().skip(first) {
                    if m.start > source.end || (m.start == source.end && !m.is_empty()) {
                        break;
                    }
                    let local = m.start.max(source.start) - source.start..m.end.min(source.end) - source.start;
                    let bg = if Some(ix) == *current { current_bg } else { match_bg };
                    backgrounds.push((to_display(local), bg));
                }
            }
            if let Some((start, end)) = selection.filter(|(s, e)| (s.line..=e.line).contains(&line)) {
                let from = if line == start.line { start.col } else { 0 };
                let to = if line == end.line { end.col } else { source.len() };
                backgrounds.push((to_display(from..to), selection_bg));
            }

            if let Some(table) = &self.table {
                let selected = selection.is_some_and(|(s, e)| (s.line..=e.line).contains(&line));
                rows.push(self.render_table_row(table, line, &self.doc.slice(source.clone()), selected, muted, selection_bg));
                continue;
            }
            let highlights = compose(&display.text, &colors, &backgrounds);
            rows.push(
                h_flex()
                    .id(line)
                    .h(row_height)
                    .whitespace_nowrap()
                    .when(self.flash_line == Some(line), |row| row.bg(flash_bg))
                    .child(
                        div()
                            .flex_none()
                            .w(gutter_width)
                            .pr_4()
                            .text_right()
                            .text_color(muted.opacity(0.6))
                            .child(SharedString::from((line + 1).to_string())),
                    )
                    .child(StyledText::new(display.text).with_highlights(highlights))
                    .into_any_element(),
            );
        }
        rows
    }

    fn render_table_row(
        &self,
        table: &TableLayout,
        line: usize,
        raw: &[u8],
        selected: bool,
        muted: Hsla,
        selection_bg: Hsla,
    ) -> AnyElement {
        let header = line == 0;
        let cells = split_row(&String::from_utf8_lossy(raw), table.delimiter);
        h_flex()
            .id(line)
            .h(self.row_height)
            .whitespace_nowrap()
            .when(selected, |row| row.bg(selection_bg))
            .when(header, |row| row.font_weight(FontWeight::SEMIBOLD))
            .child(
                div()
                    .flex_none()
                    .w(self.gutter_width)
                    .pr_4()
                    .text_right()
                    .text_color(muted.opacity(0.6))
                    .child(SharedString::from((line + 1).to_string())),
            )
            .children(table.widths.iter().enumerate().map(|(column, chars)| {
                let text = cells.get(column).cloned().unwrap_or_default();
                div()
                    .flex_none()
                    .w(self.char_width * (*chars as f32 + 2.))
                    .h_full()
                    .px_2()
                    .border_r_1()
                    .border_color(muted.opacity(0.25))
                    .when(header, |cell| cell.border_b_1())
                    .overflow_hidden()
                    .child(text)
            }))
            .into_any_element()
    }

    // ── selection & mouse ──────────────────────────────────────────────────

    fn selection(&self) -> Option<(Pos, Pos)> {
        let anchor = self.anchor.filter(|a| *a != self.head)?;
        Some((anchor.min(self.head), anchor.max(self.head)))
    }

    fn pos_of_offset(&self, offset: usize) -> Pos {
        let line = self.doc.line_of_offset(offset);
        Pos { line, col: offset.saturating_sub(self.doc.line_range(line).start) }
    }

    fn offset_of(&self, pos: Pos) -> usize {
        let range = self.doc.line_range(pos.line);
        (range.start + pos.col).min(range.end)
    }

    fn open_reference_at(&mut self, pos: Pos, cx: &mut Context<Self>) -> bool {
        let line = self.doc.text(self.doc.line_range(pos.line)).into_owned();
        let Some(token) = link_token(&line, pos.col) else { return false };
        self.open_reference(token, cx)
    }

    /// Opens a URL in the browser, or a file path (relative to this file's
    /// folder or any folder above it) in Leaf.
    fn open_reference(&mut self, token: &str, cx: &mut Context<Self>) -> bool {
        if ["http://", "https://", "mailto:"].iter().any(|p| token.starts_with(p)) {
            cx.open_url(token);
            return true;
        }
        let token = token.split('#').next().unwrap_or(token).replace("%20", " ");
        let (path, line) = split_reference(&token);
        let path = path.strip_prefix("file://").unwrap_or(path);
        let candidates: Vec<PathBuf> = if let Some(rest) = path.strip_prefix("~/") {
            dirs::home_dir().map(|h| h.join(rest)).into_iter().collect()
        } else if Path::new(path).is_absolute() {
            vec![PathBuf::from(path)]
        } else {
            self.base_dir.ancestors().take(8).map(|dir| dir.join(path.trim_start_matches("./"))).collect()
        };
        match candidates.into_iter().find(|p| p.is_file()) {
            Some(path) => {
                cx.emit(OpenLocation { path, line });
                true
            }
            None => false,
        }
    }

    fn on_open_link(&mut self, link: &OpenLink, _: &mut Window, cx: &mut Context<Self>) {
        let target = link.0.clone();
        if let Some(line) = Self::task_link(&target) {
            self.toggle_task(line, cx);
            return;
        }
        if !self.open_reference(&target, cx) && target.contains("://") {
            cx.open_url(&target);
        }
    }

    /// Shows `line` (0-based): scrolls the line view, or the rendered page.
    pub fn go_to_line(&mut self, line: usize, cx: &mut Context<Self>) {
        if self.preview.is_some() && self.reveal_in_page(line, cx) {
            return;
        }
        self.flash_line = Some(line);
        self.pending_scroll = Some((line, true));
        cx.notify();
    }

    fn hit_test(&self, point: Point<Pixels>, window: &mut Window) -> Pos {
        let (bounds, offset) = {
            let state = self.scroll.0.borrow();
            (state.base_handle.bounds(), state.base_handle.offset())
        };
        let y = point.y - bounds.top() - offset.y;
        let row = ((y / self.row_height).floor().max(0.) as usize).min(self.row_count() - 1);
        let line = self.line_of_row(row);
        let x = point.x - bounds.left() - offset.x - self.gutter_width;
        let raw = self.doc.slice(self.doc.line_range(line));
        let display = DisplayLine::new(&raw);
        let col = if x <= px(0.) || display.text.is_empty() {
            0
        } else {
            let mut run = window.text_style().to_run(display.text.len());
            run.font = self.font.clone();
            let layout = window.text_system().layout_line(&display.text, self.font_size, &[run], None);
            layout.closest_index_for_x(x)
        };
        Pos { line, col: display.display_to_source(col) }
    }

    /// The position a double-click asked to edit at, if any (taken once).
    pub fn take_edit_at(&mut self) -> Option<Pos> {
        self.edit_at.take()
    }

    fn request_edit(&mut self, at: Option<Pos>, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_at = at;
        window.dispatch_action(Box::new(crate::ToggleEdit), cx);
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let pos = self.hit_test(event.position, window);
        // ⌘-click opens a URL or a file reference (`src/main.rs:42`).
        if event.modifiers.platform && event.click_count == 1 && self.open_reference_at(pos, cx) {
            return;
        }
        self.flash_line = None;
        match event.click_count {
            2 => {
                // Double-click edits, with the cursor where you clicked.
                self.anchor = None;
                self.request_edit(Some(pos), window, cx);
                return;
            }
            3 => {
                let len = self.doc.line_range(pos.line).len();
                (self.anchor, self.head) = (Some(Pos { line: pos.line, col: 0 }), Pos { line: pos.line, col: len });
            }
            _ => {
                if event.modifiers.shift {
                    self.anchor.get_or_insert(self.head);
                } else {
                    self.anchor = Some(pos);
                }
                self.head = pos;
                self.dragging = true;
            }
        }
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.dragging || event.pressed_button != Some(MouseButton::Left) {
            self.dragging = false;
            return;
        }
        let bounds = self.scroll.0.borrow().base_handle.bounds();
        if event.position.y < bounds.top() {
            self.scroll_by(-1., cx);
        } else if event.position.y > bounds.bottom() {
            self.scroll_by(1., cx);
        }
        self.head = self.hit_test(event.position, window);
        cx.notify();
    }

    // ── scrolling ──────────────────────────────────────────────────────────

    fn scroll_by(&mut self, rows: f32, cx: &mut Context<Self>) {
        let handle = self.scroll.0.borrow().base_handle.clone();
        let mut offset = handle.offset();
        let max = handle.max_offset();
        offset.y = (offset.y - self.row_height * rows).clamp(-max.y, px(0.));
        handle.set_offset(offset);
        cx.notify();
    }

    fn page_rows(&self) -> f32 {
        let height = self.scroll.0.borrow().base_handle.bounds().size.height;
        (height / self.row_height - 2.).max(1.)
    }

    fn on_line_up(&mut self, _: &LineUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-1., cx);
    }
    fn on_line_down(&mut self, _: &LineDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(1., cx);
    }
    fn on_page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-self.page_rows(), cx);
    }
    fn on_page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(self.page_rows(), cx);
    }
    fn on_top(&mut self, _: &Top, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-f32::MAX / 4., cx);
    }
    fn on_bottom(&mut self, _: &Bottom, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(f32::MAX / 4., cx);
    }

    // ── clipboard ──────────────────────────────────────────────────────────

    fn on_copy(&mut self, _: &CopySelection, _: &mut Window, cx: &mut Context<Self>) {
        let Some((start, end)) = self.selection() else { return };
        let text = match self.filtered().filter(|_| start.line != end.line) {
            // Filtered: copy what's shown, not the hidden lines in between.
            Some(lines) => lines[lines.partition_point(|&l| l < start.line)..lines.partition_point(|&l| l <= end.line)]
                .iter()
                .map(|&line| {
                    let range = self.doc.line_range(line);
                    let from = if line == start.line { range.start + start.col } else { range.start };
                    let to = if line == end.line { (range.start + end.col).min(range.end) } else { range.end };
                    self.doc.text(from.min(to)..to).into_owned()
                })
                .collect::<Vec<_>>()
                .join("\n"),
            None => self.doc.text(self.offset_of(start)..self.offset_of(end)).into_owned(),
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn on_select_all(&mut self, _: &SelectAllText, _: &mut Window, cx: &mut Context<Self>) {
        self.anchor = Some(Pos::default());
        self.head = self.pos_of_offset(self.doc.len());
        cx.notify();
    }

    // ── find ───────────────────────────────────────────────────────────────

    fn on_find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        if self.board.as_ref().is_some_and(|b| b.visible) {
            self.focus_board_filter(window, cx);
            return;
        }
        self.preview = None; // find works on lines: show the source
        self.filter = None;
        if self.find.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
            let events = cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
                InputEvent::Change => this.run_search(cx),
                InputEvent::PressEnter { shift, .. } => this.step_match(if *shift { -1 } else { 1 }, cx),
                _ => {}
            });
            self.find = Some(FindBar {
                input,
                matches: Arc::default(),
                current: None,
                searching: None,
                _events: events,
            });
        }
        let input = self.find.as_ref().expect("just opened").input.clone();
        // Seed the query from a single-line selection, like most editors.
        if let Some((start, end)) = self.selection().filter(|(s, e)| s.line == e.line) {
            let text = self.doc.text(self.offset_of(start)..self.offset_of(end)).into_owned();
            input.update(cx, |state, cx| state.set_value(text, window, cx));
            self.run_search(cx);
        }
        input.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    /// Opens the find bar with `query` and jumps to the first match (used when
    /// a file is opened from a content search in the sidebar).
    pub fn find_text(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.on_find(&Find, window, cx);
        if let Some(find) = &self.find {
            let input = find.input.clone();
            input.update(cx, |state, cx| state.set_value(query.to_string(), window, cx));
        }
        self.run_search(cx);
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let from = self.doc.line_range(self.top_line()).start;
        let doc = self.doc.clone();
        let Some(find) = self.find.as_mut() else { return };
        let query = find.input.read(cx).value().to_string();
        find.searching = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let matches = cx.background_executor().spawn(async move { search(&doc, &query) }).await;
            this.update(cx, |viewer, cx| {
                if let Some(find) = viewer.find.as_mut() {
                    find.current = (!matches.is_empty())
                        .then(|| matches.partition_point(|m| m.start < from) % matches.len());
                    find.matches = Arc::new(matches);
                }
                viewer.reveal_current_match(cx);
            })
            .ok();
        }));
    }

    fn step_match(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_mut() else { return };
        let n = find.matches.len() as isize;
        if n == 0 {
            return;
        }
        let current = find.current.map_or(0, |c| (c as isize + delta).rem_euclid(n));
        find.current = Some(current as usize);
        self.reveal_current_match(cx);
    }

    fn reveal_current_match(&mut self, cx: &mut Context<Self>) {
        let Some(m) = self.find.as_ref().and_then(|f| f.matches.get(f.current?).cloned()) else {
            cx.notify();
            return;
        };
        let (start, end) = (self.pos_of_offset(m.start), self.pos_of_offset(m.end));
        (self.anchor, self.head) = (Some(start), end);
        let top = self.top_line();
        if start.line < top || start.line as f32 >= top as f32 + self.page_rows() {
            self.pending_scroll = Some((start.line, true));
        }
        cx.notify();
    }

    fn on_find_next(&mut self, _: &FindNext, _: &mut Window, cx: &mut Context<Self>) {
        self.step_match(1, cx);
    }

    fn on_find_prev(&mut self, _: &FindPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.step_match(-1, cx);
    }

    // ── filter ─────────────────────────────────────────────────────────────

    fn on_filter(&mut self, _: &FilterLines, window: &mut Window, cx: &mut Context<Self>) {
        self.preview = None;
        (self.find, self.goto) = (None, None);
        if self.filter.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Show lines containing…  (/regex/)"));
            let events = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.run_filter(cx);
                }
            });
            self.filter = Some(FilterBar {
                input,
                lines: None,
                truncated: false,
                stick_to_bottom: false,
                running: None,
                _events: events,
            });
        }
        let input = self.filter.as_ref().expect("just opened").input.clone();
        input.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    fn run_filter(&mut self, cx: &mut Context<Self>) {
        let keep_line = self.top_line();
        let doc = self.doc.clone();
        let Some(filter) = self.filter.as_mut() else { return };
        let query = filter.input.read(cx).value().to_string();
        filter.running = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            let result = if query.is_empty() {
                None
            } else {
                Some(cx.background_executor().spawn(async move { matching_lines(&doc, &query) }).await)
            };
            this.update(cx, |viewer, cx| {
                let Some(filter) = viewer.filter.as_mut() else { return };
                let stick = std::mem::take(&mut filter.stick_to_bottom);
                (filter.lines, filter.truncated) = match result {
                    Some((lines, truncated)) => (Some(Arc::new(lines)), truncated),
                    None => (None, false),
                };
                if stick {
                    viewer.scroll.scroll_to_item(viewer.row_count() - 1, ScrollStrategy::Bottom);
                } else {
                    viewer.pending_scroll = Some((keep_line, false));
                }
                cx.notify();
            })
            .ok();
        }));
    }

    // ── go to line ─────────────────────────────────────────────────────────

    fn on_goto(&mut self, _: &GoToLine, window: &mut Window, cx: &mut Context<Self>) {
        self.preview = None;
        self.filter = None;
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(format!("Go to line (1–{})", self.doc.line_count()))
        });
        let events = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let value = input.read(cx).value().trim().to_string();
                if let Ok(line) = value.parse::<usize>() {
                    let line = line.clamp(1, this.doc.line_count()) - 1;
                    this.pending_scroll = Some((line, true));
                    this.flash_line = Some(line);
                    (this.anchor, this.head) = (None, Pos { line, col: 0 });
                }
                this.goto = None;
                window.focus(&this.focus, cx);
                cx.notify();
            }
        });
        input.update(cx, |state, cx| state.focus(window, cx));
        self.goto = Some(GotoBar { input, _events: events });
        cx.notify();
    }

    fn on_dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.goto.take().is_some() || self.find.take().is_some() || self.filter.take().is_some() {
            window.focus(&self.focus, cx);
        } else {
            self.anchor = None;
            self.flash_line = None;
        }
        cx.notify();
    }

    fn render_bar(&self, child: impl IntoElement, cx: &mut Context<Self>) -> Div {
        h_flex()
            .key_context("ViewerBar")
            .gap_3()
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .text_sm()
            .child(div().w(px(320.)).child(child))
    }
}

impl EventEmitter<BoardEdited> for Viewer {}
impl EventEmitter<OpenLocation> for Viewer {}

impl Focusable for Viewer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Viewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        self.font = font(theme.mono_font_family.clone());
        self.font_size = theme.mono_font_size * font_scale();
        self.row_height = (self.font_size * 1.5).round();
        let digits = self.doc.line_count().to_string().len().max(3) as f32;
        let char_width = window
            .text_system()
            .advance(window.text_system().resolve_font(&self.font), self.font_size, 'm')
            .map(|size| size.width)
            .unwrap_or(self.font_size * 0.6);
        self.gutter_width = char_width * (digits + 3.);
        self.char_width = char_width;
        self.apply_pending_scroll(window, cx);
        let showing_board = self.board.as_ref().is_some_and(|b| b.visible);
        let page = self.preview.clone().filter(|_| !showing_board).map(|text| self.page_state(&text, cx));
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);

        let list = uniform_list(
            "rows",
            self.row_count(),
            cx.processor(|this, range: Range<usize>, window, cx| this.render_rows(range, window, cx)),
        )
        .track_scroll(&self.scroll)
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .with_width_from_item(if self.table.is_some() {
            Some(0)
        } else {
            self.filtered().is_none().then(|| self.doc.longest_line())
        })
        .size_full();

        v_flex()
            .size_full()
            .key_context("ViewerRoot")
            .on_action(cx.listener(Self::on_find))
            .on_action(cx.listener(Self::on_find_next))
            .on_action(cx.listener(Self::on_find_prev))
            .on_action(cx.listener(Self::on_goto))
            .on_action(cx.listener(Self::on_dismiss))
            .on_action(cx.listener(Self::on_toggle_preview))
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            .on_action(cx.listener(Self::on_zoom_reset))
            .on_action(cx.listener(Self::on_toggle_wrap))
            .on_action(cx.listener(Self::on_filter))
            .when_some(self.goto.as_ref(), |this, goto| {
                this.child(self.render_bar(Input::new(&goto.input).small(), cx))
            })
            .when_some(self.filter.as_ref(), |this, filter| {
                let label = match &filter.lines {
                    None => String::new(),
                    Some(lines) if lines.is_empty() => "No matching lines".into(),
                    Some(lines) => format!(
                        "{}{} of {} lines",
                        lines.len(),
                        if filter.truncated { "+" } else { "" },
                        self.doc.line_count()
                    ),
                };
                this.child(
                    self.render_bar(Input::new(&filter.input).small(), cx)
                        .child(div().text_color(muted).child(label))
                        .child(div().text_xs().text_color(muted).child("esc shows all lines")),
                )
            })
            .when_some(self.find.as_ref(), |this, find| {
                let label = match (find.current, find.matches.len()) {
                    (_, 0) if find.input.read(cx).value().is_empty() => String::new(),
                    (_, 0) => "No matches".into(),
                    (Some(c), n) if n >= MAX_MATCHES => format!("{} of {n}+", c + 1),
                    (Some(c), n) => format!("{} of {n}", c + 1),
                    (None, n) => format!("{n} matches"),
                };
                this.child(
                    self.render_bar(Input::new(&find.input).small(), cx)
                        .child(div().text_color(muted).child(label))
                        .child(div().text_xs().text_color(muted).child("⏎ next · ⇧⏎ previous · esc close")),
                )
            })
            .when(showing_board, |this| this.child(self.render_board(window, cx)))
            .when_some(page, |this, state| this.child(self.render_preview(state, cx)))
            .when(self.preview.is_none() && !showing_board, |this| this.child(
                div()
                    .id("viewer")
                    .key_context("Viewer")
                    .track_focus(&self.focus)
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .border_color(border)
                    .font(self.font.clone())
                    .text_size(self.font_size)
                    .cursor_text()
                    .on_action(cx.listener(Self::on_line_up))
                    .on_action(cx.listener(Self::on_line_down))
                    .on_action(cx.listener(Self::on_page_up))
                    .on_action(cx.listener(Self::on_page_down))
                    .on_action(cx.listener(Self::on_top))
                    .on_action(cx.listener(Self::on_bottom))
                    .on_action(cx.listener(Self::on_copy))
                    .on_action(cx.listener(Self::on_select_all))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    .on_mouse_move(cx.listener(Self::on_mouse_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.dragging = false))
                    .when(self.filtered().is_none_or(|l| !l.is_empty()), |this| this.child(list))
                    .child(Scrollbar::new(&self.scroll)),
            ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum LogLevel {
    Error,
    Warn,
    Debug,
}

/// The level of a log line, from the words near its start: ERROR, WARN, DEBUG…
fn log_level(head: &[u8]) -> Option<LogLevel> {
    let has = |word: &[u8]| memchr::memmem::find(head, word).is_some();
    if has(b"ERROR") || has(b"FATAL") || has(b"PANIC") || has(b"CRITICAL") || has(b"panicked") || has(b"[E]") || has(b"error:") {
        Some(LogLevel::Error)
    } else if has(b"WARN") || has(b"[W]") || has(b"warning:") {
        Some(LogLevel::Warn)
    } else if has(b"DEBUG") || has(b"TRACE") {
        Some(LogLevel::Debug)
    } else {
        None
    }
}

/// Builds the matcher for a find/filter query: literal text with smart case
/// (any uppercase letter → case-sensitive), or a regular expression when the
/// query is written as `/pattern/`.
pub fn build_matcher(query: &str) -> Option<regex::bytes::Regex> {
    if query.is_empty() {
        return None;
    }
    let case_sensitive = query.chars().any(char::is_uppercase);
    let pattern = match query.strip_prefix('/').and_then(|q| q.strip_suffix('/')) {
        Some(pattern) if !pattern.is_empty() => pattern.to_string(),
        _ => regex::escape(query),
    };
    // Multi-line: `^` and `$` match at each line, which is what a line filter means.
    regex::bytes::RegexBuilder::new(&pattern).case_insensitive(!case_sensitive).multi_line(true).build().ok()
}

/// Calls `f` with each match in order until it returns false. Documents on
/// disk are searched in 1 MB chunks that overlap enough for a match to
/// straddle a boundary; each match belongs to the chunk it starts in.
fn for_each_match(doc: &Document, regex: &regex::bytes::Regex, mut f: impl FnMut(Range<usize>) -> bool) {
    if let Some(all) = doc.as_slice() {
        for m in regex.find_iter(all) {
            if !f(m.range()) {
                return;
            }
        }
        return;
    }
    let overlap = 4096;
    let mut pos = 0;
    while pos < doc.len() {
        let end = (pos + crate::text_index::SCAN_CHUNK).min(doc.len());
        let chunk = doc.slice(pos..end + overlap);
        for m in regex.find_iter(&chunk) {
            let start = pos + m.start();
            if start >= end {
                break;
            }
            if !f(start..pos + m.end()) {
                return;
            }
        }
        pos = end;
    }
}

/// Every match of `query` in the document (capped).
fn search(doc: &Document, query: &str) -> Vec<Range<usize>> {
    let Some(regex) = build_matcher(query) else { return Vec::new() };
    let mut found = Vec::new();
    for_each_match(doc, &regex, |m| {
        found.push(m);
        found.len() < MAX_MATCHES
    });
    found
}

/// Lines containing a match of `query`, in order, and whether the list was cut off.
fn matching_lines(doc: &Document, query: &str) -> (Vec<usize>, bool) {
    let Some(regex) = build_matcher(query) else { return (Vec::new(), false) };
    let (mut lines, mut line_end, mut truncated) = (Vec::new(), None::<usize>, false);
    for_each_match(doc, &regex, |m| {
        if line_end.is_some_and(|end| m.start <= end) {
            return true; // another match on a line we already have
        }
        let line = doc.line_of_offset(m.start);
        lines.push(line);
        line_end = Some(doc.line_range(line).end);
        truncated = lines.len() >= MAX_FILTER_LINES;
        !truncated
    });
    (lines, truncated)
}

/// Column layout for CSV/TSV: the delimiter and each column's width in
/// characters, sampled from the first rows.
struct TableLayout {
    delimiter: char,
    widths: Vec<usize>,
}

impl TableLayout {
    const SAMPLE_ROWS: usize = 300;
    const MAX_COLUMN_CHARS: usize = 40;

    fn detect(doc: &Document, path: &Path) -> Option<Self> {
        let tsv = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("tsv") || e.eq_ignore_ascii_case("tab"));
        let first = doc.text(doc.line_range(0)).into_owned();
        let delimiter = if tsv || first.matches('\t').count() > first.matches(',').count() {
            '\t'
        } else if first.matches(';').count() > first.matches(',').count() {
            ';' // European CSV
        } else {
            ','
        };
        let mut widths: Vec<usize> = Vec::new();
        for range in doc.line_ranges(0..Self::SAMPLE_ROWS) {
            for (column, cell) in split_row(&doc.text(range), delimiter).iter().enumerate() {
                let chars = cell.chars().count().clamp(3, Self::MAX_COLUMN_CHARS);
                match widths.get_mut(column) {
                    Some(width) => *width = (*width).max(chars),
                    None => widths.push(chars),
                }
            }
        }
        (widths.len() > 1).then_some(Self { delimiter, widths })
    }
}

/// Splits one CSV line into cells. Quoted cells may contain the delimiter,
/// and a doubled quote inside them stands for one quote character.
fn split_row(line: &str, delimiter: char) -> Vec<String> {
    let (mut cells, mut cell, mut quoted) = (Vec::new(), String::new(), false);
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == delimiter && !quoted => cells.push(std::mem::take(&mut cell)),
            c => cell.push(c),
        }
    }
    cells.push(cell);
    cells
}

/// Pretty-printed JSON (keys in their original order), if `doc` parses.
fn pretty_json(doc: &Document) -> Option<Vec<u8>> {
    const MAX_FORMAT_BYTES: usize = 16 * 1024 * 1024;
    if doc.len() > MAX_FORMAT_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&doc.slice(0..doc.len())).ok()?;
    let mut out = Vec::with_capacity(doc.len() * 2);
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"  ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    serde::Serialize::serialize(&value, &mut serializer).ok()?;
    out.push(b'\n');
    Some(out)
}

/// Flattens color spans and background spans into the sorted, non-overlapping
/// runs `StyledText` expects. Later backgrounds win (selection is pushed last).
fn compose(
    text: &str,
    colors: &[(Range<usize>, HighlightStyle)],
    backgrounds: &[(Range<usize>, Hsla)],
) -> Vec<(Range<usize>, HighlightStyle)> {
    let len = text.len();
    let mut cuts: Vec<usize> = colors
        .iter()
        .flat_map(|(r, _)| [r.start, r.end])
        .chain(backgrounds.iter().flat_map(|(r, _)| [r.start, r.end]))
        .map(|ix| text.floor_char_boundary(ix.min(len)))
        .collect();
    cuts.extend([0, len]);
    cuts.sort_unstable();
    cuts.dedup();
    let mut out: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    for pair in cuts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let color = colors.iter().find(|(r, _)| r.start <= a && a < r.end).map(|(_, s)| *s);
        let background = backgrounds.iter().rev().find(|(r, _)| r.start <= a && a < r.end).map(|(_, c)| *c);
        if color.is_none() && background.is_none() {
            continue;
        }
        let mut style = color.unwrap_or_default();
        if background.is_some() {
            style.background_color = background;
        }
        match out.last_mut() {
            Some((last, last_style)) if last.end == a && *last_style == style => last.end = b,
            _ => out.push((a..b, style)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{LogLevel, compose, link_token, log_level, search, split_reference};
    use crate::text_index::Document;
    use gpui_kit::HighlightStyle;

    #[test]
    fn compose_splits_and_layers() {
        let red = HighlightStyle { color: Some(gpui_kit::red()), ..Default::default() };
        let out = compose("hello world", &[(0..5, red)], &[(3..8, gpui_kit::blue())]);
        let ranges: Vec<_> = out.iter().map(|(r, _)| r.clone()).collect();
        assert_eq!(ranges, vec![0..3, 3..5, 5..8]);
        assert_eq!(out[1].1.color, Some(gpui_kit::red()));
        assert_eq!(out[1].1.background_color, Some(gpui_kit::blue()));
        assert_eq!(out[2].1.color, None);
    }

    #[test]
    fn plain_text_is_escaped_for_markdown() {
        use super::plain_to_markdown;
        let out = plain_to_markdown("# not a heading\n  * not a list\nlast\n\nnext para");
        assert_eq!(out, "\\# not a heading\\\n\u{a0}\u{a0}\\* not a list\\\nlast\n\nnext para\n");
    }

    #[test]
    fn notes_get_a_title_and_line_breaks() {
        let out = super::note_markdown("Weekend plans\nHike\nLunch\n\n```\na\nb\n```");
        assert_eq!(out, "# Weekend plans\n\nHike  \nLunch\n\n```\na\nb\n```\n");
    }

    #[test]
    fn filters_lines_by_text_and_regex() {
        use crate::text_index::Bytes;
        let doc = Document::new(Bytes::Owned(b"INFO a\nERROR b\nINFO c error\nWARN d\n".to_vec()), true);
        assert_eq!(super::matching_lines(&doc, "error"), (vec![1, 2], false));
        assert_eq!(super::matching_lines(&doc, "ERROR"), (vec![1], false));
        assert_eq!(super::matching_lines(&doc, "/^(WARN|ERROR)/"), (vec![1, 3], false));
        assert_eq!(super::matching_lines(&doc, ""), (vec![], false));
    }

    #[test]
    fn splits_csv_rows_with_quotes() {
        assert_eq!(super::split_row(r#"a,"b,c",d"#, ','), ["a", "b,c", "d"]);
        assert_eq!(super::split_row(r#"x;"say ""hi""""#, ';'), ["x", r#"say "hi""#]);
        assert_eq!(super::split_row("", ','), [""]);
    }

    #[test]
    fn formats_json_keeping_key_order() {
        use crate::text_index::Bytes;
        let doc = Document::new(Bytes::Owned(br#"{"z":1,"a":[true,null]}"#.to_vec()), true);
        let out = String::from_utf8(super::pretty_json(&doc).unwrap()).unwrap();
        assert_eq!(out, "{\n  \"z\": 1,\n  \"a\": [\n    true,\n    null\n  ]\n}\n");
        let bad = Document::new(Bytes::Owned(b"{nope".to_vec()), true);
        assert!(super::pretty_json(&bad).is_none());
    }

    #[test]
    fn reads_log_levels() {
        assert_eq!(log_level(b"2026-11-10 12:00:01 ERROR db: timeout"), Some(LogLevel::Error));
        assert_eq!(log_level(b"[W] disk 91% full"), Some(LogLevel::Warn));
        assert_eq!(log_level(b"DEBUG cache hit"), Some(LogLevel::Debug));
        assert_eq!(log_level(b"INFO started"), None);
    }

    #[test]
    fn finds_references_under_the_cursor() {
        let line = "error --> src/main.rs:12:5, see https://example.com/x.";
        assert_eq!(link_token(line, 14), Some("src/main.rs:12:5"));
        assert_eq!(link_token(line, 40), Some("https://example.com/x"));
        assert_eq!(split_reference("src/main.rs:12:5"), ("src/main.rs", Some(12)));
        assert_eq!(split_reference("notes.md"), ("notes.md", None));
        assert_eq!(link_token("   ", 1), None);
    }

    #[test]
    fn search_is_smart_case() {
        use crate::text_index::Bytes;
        let doc = Document::new(Bytes::Owned(b"Foo foo FOO".to_vec()), true);
        assert_eq!(search(&doc, "foo").len(), 3);
        assert_eq!(search(&doc, "Foo"), vec![0..3]);
    }
}
