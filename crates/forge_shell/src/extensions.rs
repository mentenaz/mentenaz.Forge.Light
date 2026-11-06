//! The Extensions panel's state: registry listings, installs and installed themes.
//! Network and unpacking run on worker threads; their answers are queued and
//! applied on the UI thread when `ext-changed` fires.

use super::*;
use forge_extensions::{Entry, Extensions, Provides, ThemeRef};

pub(crate) enum ExtReply {
    Listed { seq: u64, result: Result<Vec<Entry>, String> },
    Installed { id: String, result: Result<(), String> },
}

impl Shell {
    /// Rereads what is installed and the themes in it.
    pub(crate) fn reload_extensions(&mut self) {
        let Some(ext) = &self.ext else { return };
        self.ext_installed = ext.installer.installed().into_iter().map(|m| (m.id, m.version)).collect();
        self.ext_themes = forge_extensions::installed_themes(&ext.installer);
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
        let (seq, replies, query, all) = (self.ext_seq, self.ext_replies.clone(), self.ext_query.clone(), self.ext_all);
        std::thread::spawn(move || {
            let provides: &[Provides] = if all { &[] } else { &[Provides::Themes] };
            let result = ext.registry.list(Some(&query), provides).map_err(|e| e.to_string());
            replies.lock().unwrap().push_back(ExtReply::Listed { seq, result });
            let _ = weak.upgrade_in_event_loop(|ui| ui.invoke_ext_changed());
        });
    }

    pub(crate) fn ext_search(&mut self, weak: slint::Weak<AppWindow>, query: String, all: bool) {
        self.ext_query = query;
        self.ext_all = all;
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
