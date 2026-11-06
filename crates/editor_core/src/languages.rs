//! The languages the editor can highlight: built into the binary, or installed
//! from extensions as a tree-sitter grammar compiled to wasm plus a highlight query.

use crate::syntax::Language;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Where a language's grammar comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrammarSource {
    BuiltIn(Language),
    /// From an installed extension. `version` is the extension's, so a reinstalled
    /// newer version is told apart from the one already loaded.
    Wasm { grammar: String, wasm: PathBuf, highlights: Option<PathBuf>, version: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageDef {
    pub name: String,
    /// File extensions (`rs`) and whole file names (`Makefile`), without a leading dot.
    pub suffixes: Vec<String>,
    pub source: GrammarSource,
}

impl LanguageDef {
    pub fn is_built_in(&self) -> bool {
        matches!(self.source, GrammarSource::BuiltIn(_))
    }
}

/// The built-in languages followed by the installed ones.
#[derive(Clone, Debug)]
pub struct Languages {
    defs: Vec<Arc<LanguageDef>>,
}

const BUILT_IN: [(&str, &[&str], Language); 9] = [
    ("Rust", &["rs"], Language::Rust),
    ("TOML", &["toml"], Language::Toml),
    ("JSON", &["json", "jsonc"], Language::Json),
    ("Python", &["py"], Language::Python),
    ("JavaScript", &["js", "mjs", "cjs", "jsx"], Language::JavaScript),
    ("Shell Script", &["sh", "bash"], Language::Bash),
    ("C#", &["cs"], Language::CSharp),
    ("CSS", &["css"], Language::Css),
    ("HTML", &["html", "htm"], Language::Html),
];

impl Default for Languages {
    fn default() -> Self {
        Languages::built_in()
    }
}

impl Languages {
    pub fn built_in() -> Self {
        Languages {
            defs: BUILT_IN
                .iter()
                .map(|(name, suffixes, lang)| Arc::new(LanguageDef { name: (*name).into(), suffixes: suffixes.iter().map(|s| (*s).into()).collect(), source: GrammarSource::BuiltIn(*lang) }))
                .collect(),
        }
    }

    /// The built-in languages plus `installed`. An installed language whose name is already
    /// taken (by a built-in or an earlier one) is dropped, so names stay unique.
    pub fn with_installed(installed: impl IntoIterator<Item = LanguageDef>) -> Self {
        let mut all = Languages::built_in();
        for def in installed {
            if all.by_name(&def.name).is_none() {
                all.defs.push(Arc::new(def));
            }
        }
        all
    }

    pub fn all(&self) -> &[Arc<LanguageDef>] {
        &self.defs
    }

    pub fn by_name(&self, name: &str) -> Option<Arc<LanguageDef>> {
        self.defs.iter().find(|d| d.name.eq_ignore_ascii_case(name)).cloned()
    }

    /// The language of a file. The longest matching suffix wins, so `Cargo.toml` can have
    /// its own language over `toml`; at equal length the earlier language, a built-in
    /// before any installed one, wins.
    pub fn for_path(&self, path: &Path) -> Option<Arc<LanguageDef>> {
        let file = path.file_name()?.to_str()?.to_ascii_lowercase();
        let mut best: Option<(usize, &Arc<LanguageDef>)> = None;
        for def in &self.defs {
            for suffix in &def.suffixes {
                let suffix = suffix.to_ascii_lowercase();
                let hit = !suffix.is_empty() && (file == suffix || file.strip_suffix(&suffix).is_some_and(|head| head.ends_with('.')));
                if hit && best.is_none_or(|(len, _)| suffix.len() > len) {
                    best = Some((suffix.len(), def));
                }
            }
        }
        best.map(|(_, d)| d.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wasm(name: &str, suffixes: &[&str]) -> LanguageDef {
        LanguageDef {
            name: name.into(),
            suffixes: suffixes.iter().map(|s| (*s).into()).collect(),
            source: GrammarSource::Wasm { grammar: name.to_ascii_lowercase(), wasm: PathBuf::from("g.wasm"), highlights: None, version: "1".into() },
        }
    }

    fn name(languages: &Languages, path: &str) -> Option<String> {
        languages.for_path(Path::new(path)).map(|d| d.name.clone())
    }

    #[test]
    fn built_in_languages_are_found_by_extension_in_any_case() {
        let l = Languages::built_in();
        assert_eq!(name(&l, "src/main.rs").as_deref(), Some("Rust"));
        assert_eq!(name(&l, "X.JSONC").as_deref(), Some("JSON"));
        assert_eq!(name(&l, "page.htm").as_deref(), Some("HTML"));
        assert_eq!(name(&l, "notes.txt"), None);
        assert_eq!(name(&l, "rs"), Some("Rust".into()), "a file named exactly like a suffix matches it");
        assert_eq!(name(&l, "barrs"), None, "a suffix needs its dot");
    }

    #[test]
    fn installed_languages_match_extensions_and_whole_file_names() {
        let l = Languages::with_installed([wasm("Vrl", &["vrl"]), wasm("Make", &["Makefile", "mk"])]);
        assert_eq!(name(&l, "a/b/x.vrl").as_deref(), Some("Vrl"));
        assert_eq!(name(&l, "Makefile").as_deref(), Some("Make"));
        assert_eq!(name(&l, "rules.mk").as_deref(), Some("Make"));
        assert_eq!(name(&l, "NotMakefile"), None);
    }

    #[test]
    fn a_built_in_wins_an_equal_claim_but_a_more_specific_suffix_wins() {
        let l = Languages::with_installed([wasm("Fake Rust", &["rs"]), wasm("Cargo", &["Cargo.toml"])]);
        assert_eq!(name(&l, "lib.rs").as_deref(), Some("Rust"));
        assert_eq!(name(&l, "Cargo.toml").as_deref(), Some("Cargo"));
        assert_eq!(name(&l, "other.toml").as_deref(), Some("TOML"));
    }

    #[test]
    fn names_stay_unique_and_built_ins_cannot_be_replaced() {
        let l = Languages::with_installed([wasm("rust", &["rsx"]), wasm("Zig", &["zig"]), wasm("zig", &["zon"])]);
        assert_eq!(l.all().len(), 9 + 1);
        assert!(l.by_name("RUST").unwrap().is_built_in());
        assert_eq!(name(&l, "a.rsx"), None, "the dropped duplicate claims nothing");
        assert_eq!(name(&l, "a.zon"), None);
    }
}
