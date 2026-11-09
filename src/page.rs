//! The rendered page (Markdown, prose, wrapped text): keeping its state,
//! jumping to a line, double-click to edit at the clicked paragraph,
//! clickable checklist boxes, and word counts.

use super::*;
use gpui_kit::component::text::TextViewState;

/// Link target of a checklist box in the rendered page: `leaf-task:<line>`.
const TASK_SCHEME: &str = "leaf-task:";

/// `- [ ] text` / `* [x] text` (any indent) → (byte offset of the check char, checked).
pub(super) fn task_marker(line: &str) -> Option<(usize, bool)> {
    let body = line.trim_start();
    let indent = line.len() - body.len();
    let rest = body.strip_prefix("- [").or_else(|| body.strip_prefix("* [")).or_else(|| body.strip_prefix("+ ["))?;
    let mut chars = rest.chars();
    let check = chars.next()?;
    if !matches!(check, ' ' | 'x' | 'X') || !chars.as_str().starts_with("] ") {
        return None;
    }
    Some((indent + 3, check != ' '))
}

/// Turns checklist boxes into links Leaf handles (`[☐](leaf-task:12)`), so a
/// click ticks the item in the file. Line count is unchanged.
pub(super) fn taskify(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len() + 64);
    let mut in_fence = false;
    for (ix, line) in markdown.split('\n').enumerate() {
        if ix > 0 {
            out.push('\n');
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        match task_marker(line).filter(|_| !in_fence) {
            Some((at, checked)) => {
                let glyph = if checked { "☑" } else { "☐" };
                out.push_str(&line[..at - 1]); // "- "
                out.push_str(&format!("[{glyph}]({TASK_SCHEME}{ix}) "));
                out.push_str(&line[at + 3..]); // after "x] "
            }
            None => out.push_str(line),
        }
    }
    out
}

/// Words in `text`, for the toolbar.
pub(super) fn count_words(text: &str) -> usize {
    text.split_whitespace().filter(|w| w.chars().any(char::is_alphanumeric)).count()
}

impl Viewer {
    /// The page's view state for `text`, created once and updated in place.
    pub(super) fn page_state(&mut self, text: &SharedString, cx: &mut Context<Self>) -> Entity<TextViewState> {
        match &self.page_state {
            Some((shown, state)) if shown == text => state.clone(),
            Some((_, state)) => {
                let state = state.clone();
                state.update(cx, |state, cx| state.set_text(text, cx));
                self.page_state = Some((text.clone(), state.clone()));
                state
            }
            None => {
                let state = cx.new(|cx| {
                    TextViewState::markdown(text, cx).selectable(true).selection_format(SelectionFormat::Plain).scrollable(true)
                });
                self.page_state = Some((text.clone(), state.clone()));
                state
            }
        }
    }

    /// Lines in the page and in the file match one to one, except that a
    /// note gets a blank line after the title it is given.
    fn note_title_shift(&self) -> Option<usize> {
        if !self.note || self.rich != Some(RichKind::Markdown) {
            return None;
        }
        let text = self.doc.text(0..self.doc.len().min(64 * 1024));
        let (title, line) = text.lines().enumerate().find(|(_, l)| !l.trim().is_empty())?;
        (!line.trim_start().starts_with('#')).then_some(title)
    }

    fn page_line_to_source(&self, page_line: usize) -> usize {
        match self.note_title_shift() {
            Some(title) if page_line > title + 1 => page_line - 1,
            Some(title) if page_line > title => title,
            _ => page_line,
        }
    }

    fn source_line_to_page(&self, line: usize) -> usize {
        match self.note_title_shift() {
            Some(title) if line > title => line + 1,
            _ => line,
        }
    }

