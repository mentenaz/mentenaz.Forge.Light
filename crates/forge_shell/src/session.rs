use super::*;

/// What would be saved for the project right now.
pub(crate) fn snapshot(ui: &AppWindow, s: &Shell) -> store::Session {
    let pos = ui.window().position();
    let size = ui.window().size();
    // A minimised window reports a position far off screen; that isn't worth keeping.
    let window = (pos.x > -30000 && size.width > 0).then_some(store::WindowRect { x: pos.x, y: pos.y, w: size.width, h: size.height });
    store::Session {
        layout: store::Layout {
            panels: [ui.get_left_panel().to_string(), ui.get_right_panel().to_string(), ui.get_bottom_panel().to_string()],
            last_panels: s.last_panel.clone(),
            left_w: ui.get_left_w(),
            right_w: ui.get_right_w(),
            bottom_h: ui.get_bottom_h(),
            active_group: s.group as u8,
            window,
        },
        tabs: s.tab_states(),
    }
}

/// Saves what a restart should bring back: the session, and unsaved edits.
pub(crate) fn persist(ui: &AppWindow, s: &mut Shell) {
    persist_session(ui, s);
    s.persist_drafts();
}

/// Writes the session if it changed since the last write. A failed write is noted and skipped.
pub(crate) fn persist_session(ui: &AppWindow, s: &mut Shell) {
    if s.store.is_none() {
        return;
    }
    let mut session = snapshot(ui, s);
    if session.layout.window.is_none() {
        // Keep the last good window if it can't be read now (minimised, or closing).
        session.layout.window = s.last_saved.as_ref().and_then(|l| l.layout.window);
    }
    if s.last_saved.as_ref() == Some(&session) {
        return;
    }
    let (key, path) = (path_key(&s.root), s.root.display().to_string());
    if let Some(store) = s.store.as_mut() {
        match store.save_session(&key, &path, &session) {
            Ok(()) => s.last_saved = Some(session),
            Err(e) => eprintln!("[store] couldn't save the session: {e}"),
        }
    }
}

/// Puts a saved layout on screen: docks, sizes and the window itself.
pub(crate) fn apply_layout(ui: &AppWindow, s: &mut Shell, l: &store::Layout) {
    let known = |s: &Shell, id: &str| id.is_empty() || s.panel(id).is_some();
    for dock in 0..3 {
        if known(s, &l.panels[dock]) {
            set_dock_panel(ui, dock as i32, l.panels[dock].as_str().into());
        }
        if !l.last_panels[dock].is_empty() && known(s, &l.last_panels[dock]) {
            s.last_panel[dock] = l.last_panels[dock].clone();
        }
    }
    let sane = |v: f32, lo: f32, hi: f32, default: f32| if v.is_finite() { v.clamp(lo, hi) } else { default };
    ui.set_left_w(sane(l.left_w, 180.0, 640.0, 260.0));
    ui.set_right_w(sane(l.right_w, 180.0, 640.0, 300.0));
    ui.set_bottom_h(sane(l.bottom_h, 100.0, 800.0, 240.0));
    if s.groups[1].is_empty() {
        s.group = 0;
    } else {
        s.group = (l.active_group as usize).min(1);
    }
    if let Some(w) = l.window {
        // Ignore a size or place that couldn't be reached on this screen.
        let size_ok = (760..=10000).contains(&w.w) && (480..=10000).contains(&w.h);
        let place_ok = (-(w.w as i32) + 100..=10000).contains(&w.x) && (0..=10000).contains(&w.y);
        if size_ok {
            ui.window().set_size(slint::PhysicalSize::new(w.w, w.h));
            if place_ok {
                ui.window().set_position(slint::PhysicalPosition::new(w.x, w.y));
            }
        }
    }
}

impl Shell {
    pub(crate) fn set_format_on_save(&mut self, on: bool) {
        self.format_on_save = on;
        if let Some(store) = &self.store {
            let _ = store.set_setting("format_on_save", if on { "true" } else { "false" });
        }
    }

    // ---- sessions ------------------------------------------------------

    /// The open tabs as the database stores them. Tabs that aren't files or
    /// panels (server logs) are left out.
    pub(crate) fn tab_states(&self) -> Vec<store::TabState> {
        let mut out = Vec::new();
        for g in 0..2 {
            for (i, t) in self.groups[g].iter().enumerate() {
                let (kind, target) = match (&t.path, t.key.strip_prefix("panel:")) {
                    (Some(path), _) => (store::TabKind::File, path.display().to_string()),
                    (None, Some(id)) => (store::TabKind::Panel, id.to_string()),
                    _ => continue,
                };
                let caret = t.buffer.as_ref().map_or((0, 0), Buffer::caret);
                let anchor = t.buffer.as_ref().map_or((0, 0), Buffer::anchor_pos);
                out.push(store::TabState { group: g as u8, kind, target, pinned: t.pinned, active: self.active[g] == Some(i), anchor, caret });
            }
        }
        out
    }

