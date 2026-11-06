//! Theme files of installed extensions (`themes/*.json`).
//!
//! The file format is Zed's theme family format: one file holds a family with
//! several themes, each with a flat `style` map of dotted color keys and a
//! `syntax` map. Zed reads these files leniently (comments, trailing commas),
//! so this does too.

use crate::{Installer, Manifest};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;

/// A theme file larger than this is not read.
const MAX_THEME_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    /// `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`.
    pub fn parse(text: &str) -> Option<Rgba> {
        let hex = text.trim().strip_prefix('#')?;
        if !hex.is_ascii() {
            return None;
        }
        let digit = |i: usize| u8::from_str_radix(hex.get(i..i + 1)?, 16).ok().map(|d| d * 17);
        let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
        match hex.len() {
            3 => Some(Rgba { r: digit(0)?, g: digit(1)?, b: digit(2)?, a: 255 }),
            4 => Some(Rgba { r: digit(0)?, g: digit(1)?, b: digit(2)?, a: digit(3)? }),
            6 => Some(Rgba { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: 255 }),
            8 => Some(Rgba { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: byte(6)? }),
            _ => None,
        }
    }

    /// Relative brightness, 0.0 (black) to 1.0 (white), ignoring alpha.
    pub fn luma(self) -> f32 {
        (0.2126 * self.r as f32 + 0.7152 * self.g as f32 + 0.0722 * self.b as f32) / 255.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Light,
    Dark,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SyntaxStyle {
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Style {
    #[serde(default)]
    syntax: BTreeMap<String, SyntaxStyle>,
    /// Everything else: dotted color keys (`editor.background`) and lists such as `players`.
    #[serde(flatten)]
    rest: BTreeMap<String, serde_json_lenient::Value>,
}

impl Style {
    /// The color under a dotted key such as `border.variant`. `None` when it is absent, null or unreadable.
    pub fn color(&self, key: &str) -> Option<Rgba> {
        Rgba::parse(self.rest.get(key)?.as_str()?)
    }

    /// The color of a syntax capture such as `keyword` or `string.escape`.
    pub fn syntax_color(&self, key: &str) -> Option<Rgba> {
        Rgba::parse(self.syntax.get(key)?.color.as_deref()?)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Theme {
    pub name: String,
    pub appearance: Appearance,
    #[serde(default)]
    pub style: Style,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Family {
    pub name: String,
    #[serde(default)]
    pub author: String,
    pub themes: Vec<Theme>,
}

pub fn parse_family(text: &str) -> Result<Family, String> {
    serde_json_lenient::from_str(text).map_err(|e| format!("theme file: {e}"))
}

/// Where a theme lives: enough to find it again after a restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeRef {
    pub extension_id: String,
    pub extension_name: String,
    /// Path of the theme file inside the extension, as the manifest lists it.
    pub file: String,
    pub name: String,
    pub appearance: Appearance,
}

impl ThemeRef {
    /// `extension-id|file|theme name`, for settings.
    pub fn key(&self) -> String {
        format!("{}|{}|{}", self.extension_id, self.file, self.name)
    }
}

fn read_family(installer: &Installer, manifest: &Manifest, file: &str) -> Option<Family> {
    let path = Manifest::resolve(&installer.dir(&manifest.id), file)?;
    if fs::metadata(&path).ok()?.len() > MAX_THEME_BYTES {
        return None;
    }
    parse_family(&fs::read_to_string(path).ok()?).ok()
}

/// Every theme of every installed extension, by extension then file order.
/// Files that cannot be read are skipped.
pub fn installed_themes(installer: &Installer) -> Vec<ThemeRef> {
    let mut out = Vec::new();
    for manifest in installer.installed() {
        for file in &manifest.themes {
            let Some(family) = read_family(installer, &manifest, file) else { continue };
            for theme in family.themes {
                out.push(ThemeRef {
                    extension_id: manifest.id.clone(),
                    extension_name: manifest.name.clone(),
                    file: file.clone(),
                    name: theme.name,
                    appearance: theme.appearance,
                });
            }
        }
    }
    out
}

/// Reads the theme a [`ThemeRef`] (or its [`key`](ThemeRef::key)) points at.
pub fn load_theme(installer: &Installer, extension_id: &str, file: &str, name: &str) -> Option<Theme> {
    let manifest = installer.manifest(extension_id)?;
    // Only files the manifest lists are themes.
    if !manifest.themes.iter().any(|t| t == file) {
        return None;
    }
    read_family(installer, &manifest, file)?.themes.into_iter().find(|t| t.name == name)
}

/// Splits a [`ThemeRef::key`] back into extension id, file and theme name.
pub fn split_key(key: &str) -> Option<(&str, &str, &str)> {
    let mut parts = key.splitn(3, '|');
    Some((parts.next()?, parts.next()?, parts.next()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r##"
    // A comment, as some theme files have.
    {
      "$schema": "https://zed.dev/schema/themes/v0.2.0.json",
      "name": "Zord",
      "author": "A",
      "themes": [
        {
          "name": "Zord - Dark",
          "appearance": "dark",
          "style": {
            "accents": ["#bd93f9ff"],
            "background": "#171c20",
            "editor.background": "#11161a",
            "border.transparent": null,
            "text": "#e0e0e0ff",
            "players": [{"cursor": "#fff", "background": "#000", "selection": "#0003"}],
            "syntax": {
              "keyword": {"color": "#ff79c6", "font_style": null, "font_weight": null},
              "comment": {"color": "#6272a4",},
              "plain": {"font_style": "italic"}
            },
          }
        },
        {"name": "Zord - Light", "appearance": "light", "style": {"background": "#fff"}}
      ]
    }"##;

    #[test]
    fn hex_colors_in_all_four_spellings() {
        assert_eq!(Rgba::parse("#fff"), Some(Rgba { r: 255, g: 255, b: 255, a: 255 }));
        assert_eq!(Rgba::parse("#f008"), Some(Rgba { r: 255, g: 0, b: 0, a: 0x88 }));
        assert_eq!(Rgba::parse("#171c20"), Some(Rgba { r: 0x17, g: 0x1c, b: 0x20, a: 255 }));
        assert_eq!(Rgba::parse("#bd93f9ff"), Some(Rgba { r: 0xbd, g: 0x93, b: 0xf9, a: 255 }));
        for bad in ["", "fff", "#ff", "#fffff", "#gggggg", "#é1é1é1", "rgb(1,2,3)"] {
            assert_eq!(Rgba::parse(bad), None, "{bad:?}");
        }
        assert!(Rgba::parse("#ffffff").unwrap().luma() > 0.99 && Rgba::parse("#000").unwrap().luma() < 0.01);
    }

    #[test]
    fn a_lenient_theme_file_is_read_with_its_colors_and_syntax() {
        let family = parse_family(FILE).unwrap();
        assert_eq!((family.name.as_str(), family.themes.len()), ("Zord", 2));
        let dark = &family.themes[0];
        assert_eq!(dark.appearance, Appearance::Dark);
        assert_eq!(dark.style.color("editor.background"), Rgba::parse("#11161a"));
        assert_eq!(dark.style.color("border.transparent"), None, "null is no color");
        assert_eq!(dark.style.color("accents"), None, "a list is no color");
        assert_eq!(dark.style.color("missing"), None);
        assert_eq!(dark.style.syntax_color("keyword"), Rgba::parse("#ff79c6"));
        assert_eq!(dark.style.syntax_color("plain"), None, "a style without a color");
        assert_eq!(family.themes[1].appearance, Appearance::Light);
    }

    #[test]
    fn installed_themes_are_listed_and_loaded_by_key() {
        let dir = tempfile::tempdir().unwrap();
        let installer = Installer::new(dir.path());
        let ext = installer.dir("zord");
        fs::create_dir_all(ext.join("themes")).unwrap();
        fs::write(ext.join("extension.toml"), "id = \"zord\"\nname = \"Zord Theme\"\nversion = \"1\"\nthemes = [\"themes/zord.json\", \"themes/missing.json\", \"../escape.json\"]\n").unwrap();
        fs::write(ext.join("themes/zord.json"), FILE).unwrap();
        fs::write(dir.path().join("installed/escape.json"), FILE).unwrap();

        let all = installed_themes(&installer);
        assert_eq!(all.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Zord - Dark", "Zord - Light"]);
        assert_eq!(all[0].extension_name, "Zord Theme");

        let key = all[1].key();
        let (id, file, name) = split_key(&key).unwrap();
        let loaded = load_theme(&installer, id, file, name).unwrap();
        assert_eq!(loaded.appearance, Appearance::Light);
        // Only files the manifest lists, and only inside the extension.
        assert!(load_theme(&installer, "zord", "themes/other.json", "Zord - Dark").is_none());
        assert!(load_theme(&installer, "zord", "../escape.json", "Zord - Dark").is_none());
        assert!(load_theme(&installer, "zord", "themes/zord.json", "No such").is_none());
    }
}
