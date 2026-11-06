//! `extension.toml`, the manifest at the root of every extension archive.
//!
//! The field names and their meaning follow Zed's `extension` crate
//! (`extension_manifest.rs`, GPL-3.0-or-later), which this registry mirrors.
//! Only what Forge reads is typed; sections it does not use yet stay as raw
//! TOML so a newer manifest still loads.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub lib: LibEntry,
    #[serde(default)]
    pub themes: Vec<String>,
    #[serde(default)]
    pub icon_themes: Vec<String>,
    /// Directories, each holding a `config.toml` and tree-sitter queries.
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub grammars: BTreeMap<String, GrammarEntry>,
    #[serde(default)]
    pub language_servers: BTreeMap<String, LanguageServerEntry>,
    #[serde(default)]
    pub context_servers: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub slash_commands: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub snippets: Option<toml::Value>,
    #[serde(default)]
    pub debug_adapters: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub capabilities: Vec<toml::Value>,
}

/// `[lib]`: present when the extension ships a wasm component.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct LibEntry {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct GrammarEntry {
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub rev: String,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct LanguageServerEntry {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub language_ids: BTreeMap<String, String>,
}

/// What an extension offers; the spelling matches the registry's `provides`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Provides {
    Themes,
    IconThemes,
    Languages,
    Grammars,
    LanguageServers,
    ContextServers,
    Snippets,
    DebugAdapters,
}

impl Provides {
    pub fn as_str(self) -> &'static str {
        match self {
            Provides::Themes => "themes",
            Provides::IconThemes => "icon-themes",
            Provides::Languages => "languages",
            Provides::Grammars => "grammars",
            Provides::LanguageServers => "language-servers",
            Provides::ContextServers => "context-servers",
            Provides::Snippets => "snippets",
            Provides::DebugAdapters => "debug-adapters",
        }
    }
}

impl fmt::Display for Provides {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let manifest: Manifest = toml::from_str(text).map_err(|e| format!("extension.toml: {e}"))?;
        if !valid_id(&manifest.id) {
            return Err(format!("extension.toml: {:?} is not a usable extension id", manifest.id));
        }
        Ok(manifest)
    }

    pub fn provides(&self) -> BTreeSet<Provides> {
        let mut set = BTreeSet::new();
        let mut add = |present: bool, p: Provides| {
            if present {
                set.insert(p);
            }
        };
        add(!self.themes.is_empty(), Provides::Themes);
        add(!self.icon_themes.is_empty(), Provides::IconThemes);
        add(!self.languages.is_empty(), Provides::Languages);
        add(!self.grammars.is_empty(), Provides::Grammars);
        add(!self.language_servers.is_empty(), Provides::LanguageServers);
        add(!self.context_servers.is_empty(), Provides::ContextServers);
        add(self.snippets.is_some(), Provides::Snippets);
        add(!self.debug_adapters.is_empty(), Provides::DebugAdapters);
        set
    }

    /// Whether the extension ships a wasm component.
    pub fn has_lib(&self) -> bool {
        self.lib.kind.is_some() || self.lib.version.is_some()
    }

    /// `rel`, as listed in this manifest, inside the extension's directory `root`.
    /// `None` for anything that could leave it (absolute paths, `..`).
    pub fn resolve(root: &Path, rel: &str) -> Option<PathBuf> {
        let rel = Path::new(rel);
        let safe = !rel.as_os_str().is_empty() && rel.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
        safe.then(|| root.join(rel))
    }
}

/// Ids name a directory and go into URLs, so they are held to a plain alphabet.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && !id.starts_with('.') && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEME: &str = r#"
id = "zord-theme"
name = "Zord Theme"
version = "1.0.0"
schema_version = 1
authors = ["Diogo Pereira <contact@diogopereira.dev>"]
themes = ["themes/zord-dark.json"]
icon_themes = []
languages = []
capabilities = []

[lib]

[grammars]

[language_servers]
"#;

    const LANGUAGE: &str = r#"
id = "vrl"
name = "VRL"
version = "0.2.0"
schema_version = 1
languages = ["languages/vrl"]

[grammars.vrl]
repository = "https://github.com/belltoy/tree-sitter-vrl"
rev = "8cf3be35891e366087c01bef55d3fe96750997a5"
"#;

    #[test]
    fn a_theme_manifest_provides_only_themes() {
        let m = Manifest::parse(THEME).unwrap();
        assert_eq!((m.id.as_str(), m.schema_version), ("zord-theme", 1));
        assert_eq!(m.themes, ["themes/zord-dark.json"]);
        assert_eq!(m.provides().into_iter().collect::<Vec<_>>(), [Provides::Themes]);
        assert!(!m.has_lib());
    }

    #[test]
    fn a_language_manifest_provides_languages_and_grammars() {
        let m = Manifest::parse(LANGUAGE).unwrap();
        assert_eq!(m.grammars["vrl"].rev, "8cf3be35891e366087c01bef55d3fe96750997a5");
        assert_eq!(m.provides().iter().map(|p| p.as_str()).collect::<Vec<_>>(), ["languages", "grammars"]);
    }

    #[test]
    fn language_servers_and_a_wasm_lib_are_recognised() {
        let m = Manifest::parse(
            "id = \"zuban\"\nname = \"Zuban\"\nversion = \"0.0.1\"\n[lib]\nkind = \"Rust\"\nversion = \"0.6.0\"\n[language_servers.zuban]\nname = \"Zuban\"\nlanguages = [\"Python\"]\n",
        )
        .unwrap();
        assert!(m.has_lib());
        assert_eq!(m.language_servers["zuban"].languages, ["Python"]);
        assert!(m.provides().contains(&Provides::LanguageServers));
    }

    #[test]
    fn unknown_sections_are_ignored_and_bad_ids_refused() {
        assert!(Manifest::parse("id = \"a\"\nname = \"A\"\nversion = \"1\"\n[something_new]\nx = 1\n").is_ok());
        for bad in ["", "..", ".hidden", "a/b", "a\\b", "a b", "%2e"] {
            assert!(Manifest::parse(&format!("id = {bad:?}\nname = \"A\"\nversion = \"1\"\n")).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn manifest_paths_cannot_leave_the_extension_directory() {
        let root = Path::new("ext");
        assert_eq!(Manifest::resolve(root, "themes/a.json"), Some(root.join("themes/a.json")));
        for bad in ["", "../x", "a/../../x", "/etc/passwd", "C:\\Windows"] {
            assert_eq!(Manifest::resolve(root, bad), None, "{bad:?}");
        }
    }
}
