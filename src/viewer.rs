//! The read-only viewer: draws only the rows on screen, from a shared
//! [`Document`]. Memory is the file plus a small line index; the full editor
//! is created only when the user switches a tab to edit mode.

use std::collections::VecDeque;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, h_flex,
    input::{Input, InputEvent, InputState},
    scroll::Scrollbar,
    text::{SelectionFormat, TextView},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use syntect::parsing::SyntaxReference;

use crate::highlight::{self, CHUNK, LineState, Spans};
use crate::text_index::{DisplayLine, Document};

actions!(
    viewer,
    [
        LineUp, LineDown, PageUp, PageDown, Top, Bottom, CopySelection, SelectAllText, Find,
        FindNext, FindPrev, GoToLine, Dismiss, TogglePreview
    ]
);

/// Highlighted chunks kept around for scrolling back and forth.
const CHUNK_CACHE: usize = 8;
const MAX_MATCHES: usize = 100_000;
/// Lines parsed before a chunk when highlighting it without exact state.
const WARMUP_LINES: usize = 64;
/// Frames to wait for the line list's first layout before dropping a scroll.
const MAX_SCROLL_RETRIES: u8 = 5;
/// The exact highlighting pass only walks this many chunks ahead of its last
/// checkpoint. A far jump (⌘↓ in a big file) keeps the approximate colors
/// instead of parsing everything in between (~23 s of CPU for 7 MB).
const MAX_EXACT_GAP_CHUNKS: usize = 50;
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
}

/// Markdown and plain text: shown and edited as pages, not numbered code.
pub fn is_page_file(path: &Path) -> bool {
    rich_kind(path).is_some()
}

fn rich_kind(path: &Path) -> Option<RichKind> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase());
    match ext.as_deref() {
        Some("md" | "markdown" | "mdown" | "mkd" | "mdx") => Some(RichKind::Markdown),
        Some("txt" | "text") => Some(RichKind::Prose),
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
        KeyBinding::new("cmd-shift-v", TogglePreview, root),
        KeyBinding::new("escape", Dismiss, Some("ViewerBar")),
    ]);
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

struct GotoBar {
    input: Entity<InputState>,
    _events: Subscription,
}

pub struct Viewer {
    doc: Arc<Document>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    /// Parser state at the start of chunk `k` (line `k * CHUNK`).
    syntax: Option<&'static SyntaxReference>,
    checkpoints: Vec<LineState>,
    /// The background pass stops once it has a checkpoint for this chunk.
    wanted_chunk: usize,
    pass_running: bool,
    /// `(chunk, exact, spans)`; inexact entries are replaced once the pass catches up.
    chunk_cache: VecDeque<(usize, bool, Rc<Vec<Spans>>)>,
    _checkpoint_task: Option<Task<()>>,
    anchor: Option<Pos>,
    head: Pos,
    dragging: bool,
    find: Option<FindBar>,
    goto: Option<GotoBar>,
    flash_line: Option<usize>,
    /// `(line, center)` to scroll to on the next render.
    pending_scroll: Option<(usize, bool)>,
    pending_scroll_retries: u8,
    // Set in `render`, used by hit-testing.
    row_height: Pixels,
    gutter_width: Pixels,
    font: Font,
    font_size: Pixels,
    rich: Option<RichKind>,
    /// Markdown being rendered while the page view (not numbered lines) shows.
    preview: Option<SharedString>,
    /// Folder of the file; relative image paths in Markdown resolve against it.
    base_dir: PathBuf,
    /// Where a double-click asked to start editing; read by the workspace.
    edit_at: Option<Pos>,
    /// In the Leaf Notes folder: rendered the way Notes shows notes.
    note: bool,
}

impl Viewer {
    pub fn new(doc: Arc<Document>, path: &Path, cx: &mut Context<Self>) -> Self {
        let syntax = (doc.len() <= highlight::MAX_HIGHLIGHT_BYTES)
            .then(|| highlight::find_syntax(path, &doc.text(doc.line_range(0))))
            .flatten();
        let mut this = Self {
            doc,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            syntax,
            checkpoints: Vec::new(),
            wanted_chunk: 0,
            pass_running: false,
            chunk_cache: VecDeque::new(),
            _checkpoint_task: None,
            anchor: None,
            head: Pos::default(),
            dragging: false,
            find: None,
            goto: None,
            flash_line: None,
            pending_scroll: None,
            pending_scroll_retries: 0,
            row_height: px(20.),
            gutter_width: px(60.),
            font: font("Menlo"),
            font_size: px(13.),
            rich: rich_kind(path),
            preview: None,
            base_dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
            edit_at: None,
            note: crate::notes::is_note(path),
        };
        // Viewer-first: Markdown opens rendered and text reads like Notes;
        // ⌘⇧V switches to numbered source lines.
        this.preview = this.build_preview();
        this
    }

