//! Maps an installed Zed-format theme onto the colors the Slint `Theme` global uses.

use super::*;
use forge_extensions::{Appearance, Rgba, Theme as ZedTheme};

/// Syntax kinds 1..=10 as `editor_core::Kind` numbers them; index 0 is plain text.
const SYNTAX_SLOTS: usize = 11;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    pub(crate) dark: bool,
    pub(crate) background: Rgba,
    pub(crate) foreground: Rgba,
    pub(crate) border: Rgba,
    pub(crate) chrome: Rgba,
    pub(crate) muted: Rgba,
    pub(crate) muted_fg: Rgba,
    pub(crate) tab: Rgba,
    pub(crate) primary: Rgba,
    pub(crate) primary_fg: Rgba,
    pub(crate) accent: Rgba,
    pub(crate) hover: Rgba,
    pub(crate) popover: Rgba,
    pub(crate) danger: Rgba,
    pub(crate) warning: Rgba,
    pub(crate) success: Rgba,
    pub(crate) info: Rgba,
    pub(crate) link: Rgba,
    pub(crate) syntax: [Rgba; SYNTAX_SLOTS],
}

fn hex(text: &str) -> Rgba {
    Rgba::parse(text).expect("built-in color")
}

/// The first of `keys` the theme defines, else `fallback` (the built-in color for that slot).
fn pick(theme: &ZedTheme, keys: &[&str], fallback: &str) -> Rgba {
    keys.iter().find_map(|k| theme.style.color(k)).unwrap_or_else(|| hex(fallback))
}

fn pick_syntax(theme: &ZedTheme, keys: &[&str], fallback: Rgba) -> Rgba {
    keys.iter().find_map(|k| theme.style.syntax_color(k)).unwrap_or(fallback)
}

pub(crate) fn palette(theme: &ZedTheme) -> Palette {
    let dark = theme.appearance == Appearance::Dark;
    let d = |dark_hex: &'static str, light_hex: &'static str| if dark { dark_hex } else { light_hex };

    let background = pick(theme, &["editor.background", "background"], d("#0a0a0f", "#fbfbfd"));
    let foreground = pick(theme, &["text", "editor.foreground"], d("#e0e0e0", "#1c1c28"));
    let primary = pick(theme, &["text.accent", "border.focused"], "#7c3aed");
    let primary_fg = if primary.luma() > 0.6 { hex("#111111") } else { hex("#ffffff") };

    // The built-in syntax colors, by kind, used where the theme has no entry.
    let built_in: [&str; SYNTAX_SLOTS] = [
        d("#e0e0e0", "#1c1c28"),
        d("#c084fc", "#7c3aed"),
        d("#a3e635", "#4d7c0f"),
        d("#6b7280", "#9ca3af"),
        d("#fb923c", "#c2410c"),
        d("#4ec9b0", "#0f766e"),
        d("#60a5fa", "#1d4ed8"),
        d("#7dd3fc", "#0369a1"),
        d("#fb923c", "#c2410c"),
        d("#c4b5fd", "#6d28d9"),
        d("#8b8b9e", "#6b7280"),
    ];
    let keys: [&[&str]; SYNTAX_SLOTS] = [
        &[],
        &["keyword"],
        &["string"],
        &["comment"],
        &["number", "constant"],
        &["type"],
        &["function"],
        &["property", "variable.member"],
        &["constant", "boolean"],
        &["operator"],
        &["punctuation", "punctuation.bracket"],
    ];
    let mut syntax = [foreground; SYNTAX_SLOTS];
    for kind in 1..SYNTAX_SLOTS {
        let fallback = if theme.style.syntax_color("keyword").is_some() || theme.style.syntax_color("string").is_some() { foreground } else { hex(built_in[kind]) };
        syntax[kind] = pick_syntax(theme, keys[kind], fallback);
    }

    Palette {
        dark,
        background,
        foreground,
        border: pick(theme, &["border", "border.variant"], d("#2a2a3a", "#dcdce6")),
        chrome: pick(theme, &["title_bar.background", "status_bar.background", "surface.background", "background"], d("#0f0f1a", "#f1f1f7")),
        muted: pick(theme, &["element.background", "surface.background", "panel.background"], d("#161620", "#e8e8f0")),
        muted_fg: pick(theme, &["text.muted", "text.placeholder"], d("#9ca3af", "#6b7280")),
        tab: pick(theme, &["tab.inactive_background", "tab_bar.background", "surface.background"], d("#0d0d18", "#ececf3")),
        primary,
        primary_fg,
        accent: pick(theme, &["element.selected", "ghost_element.selected"], d("#7c3aed33", "#7c3aed22")),
        hover: pick(theme, &["element.hover", "ghost_element.hover"], d("#7c3aed22", "#7c3aed18")),
        popover: pick(theme, &["elevated_surface.background", "surface.background"], d("#161620", "#ffffff")),
        danger: pick(theme, &["error", "terminal.ansi.red"], "#f44747"),
        warning: pick(theme, &["warning", "terminal.ansi.yellow"], "#d4a017"),
        success: pick(theme, &["success", "created", "terminal.ansi.green"], d("#a3e635", "#65a30d")),
        info: pick(theme, &["info", "terminal.ansi.cyan"], "#4ec9b0"),
        link: pick(theme, &["link_text.hover", "text.accent"], d("#c4b5fd", "#6d28d9")),
        syntax,
    }
}

fn color(c: Rgba) -> slint::Color {
    slint::Color::from_argb_u8(c.a, c.r, c.g, c.b)
}

/// Switches the window to `p`.
pub(crate) fn apply(ui: &AppWindow, p: &Palette) {
    let t = ui.global::<Theme>();
    t.set_dark(p.dark);
    t.set_c_background(color(p.background));
    t.set_c_foreground(color(p.foreground));
    t.set_c_border(color(p.border));
    t.set_c_chrome(color(p.chrome));
    t.set_c_muted(color(p.muted));
    t.set_c_muted_fg(color(p.muted_fg));
    t.set_c_tab(color(p.tab));
    t.set_c_primary(color(p.primary));
    t.set_c_primary_fg(color(p.primary_fg));
    t.set_c_accent(color(p.accent));
    t.set_c_hover(color(p.hover));
    t.set_c_popover(color(p.popover));
    t.set_c_danger(color(p.danger));
    t.set_c_warning(color(p.warning));
    t.set_c_success(color(p.success));
    t.set_c_info(color(p.info));
    t.set_c_link(color(p.link));
    t.set_c_syntax(model(p.syntax.iter().map(|&c| color(c)).collect()));
    t.set_custom(true);
}
