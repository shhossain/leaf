//! Quick Open (⌘P) and Go to Heading (⇧⌘J): a small list over the window
//! that filters as you type. Quick Open lists open files, files in watched
//! folders and recently closed files; Go to Heading lists the Markdown
//! headings of the file on screen.

use super::*;

actions!(palette, [QuickOpen, GoToHeading, PaletteUp, PaletteDown, PaletteClose]);

const MAX_ITEMS: usize = 60;

pub fn init(cx: &mut App) {
    let palette = Some("Palette > Input");
    cx.bind_keys([
        KeyBinding::new("cmd-p", QuickOpen, Some("Workspace")),
        KeyBinding::new("cmd-shift-j", GoToHeading, Some("Workspace")),
        KeyBinding::new("up", PaletteUp, palette),
        KeyBinding::new("down", PaletteDown, palette),
        KeyBinding::new("ctrl-p", PaletteUp, palette),
        KeyBinding::new("ctrl-n", PaletteDown, palette),
        KeyBinding::new("escape", PaletteClose, palette),
    ]);
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Mode {
    Files,
    Headings,
}

#[derive(Clone)]
enum Target {
    Path(PathBuf),
    Line(usize),
}

#[derive(Clone)]
struct Item {
    label: SharedString,
    detail: SharedString,
    /// Matched against the query.
    key: String,
    target: Target,
}

pub(super) struct Palette {
    mode: Mode,
    input: Entity<InputState>,
    all: Vec<Item>,
    shown: Vec<Item>,
    selected: usize,
    _events: Subscription,
}

/// Fuzzy match: every query character in order. Higher is better; matches
/// at word starts and in a row score more, and shorter texts win ties.
pub(super) fn fuzzy_score(query: &str, text: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let text_lower: Vec<char> = text.to_lowercase().chars().collect();
    let original: Vec<char> = text.chars().collect();
    let mut score = 0i64;
    let mut pos = 0usize;
    let mut previous: Option<usize> = None;
    for q in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let found = (pos..text_lower.len()).find(|&i| text_lower[i] == q)?;
        score += 1;
        if previous == Some(found.wrapping_sub(1)) {
            score += 5;
        }
        let at_word_start = found == 0
            || matches!(original.get(found - 1), Some('/' | '_' | '-' | '.' | ' '))
            || (original.get(found).is_some_and(|c| c.is_uppercase()) && original.get(found - 1).is_some_and(|c| c.is_lowercase()));
        if at_word_start {
            score += 8;
        }
        previous = Some(found);
        pos = found + 1;
    }
    Some(score * 100 - text_lower.len() as i64)
}

impl Workspace {
    pub(super) fn on_quick_open(&mut self, _: &QuickOpen, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette(Mode::Files, window, cx);
    }

    pub(super) fn on_go_to_heading(&mut self, _: &GoToHeading, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette(Mode::Headings, window, cx);
    }

