//! Apple Notes–like colors, applied over gpui-component's theme after every
//! light/dark switch. The stock dark theme is near-black; macOS uses soft
//! greys, quiet separators and a warm yellow accent.

use std::sync::Arc;

use gpui_kit::component::highlighter::HighlightTheme;
use gpui_kit::component::theme::Theme;
use gpui_kit::*;
use serde_json::json;

/// Notes' highlight for the selected row.
pub fn selection_row(dark: bool) -> (Hsla, Hsla) {
    if dark { (rgb(0x87703a).into(), rgb(0xf5f5f5).into()) } else { (rgb(0xf7d97a).into(), rgb(0x262628).into()) }
}

pub fn apply(window: Option<&mut Window>, cx: &mut App) {
    Theme::sync_system_appearance(window, cx);
    let dark = cx.theme_is_dark();
    let c = &mut Theme::global_mut(cx).colors;
    let hex = |v: u32| -> Hsla { rgb(v).into() };
    let alpha = |v: u32, a: f32| -> Hsla { hex(v).opacity(a) };
    if dark {
        c.background = hex(0x1e1e1e);
        c.foreground = hex(0xdedede);
        c.muted = hex(0x2a2a2a);
        c.muted_foreground = hex(0x8e8e93);
        c.border = hex(0x323232);
        c.sidebar = hex(0x262626);
        c.sidebar_foreground = hex(0xdedede);
        c.sidebar_border = hex(0x161616);
        c.secondary = hex(0x2f2f2f);
        c.secondary_hover = hex(0x383838);
        c.secondary_active = hex(0x404040);
        c.input = hex(0x3a3a3a);
        c.popover = hex(0x2a2a2a);
        c.list_hover = hex(0x2f2f2f);
        c.selection = alpha(0x3f6fd6, 0.45);
        c.caret = hex(0xe5b53c);
        c.accent = hex(0x343434);
        c.link = hex(0xe5b53c);
        c.scrollbar_thumb = alpha(0xffffff, 0.22);
        c.scrollbar_thumb_hover = alpha(0xffffff, 0.35);
    } else {
        c.background = hex(0xffffff);
        c.foreground = hex(0x262628);
        c.muted = hex(0xf2f2f2);
        c.muted_foreground = hex(0x86868b);
        c.border = hex(0xe3e3e5);
        c.sidebar = hex(0xf0f0f0);
        c.sidebar_foreground = hex(0x1d1d1f);
        c.sidebar_border = hex(0xd9d9db);
        c.secondary = hex(0xebebed);
        c.secondary_hover = hex(0xe2e2e4);
        c.secondary_active = hex(0xd8d8da);
        c.input = hex(0xdcdcdf);
        c.popover = hex(0xffffff);
        c.list_hover = hex(0xebebed);
        c.selection = alpha(0x4f8cf7, 0.30);
        c.caret = hex(0xc99a1b);
        c.accent = hex(0xebebed);
        c.link = hex(0xb07d0a);
        c.scrollbar_thumb = alpha(0x000000, 0.20);
        c.scrollbar_thumb_hover = alpha(0x000000, 0.32);
    }
    let syntax = syntax_theme(dark);
    match serde_json::from_value::<HighlightTheme>(syntax) {
        Ok(theme) => Theme::global_mut(cx).highlight_theme = Arc::new(theme),
        Err(err) => eprintln!("[leaf] syntax palette rejected: {err}"),
    }
}

/// Muted, low-saturation syntax colors that sit with the soft greys instead
/// of the stock high-contrast palette. Same names as gpui-component themes.
fn syntax_theme(dark: bool) -> serde_json::Value {
    let p = if dark {
        Palette {
            background: "#1e1e1e", foreground: "#dedede", line_number: "#5c5c60", active_line: "#252525",
            keyword: "#d4a0bf", string: "#a7c49a", number: "#d4b48c", comment: "#77777d",
            function: "#97b6d4", kind: "#93bfb5", tag: "#97b6d4", escape: "#c9a67e",
            punctuation: "#a3a3a8", title: "#ececec", link: "#97b6d4", muted: "#8e8e93",
        }
    } else {
        Palette {
            background: "#ffffff", foreground: "#262628", line_number: "#b4b4b8", active_line: "#f6f6f6",
            keyword: "#984074", string: "#40723a", number: "#8a5d1f", comment: "#8e8e93",
            function: "#2f5e8e", kind: "#2a6a60", tag: "#2f5e8e", escape: "#8a5d1f",
            punctuation: "#5f5f64", title: "#1d1d1f", link: "#2f5e8e", muted: "#86868b",
        }
    };
    let c = |color: &str| json!({ "color": color });
    json!({
        "name": "Leaf",
        "appearance": if dark { "dark" } else { "light" },
        "style": {
            "editor.background": p.background,
            "editor.gutter.background": p.background,
            "editor.foreground": p.foreground,
            "editor.active_line.background": p.active_line,
            "editor.line_number": p.line_number,
            "editor.active_line_number": p.punctuation,
            "syntax": {
                "keyword": c(p.keyword),
                "preproc": c(p.keyword),
                "variable.special": c(p.keyword),
                "string": c(p.string),
                "string.escape": c(p.escape),
                "string.regex": c(p.escape),
                "string.special": c(p.escape),
                "string.special.symbol": c(p.escape),
                "number": c(p.number),
                "boolean": c(p.number),
                "constant": c(p.number),
                "label": c(p.number),
                "attribute": c(p.number),
                "comment": { "color": p.comment, "font_style": "italic" },
                "comment.doc": { "color": p.comment, "font_style": "italic" },
                "function": c(p.function),
                "constructor": c(p.kind),
                "type": c(p.kind),
                "tag": c(p.tag),
                "property": c(p.foreground),
                "variable": c(p.foreground),
                "operator": c(p.punctuation),
                "punctuation": c(p.punctuation),
                "punctuation.bracket": c(p.punctuation),
                "punctuation.delimiter": c(p.punctuation),
                "punctuation.list_marker": c(p.muted),
                "title": { "color": p.title, "font_weight": 600 },
                "emphasis": { "font_style": "italic" },
                "emphasis.strong": { "font_weight": 700 },
                "link_text": c(p.link),
                "link_uri": c(p.muted),
                "text.literal": c(p.escape),
                "text.code.span": c(p.escape),
            }
        }
    })
}

struct Palette {
    background: &'static str,
    foreground: &'static str,
    line_number: &'static str,
    active_line: &'static str,
    keyword: &'static str,
    string: &'static str,
    number: &'static str,
    comment: &'static str,
    function: &'static str,
    kind: &'static str,
    tag: &'static str,
    escape: &'static str,
    punctuation: &'static str,
    title: &'static str,
    link: &'static str,
    muted: &'static str,
}

trait ThemeMode {
    fn theme_is_dark(&self) -> bool;
}

impl ThemeMode for App {
    fn theme_is_dark(&self) -> bool {
        Theme::global(self).mode.is_dark()
    }
}