    /// Scrolls the page to the text of source `line`.
    pub(super) fn reveal_in_page(&mut self, line: usize, cx: &mut Context<Self>) -> bool {
        let Some((text, state)) = self.page_state.clone() else { return false };
        let page_line = self.source_line_to_page(line);
        let offset = text.split('\n').take(page_line).map(|l| l.len() + 1).sum::<usize>().min(text.len());
        state.update(cx, |state, cx| state.reveal_range(offset..offset, cx)).is_ok()
    }

    /// After a double-click on the page: edit at the paragraph that was clicked.
    pub(super) fn edit_at_page_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((text, state)) = self.page_state.clone() else {
            self.request_edit(None, window, cx);
            return;
        };
        let at = state.read(cx).selected_source_range().map(|range| {
            let offset = range.start.min(text.len());
            let page_line = text[..offset].matches('\n').count();
            let line = self.page_line_to_source(page_line);
            // Same text in page and file (plain Markdown): keep the column too.
            let col = if self.rich == Some(RichKind::Markdown) && !self.note && !self.wrap {
                offset - text[..offset].rfind('\n').map_or(0, |p| p + 1)
            } else {
                0
            };
            Pos { line, col }
        });
        self.request_edit(at, window, cx);
    }

    /// A checklist box was clicked: tick or untick that line in the file.
    pub(super) fn toggle_task(&mut self, page_line: usize, cx: &mut Context<Self>) {
        if self.doc.lossy || self.doc.as_slice().is_none() || self.raw.is_some() {
            return;
        }
        let line = self.page_line_to_source(page_line);
        let range = self.doc.line_range(line);
        let text = self.doc.text(range.clone()).into_owned();
        let Some((at, checked)) = task_marker(&text) else { return };
        let mut all = self.doc.text(0..self.doc.len()).into_owned();
        all.replace_range(range.start + at..range.start + at + 1, if checked { " " } else { "x" });
        self.doc = Arc::new(Document::new(Bytes::Owned(all.into_bytes()), false));
        self.preview = self.build_preview();
        cx.emit(BoardEdited); // the workspace saves the file
        cx.notify();
    }

    /// `leaf-task:12` links from [`taskify`].
    pub(super) fn task_link(target: &str) -> Option<usize> {
        target.strip_prefix(TASK_SCHEME)?.parse().ok()
    }

    /// "1,204 words · 5 min", or the selection's count, for Markdown and text.
    pub(super) fn word_status(&self, cx: &App) -> Option<String> {
        if !matches!(self.rich, Some(RichKind::Markdown | RichKind::Prose)) || self.doc.len() > 4 * 1024 * 1024 {
            return None;
        }
        if let Some((_, state)) = &self.page_state {
            let selected = state.read(cx).selected_text();
            if !selected.trim().is_empty() {
                return Some(format!("{} words selected", count_words(&selected)));
            }
        }
        let key = Arc::as_ptr(&self.doc) as usize;
        let words = match self.word_cache.get() {
            Some((cached, words)) if cached == key => words,
            _ => {
                let words = count_words(&self.doc.text(0..self.doc.len()));
                self.word_cache.set(Some((key, words)));
                words
            }
        };
        let minutes = words.div_ceil(230).max(1);
        Some(format!("{words} words · {minutes} min read"))
    }
}

#[cfg(test)]
mod tests {
    use super::{count_words, task_marker, taskify};

    #[test]
    fn checklist_boxes_become_links() {
        assert_eq!(task_marker("  - [x] done"), Some((5, true)));
        assert_eq!(task_marker("- [ ] todo"), Some((3, false)));
        assert_eq!(task_marker("- [link](x)"), None);
        let md = "# T\n- [ ] a\n```\n- [ ] code\n```\n  * [x] b";
        let out = taskify(md);
        assert_eq!(out.lines().count(), md.lines().count());
        assert!(out.contains("- [☐](leaf-task:1) a"));
        assert!(out.contains("- [ ] code"));
        assert!(out.contains("  * [☑](leaf-task:5) b"));
        assert_eq!(count_words("Hello, world — 42 times."), 4);
    }
}
