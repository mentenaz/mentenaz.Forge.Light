//! The Extensions panel's state: registry listings, installs and installed themes.
//! Network and unpacking run on worker threads; their answers are queued and
//! applied on the UI thread when `ext-changed` fires.

use super::*;
use editor_core::{GrammarSource, LoadedGrammar};
use forge_extensions::{Entry, Extensions, Provides, ThemeRef};

pub(crate) enum ExtReply {
    Listed { seq: u64, result: Result<Vec<Entry>, String> },
    Installed { id: String, result: Result<(), String> },
    /// An installed language's grammar, loaded on a worker thread.
    Grammar { def: Arc<LanguageDef>, result: Result<LoadedGrammar, String> },
}

impl Shell {
    /// Rereads what is installed and the themes in it.
    pub(crate) fn reload_extensions(&mut self) {
        let Some(ext) = &self.ext else { return };
        self.ext_installed = ext.installer.installed().into_iter().map(|m| (m.id, m.version)).collect();
        self.ext_themes = forge_extensions::installed_themes(&ext.installer);
        let installed = forge_extensions::installed_languages(&ext.installer).into_iter().map(|l| LanguageDef {
            name: l.name,
            suffixes: l.suffixes,
            source: GrammarSource::Wasm { grammar: l.grammar, wasm: l.grammar_wasm, highlights: l.highlights, version: l.version },
        });
        self.set_languages(Languages::with_installed(installed));
    }

    /// Switches to a new set of languages: files take the language their name now selects,
    /// and grammars of languages that changed or went away are dropped.
    pub(crate) fn set_languages(&mut self, new: Languages) {
        for old in self.languages.all().iter().filter(|d| !d.is_built_in()) {
            if new.by_name(&old.name).is_none_or(|d| *d != **old) {
                self.highlighter.borrow_mut().forget(&old.name);
                self.grammar_failed.remove(&old.name);
            }
        }
        self.languages = new;
        for tab in self.groups.iter_mut().flatten() {
            let lang = tab.path.as_deref().and_then(|p| self.languages.for_path(p));
            if lang != tab.lang {
                tab.lang = lang;
                tab.reset_highlighting();
            }
        }
        self.ensure_grammars();
    }

    /// Starts loading the grammar of every open file's installed language that is neither
    /// loaded, loading, nor known to fail. Needs the window, to be told when a load finishes.
    pub(crate) fn ensure_grammars(&mut self) {
        let Some(weak) = self.weak.clone() else { return };
        let mut wanted: Vec<Arc<LanguageDef>> = Vec::new();
        for lang in self.groups.iter().flatten().filter_map(|t| t.lang.as_ref()).filter(|l| !l.is_built_in()) {
            if !wanted.iter().any(|w| w.name == lang.name) {
                wanted.push(lang.clone());
            }
        }
        for def in wanted {
            if self.highlighter.borrow().is_ready(&def) || self.grammar_failed.contains(&def.name) || !self.grammar_loading.insert(def.name.clone()) {
                continue;
            }
            let (replies, weak) = (self.ext_replies.clone(), weak.clone());
            std::thread::spawn(move || {
                let result = editor_core::load_grammar(&def);
                replies.lock().unwrap().push_back(ExtReply::Grammar { def, result });
                let _ = weak.upgrade_in_event_loop(|ui| ui.invoke_ext_changed());
            });
        }
    }

    /// Fetches the registry listing for the current query. Without `force`, only the first time.
    pub(crate) fn ext_refresh(&mut self, weak: slint::Weak<AppWindow>, force: bool) {
        if !force && (self.ext_loaded || self.ext_loading) {
            return;
        }
        let Some(ext) = self.ext.clone() else {
            self.ext_status = "Extensions are unavailable: there is no data directory.".into();
            return;
        };
        self.ext_loading = true;
        self.ext_status = "Loading…".into();
        self.ext_seq += 1;
        let (seq, replies, query, kind) = (self.ext_seq, self.ext_replies.clone(), self.ext_query.clone(), self.ext_kind);
        std::thread::spawn(move || {
            let provides: &[Provides] = match kind {
                0 => &[Provides::Themes],
                1 => &[Provides::Languages],
                _ => &[],
            };
            let result = ext.registry.list(Some(&query), provides).map_err(|e| e.to_string());
            replies.lock().unwrap().push_back(ExtReply::Listed { seq, result });
            let _ = weak.upgrade_in_event_loop(|ui| ui.invoke_ext_changed());
        });
    }

