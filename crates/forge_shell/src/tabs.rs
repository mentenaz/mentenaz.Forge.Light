use super::*;

impl Shell {
    // ---- tabs ----------------------------------------------------------

    pub(crate) fn open_tab(&mut self, tab: Tab) {
        let g = self.group;
        if let Some(i) = self.groups[g].iter().position(|t| t.key == tab.key) {
            self.active[g] = Some(i);
        } else {
            self.attach_lsp(&tab);
            self.groups[g].push(tab);
            self.active[g] = Some(self.groups[g].len() - 1);
        }
    }

    /// Opens `path` (or switches to its tab) with the caret at `line` and a UTF-16 column.
    pub(crate) fn goto(&mut self, path: PathBuf, line: usize, col16: usize) {
        self.show_path(path);
        let g = self.group;
        if let Some(buf) = self.active[g].and_then(|a| self.groups[g].get_mut(a)).and_then(|t| t.buffer.as_mut()) {
            let line = line.min(buf.len_lines() - 1);
            let col = utf16_to_char_col(&buf.line(line), col16);
            buf.set_caret(line, col, false);
        }
    }

    /// Opens `path` with the `len` chars at (`line`, `col`) selected.
    pub(crate) fn goto_match(&mut self, path: PathBuf, line: usize, col: usize, len: usize) {
        self.show_path(path);
        let g = self.group;
        if let Some(buf) = self.active[g].and_then(|a| self.groups[g].get_mut(a)).and_then(|t| t.buffer.as_mut()) {
            let line = line.min(buf.len_lines() - 1);
            let col = col.min(buf.line_len(line));
            buf.set_caret(line, col, false);
            buf.set_caret(line, (col + len).min(buf.line_len(line)), true);
        }
    }

    /// Switches to the tab showing `path`, opening it if it isn't.
    pub(crate) fn show_path(&mut self, path: PathBuf) {
        let key = path_key(&path);
        let existing = (0..2).find_map(|g| {
            let i = self.groups[g].iter().position(|t| t.path.as_deref().is_some_and(|p| path_key(p) == key))?;
            Some((g, i))
        });
        match existing {
            Some((g, i)) => {
                self.group = g;
                self.active[g] = Some(i);
            }
            None => self.open_file(normalize_path(&path)),
        }
    }

    pub(crate) fn open_file(&mut self, path: PathBuf) {
        self.open_tab(Tab::file(path));
    }

    pub(crate) fn open_panel_tab(&mut self, id: &str) {
        let (title, icon) = match self.panel(id) {
            Some(p) => (p.1.to_string(), p.2.to_string()),
            None => (id.to_string(), "file".to_string()),
        };
        self.open_tab(Tab::panel(title, icon, format!("panel:{id}")));
    }

    pub(crate) fn close_tab(&mut self, g: usize, i: usize) {
        if i >= self.groups[g].len() {
            return;
        }
        self.groups[g].remove(i);
        self.active[g] = match self.active[g] {
            _ if self.groups[g].is_empty() => None,
            Some(a) if a == i => Some(i.min(self.groups[g].len() - 1)),
            Some(a) if a > i => Some(a - 1),
            other => other,
        };
        if self.groups[1].is_empty() {
            self.group = 0;
        }
    }

    pub(crate) fn close_others(&mut self, g: usize, keep: usize) {
        if keep >= self.groups[g].len() {
            return;
        }
        let kept_key = self.groups[g][keep].key.clone();
        self.groups[g].retain(|t| t.pinned || t.key == kept_key);
        self.active[g] = self.groups[g].iter().position(|t| t.key == kept_key);
    }

    pub(crate) fn toggle_pin(&mut self, g: usize, i: usize) {
        if i >= self.groups[g].len() {
            return;
        }
        let key = self.active[g].and_then(|a| self.groups[g].get(a)).map(|t| t.key.clone());
        let mut tab = self.groups[g].remove(i);
        tab.pinned = !tab.pinned;
        // Pinned tabs sit at the front, in the order they were pinned.
        let at = self.groups[g].iter().take_while(|t| t.pinned).count();
        self.groups[g].insert(at, tab);
        if let Some(key) = key {
            self.active[g] = self.groups[g].iter().position(|t| t.key == key);
        }
    }

    /// Zed's "split": the tab is shown again in the pane on the right.
    pub(crate) fn split_tab(&mut self, g: usize, i: usize) {
        if g != 0 {
            return;
        }
        let Some(t) = self.groups[0].get(i) else { return };
        // The split gets its own copy of the file; edits in one pane don't show in the other.
        let copy = match &t.path {
            Some(path) => Tab::file(path.clone()),
            None => Tab::panel(t.title.clone(), t.icon.clone(), t.key.clone()),
        };
        self.group = 1;
        self.open_tab(copy);
    }

    pub(crate) fn swap_panes(&mut self) {
        if self.groups[1].is_empty() {
            return;
        }
        self.groups.swap(0, 1);
        self.active.swap(0, 1);
        self.group = 1 - self.group;
    }

    pub(crate) fn cycle(&mut self, dir: i32) {
        let g = self.group;
        let n = self.groups[g].len() as i32;
        if n == 0 {
            return;
        }
        let cur = self.active[g].map_or(0, |a| a as i32);
        self.active[g] = Some((cur + dir).rem_euclid(n) as usize);
    }

    // ---- workspace ---------------------------------------------------------

    pub(crate) fn branch(&self) -> Option<String> {
        let head = fs::read_to_string(self.root.join(".git").join("HEAD")).ok()?;
        let head = head.trim();
        Some(head.strip_prefix("ref: refs/heads/").map_or_else(|| head.chars().take(8).collect(), str::to_string))
    }

    pub(crate) fn branches(&self) -> Vec<String> {
        fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
            let Ok(entries) = fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                let name = format!("{prefix}{}", e.file_name().to_string_lossy());
                if e.path().is_dir() {
                    walk(&e.path(), &format!("{name}/"), out);
                } else {
                    out.push(name);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root.join(".git").join("refs").join("heads"), "", &mut out);
        out.sort();
        out
    }

    pub(crate) fn file_rows(&self) -> Vec<(PathBuf, FileRow)> {
        fn walk(shell: &Shell, dir: &Path, depth: i32, out: &mut Vec<(PathBuf, FileRow)>) {
            let Ok(entries) = fs::read_dir(dir) else { return };
            let mut entries: Vec<_> = entries.flatten().map(|e| (e.path(), e.file_name().to_string_lossy().into_owned())).collect();
            entries.sort_by_key(|a| (!a.0.is_dir(), a.1.to_lowercase()));
            for (path, name) in entries {
                let is_dir = path.is_dir();
                if is_dir && SKIPPED_DIRS.contains(&name.as_str()) {
                    continue;
                }
                let expanded = is_dir && shell.expanded.contains(&path);
                out.push((path.clone(), FileRow { name: name.into(), depth, is_dir, expanded }));
                if expanded {
                    walk(shell, &path, depth + 1, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(self, &self.root, 0, &mut out);
        out
    }

    pub(crate) fn language(path: &Path) -> &'static str {
        match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "rs" => "Rust",
            "toml" => "TOML",
            "md" => "Markdown",
            "json" => "JSON",
            "slint" => "Slint",
            "yml" | "yaml" => "YAML",
            "py" => "Python",
            "ts" | "tsx" => "TypeScript",
            "js" => "JavaScript",
            "cs" => "C#",
            "html" => "HTML",
            "css" => "CSS",
            "sh" => "Bash",
            _ => "Plain Text",
        }
    }
}