    fn open_palette(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        let all = match mode {
            Mode::Files => self.file_items(),
            Mode::Headings => self.heading_items(cx),
        };
        let placeholder = match mode {
            Mode::Files => "Open a file…",
            Mode::Headings if all.is_empty() => "No headings in this file",
            Mode::Headings => "Go to a heading…",
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let events = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => this.filter_palette(cx),
            InputEvent::PressEnter { .. } => this.accept_palette(None, window, cx),
            _ => {}
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.palette = Some(Palette { mode, input, shown: Vec::new(), all, selected: 0, _events: events });
        self.filter_palette(cx);
    }

    /// Open files first, then watched-folder files, then recently closed ones.
    fn file_items(&self) -> Vec<Item> {
        let mut seen = std::collections::HashSet::new();
        let mut items = Vec::new();
        let mut push = |path: &Path, detail: String, items: &mut Vec<Item>| {
            if !seen.insert(path.to_path_buf()) {
                return;
            }
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let key = format!("{name} {}", path.display());
            items.push(Item { label: name.into(), detail: detail.into(), key, target: Target::Path(path.to_path_buf()) });
        };
        let home = dirs::home_dir();
        let short = |path: &Path| {
            let parent = path.parent().unwrap_or(path);
            match home.as_ref().and_then(|h| parent.strip_prefix(h).ok()) {
                Some(rel) => format!("~/{}", rel.display()),
                None => parent.display().to_string(),
            }
        };
        for tab in self.tabs.iter().filter(|t| !t.in_folder) {
            push(&tab.path, format!("Open  ·  {}", short(&tab.path)), &mut items);
        }
        for folder in &self.folders {
            for entry in folder.entries.iter().filter(|e| !e.meta.as_ref().is_some_and(|m| m.binary)) {
                push(&entry.path, format!("{}  ·  {}", folder.name, entry.subfolder), &mut items);
            }
        }
        for (path, _) in self.closed.iter().rev() {
            if path.is_file() {
                push(path, format!("Recent  ·  {}", short(path)), &mut items);
            }
        }
        items
    }

    /// Markdown headings of the file on screen (outside code blocks).
    fn heading_items(&self, cx: &App) -> Vec<Item> {
        let Some(doc) = self.active_tab().and_then(|t| t.doc(cx)) else { return Vec::new() };
        if doc.len() > 8 * 1024 * 1024 {
            return Vec::new();
        }
        let text = doc.text(0..doc.len());
        let mut in_fence = false;
        let mut items = Vec::new();
        for (line, raw) in text.lines().enumerate() {
            let trimmed = raw.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence || !raw.starts_with('#') {
                continue;
            }
            let level = raw.chars().take_while(|&c| c == '#').count();
            let title = raw[level..].trim();
            if level > 6 || title.is_empty() || !raw[level..].starts_with(' ') {
                continue;
            }
            let indent = "    ".repeat(level.saturating_sub(1));
            items.push(Item {
                label: format!("{indent}{title}").into(),
                detail: format!("Line {}", line + 1).into(),
                key: title.to_string(),
                target: Target::Line(line),
            });
        }
        items
    }

    fn filter_palette(&mut self, cx: &mut Context<Self>) {
        let Some(palette) = self.palette.as_mut() else { return };
        let query = palette.input.read(cx).value().to_string();
        let mut scored: Vec<(i64, usize)> = palette
            .all
            .iter()
            .enumerate()
            .filter_map(|(ix, item)| {
                // Names count more than folders.
                let name_score = fuzzy_score(&query, &item.label).map(|s| s + 10_000);
                name_score.or_else(|| fuzzy_score(&query, &item.key)).map(|s| (s, ix))
            })
            .collect();
        if !query.is_empty() {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        palette.shown = scored.into_iter().take(MAX_ITEMS).map(|(_, ix)| palette.all[ix].clone()).collect();
        palette.selected = 0;
        cx.notify();
    }

    pub(super) fn on_palette_up(&mut self, _: &PaletteUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.palette.as_mut() {
            p.selected = p.selected.saturating_sub(1);
            cx.notify();
        }
    }

    pub(super) fn on_palette_down(&mut self, _: &PaletteDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.palette.as_mut() {
            p.selected = (p.selected + 1).min(p.shown.len().saturating_sub(1));
            cx.notify();
        }
    }

    pub(super) fn on_palette_close(&mut self, _: &PaletteClose, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        match self.active_index() {
            Some(ix) => self.focus_tab(ix, window, cx),
            None => window.focus(&self.focus, cx),
        }
        cx.notify();
    }

    fn accept_palette(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(palette) = self.palette.take() else { return };
        let Some(item) = palette.shown.get(index.unwrap_or(palette.selected)).cloned() else {
            self.close_palette(window, cx);
            return;
        };
        match item.target {
            Target::Path(path) => {
                if let Some(id) = self.open_paths(vec![path], cx) {
                    self.activate(id, window, cx);
                }
            }
            Target::Line(line) => self.go_to_line_in_active(line, cx),
        }
        self.close_palette(window, cx);
    }

    pub(super) fn render_palette(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let palette = self.palette.as_ref()?;
        let theme = cx.theme();
        let (muted, popover, border) = (theme.muted_foreground, theme.popover, theme.border);
        let (selected_bg, selected_fg) = style::selection_row(theme.mode.is_dark());
        let empty = palette.shown.is_empty();
        let rows = palette.shown.iter().enumerate().map(|(ix, item)| {
            let selected = ix == palette.selected;
            h_flex()
                .id(("palette-item", ix))
                .gap_3()
                .px_3()
                .py_1p5()
                .rounded(px(8.))
                .text_sm()
                .when(selected, |el| el.bg(selected_bg).text_color(selected_fg))
                .on_click(cx.listener(move |this, _, window, cx| this.accept_palette(Some(ix), window, cx)))
                .child(div().flex_none().max_w(px(300.)).truncate().whitespace_nowrap().child(item.label.clone()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(if selected { selected_fg.opacity(0.75) } else { muted })
                        .child(item.detail.clone()),
                )
        });
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(72.))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_palette(window, cx)))
                .child(
                    v_flex()
                        .id("palette")
                        .key_context("Palette")
                        .w(px(560.))
                        .max_h(px(440.))
                        .p_2()
                        .gap_1()
                        .rounded(px(12.))
                        .bg(popover)
                        .border_1()
                        .border_color(border)
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_action(cx.listener(Self::on_palette_up))
                        .on_action(cx.listener(Self::on_palette_down))
                        .on_action(cx.listener(Self::on_palette_close))
                        .child(div().px_2().pb_1().child(Input::new(&palette.input).appearance(false)))
                        .child(div().h(px(1.)).bg(border))
                        .when(empty, |el| {
                            let text = if palette.mode == Mode::Headings { "No headings" } else { "No matching files" };
                            el.child(div().px_3().py_2().text_sm().text_color(muted).child(text))
                        })
                        .child(v_flex().id("palette-items").overflow_y_scroll().children(rows)),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::fuzzy_score;

    #[test]
    fn fuzzy_prefers_word_starts_and_runs() {
        assert!(fuzzy_score("wsp", "workspace.rs").is_some());
        assert!(fuzzy_score("xyz", "workspace.rs").is_none());
        assert!(fuzzy_score("ws", "workspace.rs") < fuzzy_score("ws", "ws.rs"));
        assert!(fuzzy_score("main", "main.rs") > fuzzy_score("main", "domain_names.rs"));
    }
}
