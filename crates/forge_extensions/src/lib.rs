//! Installing extensions from the Forge registry. No UI and no async runtime:
//! callers run the (blocking) calls on a worker thread.

mod install;
mod languages;
mod manifest;
mod registry;
mod theme;

pub use install::Installer;
pub use languages::{InstalledLanguage, installed_languages};
pub use manifest::{GrammarEntry, LanguageServerEntry, LibEntry, Manifest, Provides, valid_id};
pub use theme::{Appearance, Family, Rgba, Style, Theme, ThemeRef, installed_themes, load_theme, parse_family, split_key};
pub use registry::{DEFAULT_BASE_URL, Entry, Registry, SCHEMA_VERSIONS, WASM_API_VERSIONS};

use std::fmt;
use std::path::PathBuf;

#[derive(Debug)]
pub enum Error {
    /// The id (or version) is not a plain name, so it must not reach a path or a URL.
    InvalidId(String),
    /// The registry has no such extension.
    NotFound(String),
    /// The registry could not be reached, or answered something unusable.
    Registry(String),
    /// The downloaded archive is unusable or unsafe.
    Archive(String),
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidId(id) => write!(f, "{id:?} is not a valid extension id"),
            Error::NotFound(id) => write!(f, "the extension {id:?} is not in the registry"),
            Error::Registry(m) => write!(f, "extension registry: {m}"),
            Error::Archive(m) => write!(f, "extension archive: {m}"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// The registry together with the directory it installs into.
pub struct Extensions {
    pub registry: Registry,
    pub installer: Installer,
}

impl Extensions {
    pub fn new(registry: Registry, root: impl AsRef<std::path::Path>) -> Self {
        Extensions { registry, installer: Installer::new(root) }
    }

    /// The default registry, installing under [`default_root`].
    pub fn open_default() -> Option<Self> {
        Some(Extensions::new(Registry::default(), default_root()?))
    }

    /// Downloads and installs the latest compatible version of `id`.
    pub fn install_latest(&self, id: &str) -> Result<Manifest, Error> {
        let archive = self.registry.download(id, None)?;
        self.installer.install(id, &archive)
    }

    /// Installed extensions for which the registry has a different version, as registry entries.
    pub fn updates(&self) -> Result<Vec<Entry>, Error> {
        let installed = self.installer.installed();
        if installed.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<&str> = installed.iter().map(|m| m.id.as_str()).collect();
        let latest = self.registry.latest(&ids)?;
        Ok(latest.into_iter().filter(|e| installed.iter().any(|m| m.id == e.id && m.version != e.version)).collect())
    }
}

/// `FORGE_EXTENSIONS`, or `extensions` beside the app's database in `%APPDATA%\ForgeSlint`.
pub fn default_root() -> Option<PathBuf> {
    std::env::var_os("FORGE_EXTENSIONS").map(PathBuf::from).or_else(|| std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("ForgeSlint").join("extensions")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Installs a theme and a language extension from the real registry:
    /// `cargo test -p forge_extensions -- --ignored`.
    #[test]
    #[ignore]
    fn live_install_of_a_theme_and_a_language() {
        let dir = tempfile::tempdir().unwrap();
        let ext = Extensions::new(Registry::default(), dir.path());
        let theme = ext.install_latest("zord-theme").unwrap();
        assert!(theme.provides().contains(&Provides::Themes));
        let theme_file = Manifest::resolve(&ext.installer.dir("zord-theme"), &theme.themes[0]).unwrap();
        assert!(theme_file.is_file(), "{theme_file:?}");

        let lang = ext.install_latest("vrl").unwrap();
        assert!(lang.grammars.contains_key("vrl"));
        assert!(ext.installer.dir("vrl").join("grammars/vrl.wasm").is_file());
        assert!(ext.installer.dir("vrl").join("languages/vrl/config.toml").is_file());

        assert_eq!(ext.installer.installed().len(), 2);
        assert!(matches!(ext.install_latest("no-such-extension-xyz"), Err(Error::NotFound(_) | Error::Registry(_))));
    }

    /// Installs a sample of real theme extensions and reads every theme in them.
    #[test]
    #[ignore]
    fn live_theme_extensions_parse() {
        let dir = tempfile::tempdir().unwrap();
        let ext = Extensions::new(Registry::default(), dir.path());
        let listed = ext.registry.list(None, &[Provides::Themes]).unwrap();
        let mut failures = Vec::new();
        let (mut themes, mut tried) = (0, 0);
        for entry in listed.iter().step_by(listed.len() / 40 + 1) {
            tried += 1;
            match ext.install_latest(&entry.id) {
                Ok(m) => {
                    let found = installed_themes(&ext.installer).into_iter().filter(|t| t.extension_id == m.id).count();
                    themes += found;
                    if found == 0 {
                        failures.push(format!("{}: no readable theme in {:?}", entry.id, m.themes));
                    }
                }
                Err(e) => failures.push(format!("{}: install failed: {e}", entry.id)),
            }
        }
        eprintln!("tried {tried} extensions, read {themes} themes, {} failures", failures.len());
        for f in &failures {
            eprintln!("  {f}");
        }
        assert!(failures.len() * 10 <= tried, "too many failures");
    }
}
