use super::*;

/// A server's log as a buffer with the caret at the end.
/// (buffer revision, first line, end line, spans)
pub(crate) type SpanCache = (u64, usize, usize, Vec<Vec<Span>>);

pub(crate) fn log_buffer(lines: &[String]) -> Buffer {
    let mut buf = Buffer::new(&lines.join("\n"));
    buf.doc_end(false);
    buf
}

/// Writes a file tab to disk. `Ok(false)` if the tab has no file.
pub(crate) fn save_tab(tab: &mut Tab) -> Result<bool, String> {
    let (Some(path), Some(buf)) = (&tab.path, tab.buffer.as_mut()) else { return Ok(false) };
    fs::write(path, buf.to_file_text()).map_err(|e| format!("{}: {e}", path.display()))?;
    buf.mark_saved();
    tab.disk = disk_stamp(path);
    // `cargo check` results that follow describe the text as saved now.
    tab.diag.borrow_mut().saved_cursor = buf.edit_cursor();
    Ok(true)
}

/// The LSP `languageId` for a file.
pub(crate) fn language_id(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "rs" => "rust",
        "toml" => "toml",
        "json" => "json",
        "py" => "python",
        "js" => "javascript",
        "cs" => "csharp",
        "css" => "css",
        "html" => "html",
        "sh" => "shellscript",
        _ => "plaintext",
    }
}

pub(crate) struct Tab {
    pub(crate) title: String,
    pub(crate) icon: String,
    /// Identity: the file path, or the panel id for a panel's own tab.
    pub(crate) key: String,
    pub(crate) path: Option<PathBuf>,
    pub(crate) pinned: bool,
    /// The editable text, for files that could be read as UTF-8.
    pub(crate) buffer: Option<Buffer>,
    /// Tree-sitter grammar when there is one; otherwise the simple tokenizer in `syntax`.
    pub(crate) lang: Option<Arc<LanguageDef>>,
    pub(crate) syntax: Option<Syntax>,
    /// The syntax tree, opened on first draw and then kept in step with the buffer.
    pub(crate) parsed: RefCell<Option<Parsed>>,
    pub(crate) parse_failed: Cell<bool>,
    /// Spans of the visible lines: (buffer revision, first line, end line, spans).
    pub(crate) spans: RefCell<Option<SpanCache>>,
    /// The visible line window the view last asked for: first line, count.
    pub(crate) view: (usize, usize),
    /// Find state: the query, whether it is case sensitive, and its matches in the current text.
    pub(crate) find_query: String,
    pub(crate) find_case: bool,
    pub(crate) matches: Vec<Match>,
    pub(crate) lsp: RefCell<Option<TabLsp>>,
    pub(crate) diag: RefCell<TabDiag>,
    pub(crate) hover: RefCell<Option<HoverState>>,
    pub(crate) completion: RefCell<Option<CompletionSession>>,
    pub(crate) signature: RefCell<Option<SignatureState>>,
    pub(crate) actions: RefCell<Option<ActionMenu>>,
    /// The line the lightbulb shows on, when the server has actions for the caret there.
    pub(crate) bulb: Cell<Option<usize>>,
    /// The file's length and modified time (ms) when this tab last read or wrote it.
    pub(crate) disk: Option<(u64, i64)>,
    /// The buffer revision whose text sits in the drafts table, if any.
    pub(crate) draft_rev: Cell<Option<u64>>,
}

/// A file's length and modified time in ms, to tell whether it changed behind our back.
pub(crate) fn disk_stamp(path: &Path) -> Option<(u64, i64)> {
    let meta = fs::metadata(path).ok()?;
    let ms = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64;
    Some((meta.len(), ms))
}

/// Buffers larger than this aren't kept as drafts.
pub(crate) const DRAFT_MAX_BYTES: usize = 2 * 1024 * 1024;

impl Tab {
    pub(crate) fn panel(title: String, icon: String, key: String) -> Self {
        Self { title, icon, key, path: None, pinned: false, buffer: None, lang: None, syntax: None, parsed: RefCell::new(None), parse_failed: Cell::new(false), spans: RefCell::new(None), view: (0, 60), find_query: String::new(), find_case: false, matches: Vec::new(), lsp: RefCell::new(None), diag: RefCell::new(TabDiag::default()), hover: RefCell::new(None), completion: RefCell::new(None), signature: RefCell::new(None), actions: RefCell::new(None), bulb: Cell::new(None), disk: None, draft_rev: Cell::new(None) }
    }

    /// Recomputes the matches after the text or the query changed.
    pub(crate) fn refresh_matches(&mut self) {
        self.matches = match &self.buffer {
            Some(b) if !self.find_query.is_empty() => b.find_matches(&self.find_query, self.find_case),
            _ => Vec::new(),
        };
    }

    /// Tells the server about the latest edits (and, with `saved`, the save).
    pub(crate) fn lsp_sync(&self, saved: bool) {
        let (Some(buf), Some(l)) = (&self.buffer, &mut *self.lsp.borrow_mut()) else { return };
        let TabLsp { mgr, id, doc, cursor } = l;
        doc.sync(mgr, *id, &mut BufSource { buf, cursor });
        self.diag.borrow_mut().note_version(doc.version(), *cursor);
        if saved {
            doc.save(mgr, *id);
        }
    }

    /// Picks up what the servers last published for this file and moves it
    /// along with the edits made since the server saw the text.
    pub(crate) fn refresh_diags(&self, mgr: &Manager) {
        let (Some(buf), Some(path)) = (&self.buffer, &self.path) else { return };
        buf.set_track_edits(true);
        let d = &mut *self.diag.borrow_mut();
        if !d.started {
            d.started = true;
            d.saved_cursor = buf.edit_cursor();
        }
        let generation = mgr.diag_gen();
        if d.seen != generation {
            d.seen = generation;
            let found = mgr.diagnostics(&path_key(path)).unwrap_or_default();
            // Diagnostics for an older version start from the edit-log position of that version.
            let base = found.version.and_then(|v| d.versions.iter().find(|(x, _)| *x == v).map(|&(_, c)| c));
            let (saved, live): (Vec<_>, Vec<_>) = found.diags.into_iter().partition(|x| x.saved_text);
            d.live = DiagGroup { list: live, cursor: base.unwrap_or_else(|| buf.edit_cursor()) };
            d.saved = DiagGroup { list: saved, cursor: d.saved_cursor };
        }
        d.live.follow(buf);
        d.saved.follow(buf);
    }

    /// Forgets the syntax tree and coloured spans, so the next draw starts from the current grammar.
    pub(crate) fn reset_highlighting(&mut self) {
        *self.parsed.borrow_mut() = None;
        *self.spans.borrow_mut() = None;
        self.parse_failed.set(false);
    }

    pub(crate) fn file(path: PathBuf, languages: &Languages) -> Self {
        let title = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let mut tab = Self::panel(title, "file".into(), path.display().to_string());
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        tab.lang = languages.for_path(&path);
        tab.syntax = Syntax::for_extension(ext);
        tab.disk = disk_stamp(&path);
        let small = fs::metadata(&path).is_ok_and(|m| m.len() <= MAX_FILE_BYTES);
        if let Some(text) = small.then(|| fs::read_to_string(&path).ok()).flatten() {
            tab.buffer = Some(Buffer::new(&text));
        }
        tab.path = Some(path);
        tab
    }
}