    fn build_preview(&self) -> Option<SharedString> {
        let text = || self.doc.text(0..self.doc.len());
        match self.rich? {
            RichKind::Markdown if self.doc.len() <= MAX_PREVIEW_BYTES => Some(if self.note {
                note_markdown(&text()).into()
            } else {
                text().into_owned().into()
            }),
            RichKind::Prose if self.doc.len() <= MAX_PROSE_BYTES => Some(plain_to_markdown(&text()).into()),
            _ => None,
        }
    }

    /// Showing a rendered page (Markdown or prose) rather than numbered lines.
    pub fn page_kind(&self) -> Option<RichKind> {
        self.preview.as_ref().and(self.rich)
    }

    pub fn can_toggle_page(&self) -> bool {
        self.rich.is_some()
    }

    fn on_toggle_preview(&mut self, _: &TogglePreview, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_page(cx);
    }

    /// Switches between the rendered page and numbered source lines.
    pub fn toggle_page(&mut self, cx: &mut Context<Self>) {
        if self.preview.take().is_none() {
            self.preview = self.build_preview();
        }
        cx.notify();
    }

    fn render_preview(&self, text: SharedString, cx: &mut Context<Self>) -> impl IntoElement {
        let base = self.base_dir.clone();
        div()
            .id("preview")
            // Double-click anywhere on the page to start editing, like Notes.
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                if event.click_count == 2 {
                    this.request_edit(None, window, cx);
                }
            }))
            .key_context("Viewer")
            .track_focus(&self.focus)
            .flex_1()
            .min_h_0()
            .child(
                TextView::markdown(("markdown", cx.entity_id().as_u64() as usize), text)
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

    pub fn doc(&self) -> &Arc<Document> {
        &self.doc
    }

    pub fn top_line(&self) -> usize {
        let offset = self.scroll.0.borrow().base_handle.offset();
        ((-offset.y / self.row_height).max(0.) as usize).min(self.doc.line_count() - 1)
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
        let content = self.row_height * self.doc.line_count() as f32;
        let max = (content - viewport).max(px(0.));
        let mut y = self.row_height * line.min(self.doc.line_count() - 1) as f32;
        if center {
            y -= (viewport - self.row_height) / 2.;
        }
        handle.set_offset(point(handle.offset().x, -y.clamp(px(0.), max)));
        self.pending_scroll = None;
    }

    /// One-line summary for the status bar.
    pub fn status(&self) -> String {
        let mut parts = Vec::new();
        if let Some((start, end)) = self.selection() {
            let chars = end.line - start.line + 1;
            parts.push(if start.line == end.line {
                format!("Ln {}, Col {}", end.line + 1, end.col + 1)
            } else {
                format!("{chars} lines selected")
            });
        }
        if self.pass_running {
            parts.push("highlighting…".into());
        }
        parts.join("  ·  ")
    }

    // ── syntax highlighting ────────────────────────────────────────────────

    /// Asks the background pass for exact parser state up to `chunk`. The pass
    /// only ever parses as far as the reader has scrolled, then idles.
    fn want_chunk(&mut self, chunk: usize, cx: &mut Context<Self>) {
        let Some(syntax) = self.syntax else { return };
        if chunk > self.checkpoints.len() + MAX_EXACT_GAP_CHUNKS {
            return;
        }
        let total = self.doc.line_count().div_ceil(CHUNK);
        self.wanted_chunk = self.wanted_chunk.max(chunk.min(total - 1));
        if self.pass_running || self.checkpoints.len() > self.wanted_chunk {
            return;
        }
        if self.checkpoints.is_empty() {
            self.checkpoints.push(LineState::new(syntax));
        }
        self.pass_running = true;
        let doc = self.doc.clone();
        self._checkpoint_task = Some(cx.spawn(async move |this, cx| {
            loop {
                // Resume from the last checkpoint; at most 16 chunks (2048 lines) per batch.
                let Ok((mut state, from, to)) = this.update(cx, |viewer, _| {
                    let from = viewer.checkpoints.len() - 1;
                    let to = (from + 16).min(viewer.wanted_chunk);
                    (viewer.checkpoints[from].clone(), from, to)
                }) else {
                    return;
                };
                if to <= from {
                    this.update(cx, |viewer, cx| {
                        viewer.pass_running = false;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                let doc = doc.clone();
                let batch = cx
                    .background_executor()
                    .spawn(async move {
                        let mut out = Vec::with_capacity(to - from);
                        for c in from..to {
                            state = highlight::advance(&doc, state, c * CHUNK..(c + 1) * CHUNK);
                            out.push(state.clone());
                        }
                        out
                    })
                    .await;
                let alive = this.update(cx, |viewer, cx| {
                    viewer.checkpoints.extend(batch);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
        }));
    }

    fn chunk_spans(&mut self, chunk: usize, cx: &mut Context<Self>) -> Option<Rc<Vec<Spans>>> {
        let syntax = self.syntax?;
        let exact = chunk < self.checkpoints.len();
        if let Some((_, cached_exact, spans)) = self.chunk_cache.iter().find(|(c, ..)| *c == chunk) {
            if *cached_exact || !exact {
                return Some(spans.clone());
            }
        }
        self.want_chunk(chunk + 2, cx);
        let start = chunk * CHUNK;
        let spans = if exact {
            highlight::highlight(&self.doc, self.checkpoints[chunk].clone(), start..start + CHUNK)
        } else {
            // Best guess until the exact pass arrives: start fresh a little earlier.
            let warm = start.saturating_sub(WARMUP_LINES);
            let mut spans = highlight::highlight(&self.doc, LineState::new(syntax), warm..start + CHUNK);
            spans.drain(..(start - warm).min(spans.len()));
            spans
        };
        let spans = Rc::new(spans);
        self.chunk_cache.retain(|(c, ..)| *c != chunk);
        self.chunk_cache.push_front((chunk, exact, spans.clone()));
        self.chunk_cache.truncate(CHUNK_CACHE);
        Some(spans)
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
        let line_ranges = self.doc.line_ranges(range.clone());
        let mut rows = Vec::with_capacity(line_ranges.len());

        for (i, source) in line_ranges.into_iter().enumerate() {
            let line = range.start + i;
            let display = DisplayLine::new(&self.doc.bytes()[source.clone()]);
            let to_display = |r: Range<usize>| display.source_to_display(r.start)..display.source_to_display(r.end);

            let mut colors = Vec::new();
            if let Some(spans) = self.chunk_spans(line / CHUNK, cx) {
                for (span, name) in spans.get(line % CHUNK).into_iter().flatten() {
                    if let Some(style) = syntax_theme.style(name) {
                        colors.push((to_display(span.clone()), style));
                    }
                }
            }

            let mut backgrounds = Vec::new();
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

    fn hit_test(&self, point: Point<Pixels>, window: &mut Window) -> Pos {
        let (bounds, offset) = {
            let state = self.scroll.0.borrow();
            (state.base_handle.bounds(), state.base_handle.offset())
        };
        let y = point.y - bounds.top() - offset.y;
        let last = self.doc.line_count() - 1;
        let line = ((y / self.row_height).floor().max(0.) as usize).min(last);
        let x = point.x - bounds.left() - offset.x - self.gutter_width;
        let raw = &self.doc.bytes()[self.doc.line_range(line)];
        let display = DisplayLine::new(raw);
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
        if let Some((start, end)) = self.selection() {
            let text = self.doc.text(self.offset_of(start)..self.offset_of(end)).into_owned();
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn on_select_all(&mut self, _: &SelectAllText, _: &mut Window, cx: &mut Context<Self>) {
        self.anchor = Some(Pos::default());
        self.head = self.pos_of_offset(self.doc.len());
        cx.notify();
    }

    // ── find ───────────────────────────────────────────────────────────────

    fn on_find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        self.preview = None; // find works on lines: show the source
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

    // ── go to line ─────────────────────────────────────────────────────────

    fn on_goto(&mut self, _: &GoToLine, window: &mut Window, cx: &mut Context<Self>) {
        self.preview = None;
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
        if self.goto.take().is_some() || self.find.take().is_some() {
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

impl Focusable for Viewer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Viewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        self.font = font(theme.mono_font_family.clone());
        self.font_size = theme.mono_font_size;
        self.row_height = (theme.mono_font_size * 1.5).round();
        let digits = self.doc.line_count().to_string().len().max(3) as f32;
        let char_width = window
            .text_system()
            .advance(window.text_system().resolve_font(&self.font), self.font_size, 'm')
            .map(|size| size.width)
            .unwrap_or(self.font_size * 0.6);
        self.gutter_width = char_width * (digits + 3.);
        self.apply_pending_scroll(window, cx);
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);

        let list = uniform_list(
            "rows",
            self.doc.line_count(),
            cx.processor(|this, range: Range<usize>, window, cx| this.render_rows(range, window, cx)),
        )
        .track_scroll(&self.scroll)
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .with_width_from_item(Some(self.doc.longest_line()))
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
            .when_some(self.goto.as_ref(), |this, goto| {
                this.child(self.render_bar(Input::new(&goto.input).small(), cx))
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
            .when_some(self.preview.clone(), |this, text| this.child(self.render_preview(text, cx)))
            .when(self.preview.is_none(), |this| this.child(
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
                    .child(list)
                    .child(Scrollbar::new(&self.scroll)),
            ))
    }
}

/// Searches the whole document; smart case (any uppercase → case-sensitive).
fn search(doc: &Document, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let case_sensitive = query.chars().any(char::is_uppercase);
    let Ok(regex) = regex::bytes::RegexBuilder::new(&regex::escape(query))
        .case_insensitive(!case_sensitive)
        .build()
    else {
        return Vec::new();
    };
    regex.find_iter(doc.bytes()).take(MAX_MATCHES).map(|m| m.range()).collect()
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
    use super::{compose, search};
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
    fn search_is_smart_case() {
        use crate::text_index::Bytes;
        let doc = Document::new(Bytes::Owned(b"Foo foo FOO".to_vec()), true);
        assert_eq!(search(&doc, "foo").len(), 3);
        assert_eq!(search(&doc, "Foo"), vec![0..3]);
    }
}
