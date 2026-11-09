//! Apple Notes–like colors, applied over gpui-component's theme after every
//! light/dark switch. The stock dark theme is near-black; macOS uses soft
//! greys, quiet separators and a warm yellow accent.

use gpui_kit::component::theme::Theme;
use gpui_kit::*;

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
}

trait ThemeMode {
    fn theme_is_dark(&self) -> bool;
}

impl ThemeMode for App {
    fn theme_is_dark(&self) -> bool {
        Theme::global(self).mode.is_dark()
    }
}
