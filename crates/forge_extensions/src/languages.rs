//! Languages provided by installed extensions: `languages/<dir>/config.toml`,
//! its tree-sitter queries, and the grammar compiled to wasm in `grammars/`.
//!
//! The config format follows Zed's language `config.toml`; only the fields Forge uses
//! are read.

use crate::{Installer, Manifest};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

/// A `config.toml` larger than this is not read.
const MAX_CONFIG_BYTES: u64 = 256 * 1024;

#[derive(Deserialize)]
struct Config {
    name: String,
    #[serde(default)]
    grammar: Option<String>,
    #[serde(default)]
    path_suffixes: Vec<String>,
    #[serde(default)]
    hidden: bool,
}

/// One language of an installed extension that has a grammar to highlight it with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledLanguage {
    pub extension_id: String,
    /// The extension's version, to tell an updated grammar from the one loaded.
    pub version: String,
    pub name: String,
    pub grammar: String,
    pub suffixes: Vec<String>,
    pub grammar_wasm: PathBuf,
    /// `highlights.scm`, when the extension has one for this language.
    pub highlights: Option<PathBuf>,
}

/// Every language of every installed extension, by extension then manifest order.
/// Languages that are hidden, have no grammar, claim no file type, or whose grammar
/// file is missing are left out.
pub fn installed_languages(installer: &Installer) -> Vec<InstalledLanguage> {
    let mut out = Vec::new();
    for manifest in installer.installed() {
        let root = installer.dir(&manifest.id);
        for dir in &manifest.languages {
            let Some(dir) = Manifest::resolve(&root, dir) else { continue };
            let config = dir.join("config.toml");
            let Ok(meta) = fs::metadata(&config) else { continue };
            if meta.len() > MAX_CONFIG_BYTES {
                continue;
            }
            let Some(config) = fs::read_to_string(&config).ok().and_then(|t| toml::from_str::<Config>(&t).ok()) else { continue };
            let Some(grammar) = config.grammar.filter(|g| !g.is_empty()) else { continue };
            if config.hidden || config.path_suffixes.is_empty() {
                continue;
            }
            let Some(grammar_wasm) = Manifest::resolve(&root, &format!("grammars/{grammar}.wasm")).filter(|p| p.is_file()) else { continue };
            let highlights = Some(dir.join("highlights.scm")).filter(|p| p.is_file());
            out.push(InstalledLanguage {
                extension_id: manifest.id.clone(),
                version: manifest.version.clone(),
                name: config.name,
                grammar,
                suffixes: config.path_suffixes,
                grammar_wasm,
                highlights,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install(dir: &std::path::Path, id: &str, version: &str, languages: &[(&str, &str)], grammar_files: &[&str]) {
        let root = dir.join("installed").join(id);
        let dirs: Vec<String> = languages.iter().map(|(d, _)| format!("\"languages/{d}\"")).collect();
        fs::create_dir_all(root.join("grammars")).unwrap();
        fs::write(root.join("extension.toml"), format!("id = \"{id}\"\nname = \"{id}\"\nversion = \"{version}\"\nlanguages = [{}]\n", dirs.join(", "))).unwrap();
        for (d, config) in languages {
            fs::create_dir_all(root.join("languages").join(d)).unwrap();
            fs::write(root.join("languages").join(d).join("config.toml"), config).unwrap();
            fs::write(root.join("languages").join(d).join("highlights.scm"), "(comment) @comment").unwrap();
        }
        for g in grammar_files {
            fs::write(root.join("grammars").join(format!("{g}.wasm")), b"\0asm").unwrap();
        }
    }

    #[test]
    fn languages_with_a_grammar_are_listed_with_their_files() {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), "vrl", "0.2.0", &[("vrl", "name = \"VRL\"\ngrammar = \"vrl\"\npath_suffixes = [\"vrl\"]\nline_comments = [\"# \"]\n")], &["vrl"]);
        let installer = Installer::new(dir.path());
        let found = installed_languages(&installer);
        assert_eq!(found.len(), 1);
        let l = &found[0];
        assert_eq!((l.extension_id.as_str(), l.version.as_str(), l.name.as_str(), l.grammar.as_str()), ("vrl", "0.2.0", "VRL", "vrl"));
        assert_eq!(l.suffixes, ["vrl"]);
        assert!(l.grammar_wasm.ends_with("grammars/vrl.wasm") && l.grammar_wasm.is_file());
        assert!(l.highlights.as_ref().is_some_and(|h| h.ends_with("languages/vrl/highlights.scm")));
    }

    #[test]
    fn unusable_languages_are_left_out() {
        let dir = tempfile::tempdir().unwrap();
        install(
            dir.path(),
            "mixed",
            "1",
            &[
                ("ok", "name = \"Ok\"\ngrammar = \"ok\"\npath_suffixes = [\"ok\"]\n"),
                ("nogrammar", "name = \"NoGrammar\"\npath_suffixes = [\"ng\"]\n"),
                ("nosuffix", "name = \"NoSuffix\"\ngrammar = \"nosuffix\"\n"),
                ("hidden", "name = \"Hidden\"\ngrammar = \"hidden\"\npath_suffixes = [\"h\"]\nhidden = true\n"),
                ("nowasm", "name = \"NoWasm\"\ngrammar = \"missing\"\npath_suffixes = [\"nw\"]\n"),
                ("escape", "name = \"Escape\"\ngrammar = \"../../../outside\"\npath_suffixes = [\"e\"]\n"),
                ("broken", "this is = not [ valid toml"),
            ],
            &["ok", "nosuffix", "hidden"],
        );
        let names: Vec<String> = installed_languages(&Installer::new(dir.path())).into_iter().map(|l| l.name).collect();
        assert_eq!(names, ["Ok"]);
    }

    #[test]
    fn a_language_directory_outside_the_extension_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("installed/evil");
        fs::create_dir_all(root.join("grammars")).unwrap();
        fs::write(root.join("extension.toml"), "id = \"evil\"\nname = \"evil\"\nversion = \"1\"\nlanguages = [\"../../outside\"]\n").unwrap();
        fs::create_dir_all(dir.path().join("outside")).unwrap();
        fs::write(dir.path().join("outside/config.toml"), "name = \"X\"\ngrammar = \"x\"\npath_suffixes = [\"x\"]\n").unwrap();
        assert!(installed_languages(&Installer::new(dir.path())).is_empty());
    }
}