    /// Reopens saved tabs. Files that are gone and panels that no longer exist are skipped.
    pub(crate) fn restore_tabs(&mut self, tabs: &[store::TabState]) {
        for t in tabs {
            let g = (t.group as usize).min(1);
            let mut tab = match t.kind {
                store::TabKind::File => {
                    let path = PathBuf::from(&t.target);
                    if !path.is_file() {
                        continue;
                    }
                    Tab::file(path)
                }
                store::TabKind::Panel => match self.panel(&t.target) {
                    Some(p) => Tab::panel(p.1.to_string(), p.2.to_string(), format!("panel:{}", t.target)),
                    None => continue,
                },
            };
            if self.groups[g].iter().any(|x| x.key == tab.key) {
                continue;
            }
            tab.pinned = t.pinned;
            if let Some(buf) = tab.buffer.as_mut() {
                let at = |buf: &Buffer, (line, col): (usize, usize)| {
                    let line = line.min(buf.len_lines() - 1);
                    (line, col.min(buf.line_len(line)))
                };
                let (a, c) = (at(buf, t.anchor), at(buf, t.caret));
                buf.set_caret(a.0, a.1, false);
                buf.set_caret(c.0, c.1, true);
            }
            self.attach_lsp(&tab);
            self.groups[g].push(tab);
            if t.active {
                self.active[g] = Some(self.groups[g].len() - 1);
            }
        }
        for g in 0..2 {
            if self.active[g].is_none() && !self.groups[g].is_empty() {
                self.active[g] = Some(0);
            }
        }
        if self.groups[1].is_empty() {
            self.group = 0;
        }
    }

    pub(crate) fn close_all_tabs(&mut self) {
        self.groups = [Vec::new(), Vec::new()];
        self.active = [None, None];
        self.group = 0;
        self.drafted.clear();
    }

    /// Keeps the database's drafts in step with the unsaved edits: new text is
    /// written, and files that were saved, reverted or closed are forgotten.
    pub(crate) fn persist_drafts(&mut self) {
        if self.store.is_none() {
            return;
        }
        let mut put = Vec::new();
        let mut written = Vec::new();
        let mut live = HashSet::new();
        for (g, tabs) in self.groups.iter().enumerate() {
            for (i, tab) in tabs.iter().enumerate() {
                let (Some(path), Some(buf)) = (&tab.path, &tab.buffer) else { continue };
                if !buf.is_dirty() {
                    tab.draft_rev.set(None);
                    continue;
                }
                let key = path.display().to_string();
                // A file open in both panes has one draft: the first edited copy's.
                if buf.len_bytes() > DRAFT_MAX_BYTES || !live.insert(key.clone()) {
                    continue;
                }
                if tab.draft_rev.get() != Some(buf.revision()) {
                    put.push(store::Draft { path: key, text: buf.text(), base: tab.disk });
                    written.push((g, i, buf.revision()));
                }
            }
        }
        let remove: Vec<String> = self.drafted.iter().filter(|p| !live.contains(*p)).cloned().collect();
        if put.is_empty() && remove.is_empty() {
            return;
        }
        let project = path_key(&self.root);
        let Some(store) = self.store.as_mut() else { return };
        match store.apply_drafts(&project, &put, &remove) {
            Ok(()) => {
                for (g, i, rev) in written {
                    self.groups[g][i].draft_rev.set(Some(rev));
                }
                self.drafted = live;
            }
            Err(e) => eprintln!("[store] couldn't save unsaved edits: {e}"),
        }
    }

    /// Brings back the unsaved edits of the last run. The text goes in as one
    /// undoable change, so undo returns to what is on disk. If the file changed
    /// on disk since, the edits are still restored, with a warning.
    pub(crate) fn restore_drafts(&mut self) {
        let Some(store) = &self.store else { return };
        let drafts = store.load_drafts(&path_key(&self.root)).unwrap_or_default();
        let (mut recovered, mut warnings) = (Vec::new(), Vec::new());
        for d in drafts {
            let path = PathBuf::from(&d.path);
            self.drafted.insert(d.path.clone());
            let found = (0..2).find_map(|g| self.groups[g].iter().position(|t| t.path.as_deref() == Some(path.as_path())).map(|i| (g, i)));
            let (g, i) = found.unwrap_or_else(|| {
                let tab = Tab::file(path.clone());
                self.attach_lsp(&tab);
                self.groups[0].push(tab);
                if self.active[0].is_none() {
                    self.active[0] = Some(self.groups[0].len() - 1);
                }
                (0, self.groups[0].len() - 1)
            });
            let tab = &mut self.groups[g][i];
            let name = tab.title.clone();
            let moved_on = tab.disk != d.base;
            let existed = tab.disk.is_some();
            let buf = tab.buffer.get_or_insert_with(|| Buffer::new(""));
            if buf.text() == d.text {
                continue;
            }
            buf.replace_text(&d.text);
            tab.draft_rev.set(Some(buf.revision()));
            recovered.push(name.clone());
            if !existed {
                warnings.push(format!("{name} no longer exists on disk; saving will create it again."));
            } else if moved_on {
                warnings.push(format!("{name} also changed on disk since these edits were made; saving will replace those changes."));
            }
        }
        if !recovered.is_empty() {
            self.notify(1, "Recovered unsaved changes", recovered.join(", "));
        }
        for w in warnings {
            self.notify(2, "File changed on disk", w);
        }
    }

    pub(crate) fn saved_session(&self) -> Option<store::Session> {
        self.store.as_ref()?.load_session(&path_key(&self.root)).ok().flatten()
    }

    pub(crate) fn set_root(&mut self, root: PathBuf) {
        self.expanded = HashSet::from([root.clone()]);
        self.root = root;
    }
}