    pub(crate) fn ext_search(&mut self, weak: slint::Weak<AppWindow>, query: String, kind: i32) {
        self.ext_query = query;
        self.ext_kind = kind;
        self.ext_refresh(weak, true);
    }

    pub(crate) fn ext_install(&mut self, weak: slint::Weak<AppWindow>, id: String) {
        let Some(ext) = self.ext.clone() else { return };
        if !self.ext_busy.insert(id.clone()) {
            return;
        }
        self.ext_status = format!("Installing {id}…");
        let replies = self.ext_replies.clone();
        std::thread::spawn(move || {
            let result = ext.install_latest(&id).map(|_| ()).map_err(|e| e.to_string());
            replies.lock().unwrap().push_back(ExtReply::Installed { id, result });
            let _ = weak.upgrade_in_event_loop(|ui| ui.invoke_ext_changed());
        });
    }

    /// Applies queued answers. Returns whether anything changed.
    pub(crate) fn apply_ext_replies(&mut self) -> bool {
        let replies: Vec<ExtReply> = self.ext_replies.lock().unwrap().drain(..).collect();
        let mut changed = false;
        for reply in replies {
            match reply {
                // Only the newest listing counts; an older one may arrive late.
                ExtReply::Listed { seq, result } if seq == self.ext_seq => {
                    self.ext_loading = false;
                    match result {
                        Ok(entries) => {
                            self.ext_status = if entries.is_empty() { "No extensions found.".into() } else { format!("{} extensions", entries.len()) };
                            self.ext_entries = entries;
                            self.ext_loaded = true;
                        }
                        Err(e) => self.ext_status = e,
                    }
                    changed = true;
                }
                ExtReply::Listed { .. } => {}
                ExtReply::Grammar { def, result } => {
                    self.grammar_loading.remove(&def.name);
                    // The extension may have been updated or removed while the grammar loaded.
                    if self.languages.by_name(&def.name).is_some_and(|d| *d == *def) {
                        match result {
                            Ok(grammar) => {
                                self.highlighter.borrow_mut().install(&def.name, grammar);
                                for tab in self.groups.iter_mut().flatten().filter(|t| t.lang.as_ref().is_some_and(|l| l.name == def.name)) {
                                    tab.reset_highlighting();
                                }
                            }
                            Err(e) => {
                                self.grammar_failed.insert(def.name.clone());
                                self.notify(2, &format!("{} highlighting unavailable", def.name), e);
                            }
                        }
                    }
                    changed = true;
                }
                ExtReply::Installed { id, result } => {
                    self.ext_busy.remove(&id);
                    match result {
                        Ok(()) => {
                            self.reload_extensions();
                            self.ext_status = format!("Installed {id}");
                        }
                        Err(e) => self.ext_status = format!("Could not install {id}: {e}"),
                    }
                    changed = true;
                }
            }
        }
        self.ensure_grammars();
        changed
    }

    /// The palette of the installed theme at `index` in `ext_themes`, with its settings key.
    pub(crate) fn theme_palette(&self, index: usize) -> Option<(String, themes::Palette)> {
        let r: &ThemeRef = self.ext_themes.get(index)?;
        let theme = forge_extensions::load_theme(&self.ext.as_ref()?.installer, &r.extension_id, &r.file, &r.name)?;
        Some((r.key(), themes::palette(&theme)))
    }

    /// Remembers the chosen theme (`None` for the built-in one).
    pub(crate) fn set_theme_key(&mut self, key: Option<String>) {
        if let Some(store) = &self.store {
            let _ = store.set_setting("theme", key.as_deref().unwrap_or(""));
        }
        self.theme_key = key;
    }

    /// The theme chosen last time, if it is still installed.
    pub(crate) fn saved_theme(&self) -> Option<(String, themes::Palette)> {
        let key = self.store.as_ref()?.setting("theme").filter(|k| !k.is_empty())?;
        let (id, file, name) = forge_extensions::split_key(&key)?;
        let theme = forge_extensions::load_theme(&self.ext.as_ref()?.installer, id, file, name)?;
        Some((key, themes::palette(&theme)))
    }
}

pub(crate) fn open_extensions() -> Option<Arc<Extensions>> {
    Extensions::open_default().map(Arc::new)
}
