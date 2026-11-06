use super::*;

pub(crate) fn read_body(tab: Option<&Tab>) -> String {
    let Some(path) = tab.filter(|t| t.buffer.is_none()).and_then(|t| t.path.as_ref()) else { return String::new() };
    match fs::metadata(path) {
        Ok(m) if m.len() > MAX_FILE_BYTES => format!("(file is larger than {} KB, not shown)", MAX_FILE_BYTES / 1024),
        Ok(_) => fs::read(path).map_or_else(
            |e| format!("(couldn't read file: {e})"),
            |bytes| {
                let text = String::from_utf8_lossy(&bytes).replace('\t', "    ");
                if text.is_empty() { "(empty file)".into() } else { text }
            },
        ),
        Err(e) => format!("(couldn't read file: {e})"),
    }
}

pub(crate) fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

pub(crate) fn tab_infos(tabs: &[Tab]) -> ModelRc<TabInfo> {
    model(
        tabs.iter()
            .map(|t| TabInfo { title: t.title.as_str().into(), icon: t.icon.as_str().into(), dirty: t.buffer.as_ref().is_some_and(Buffer::is_dirty), pinned: t.pinned })
            .collect(),
    )
}

/// The match to move to from `from`, wrapping around at either end.
pub(crate) fn pick_match(matches: &[Match], from: (usize, usize), backwards: bool) -> Option<Match> {
    if backwards {
        matches.iter().rev().find(|m| (m.line, m.end) <= from).or(matches.last()).copied()
    } else {
        matches.iter().find(|m| (m.line, m.start) >= from).or(matches.first()).copied()
    }
}

/// One find-bar action on a tab (`kind` is what `CodeView` sends).
pub(crate) fn find(tab: &mut Tab, kind: &str, text: &str, flag: bool) {
    let Some(buf) = tab.buffer.as_mut() else { return };
    let (start, end) = buf.selection_bounds();
    match kind {
        "query" => {
            tab.find_query = text.to_string();
            tab.find_case = flag;
            tab.refresh_matches();
            // Keep the current match stable while typing: first match at or after the selection start.
            if let Some(m) = pick_match(&tab.matches, start, false) {
                tab.buffer.as_mut().unwrap().select_match(m);
            }
        }
        "close" => {
            tab.find_query.clear();
            tab.matches.clear();
        }
        "next" | "prev" => {
            let prev = kind == "prev";
            if let Some(m) = pick_match(&tab.matches, if prev { start } else { end }, prev) {
                buf.select_match(m);
            }
        }
        "replace" => {
            if let Some(i) = buf.match_at_selection(&tab.matches) {
                buf.replace_match(tab.matches[i], text);
                tab.refresh_matches();
                let (_, end) = tab.buffer.as_ref().unwrap().selection_bounds();
                if let Some(m) = pick_match(&tab.matches, end, false) {
                    tab.buffer.as_mut().unwrap().select_match(m);
                }
            } else if let Some(m) = pick_match(&tab.matches, end, false) {
                buf.select_match(m);
            }
        }
        "replace-all" => {
            buf.replace_all(&tab.matches, text);
            tab.refresh_matches();
        }
        _ => {}
    }
}

pub(crate) fn editor_view(tab: &Tab, highlighter: &RefCell<Highlighter>) -> EditorView {
    let Some(buf) = &tab.buffer else {
        return EditorView { lines: ModelRc::default(), line_count: 0, max_cols: 0, caret_line: 0, caret_col: 0, match_count: 0, match_index: 0, hover_text: SharedString::new(), hover_line: 0, hover_start: 0, hover_end: 0, completion: ModelRc::default(), completion_selected: 0, completion_line: 0, completion_col: 0, completion_doc: SharedString::new(), signature_open: false, signature_before: SharedString::new(), signature_active: SharedString::new(), signature_after: SharedString::new(), signature_doc: SharedString::new(), signature_line: 0, signature_col: 0, actions: ModelRc::default(), actions_selected: 0, actions_line: 0, actions_col: 0, bulb_line: -1 };
    };
    let current = buf.match_at_selection(&tab.matches);
    let (first, count) = tab.view;
    let last = (first + count).min(buf.len_lines());
    let first = first.min(last);

    // The syntax tree follows the buffer's edits; only the visible lines are coloured,
    // and only again when the text or the visible window changed.
    if tab.parsed.borrow().is_none() && !tab.parse_failed.get() {
        match tab.lang.and_then(|lang| highlighter.borrow_mut().open(lang, buf)) {
            Some(parsed) => *tab.parsed.borrow_mut() = Some(parsed),
            None => tab.parse_failed.set(true),
        }
    }
    let revision = buf.revision();
    if let Some(parsed) = tab.parsed.borrow_mut().as_mut() {
        let fresh = tab.spans.borrow().as_ref().is_some_and(|(r, f, l, _)| (*r, *f, *l) == (revision, first, last));
        if !fresh {
            parsed.update(buf);
            *tab.spans.borrow_mut() = Some((revision, first, last, parsed.line_spans(buf, first, last - first)));
        }
    }
    let diag = tab.diag.borrow();
    let cache = tab.spans.borrow();
    let by_tree = tab.parsed.borrow().is_some().then(|| cache.as_ref().map(|(_, _, _, lines)| lines)).flatten();
    let lines = (first.min(last)..last)
        .map(|n| {
            // The view draws in cells, so every column on the line goes through `vc`.
            let text = buf.line(n);
            let vc = |c: i32| visual_col(&text, c.max(0) as usize) as i32;
            let (sel_start, sel_end) = buf.selection_on_line(n).unwrap_or((0, 0));
            let on_line: Vec<DiagView> = line_diags(buf, n, &diag).into_iter().map(|d| DiagView { start: vc(d.start), end: vc(d.end), ..d }).collect();
            LineView {
                number: n as i32,
                spans: model(
                    by_tree
                        .and_then(|lines| lines.get(n - first).cloned())
                        .unwrap_or_else(|| editor_core::spans(&text, tab.syntax))
                        .into_iter()
                        .map(|s| {
                            let col = visual_col(&text, s.col);
                            SpanView { text: expand_tabs_at(&s.text, col).into(), col: col as i32, kind: s.kind as i32 }
                        })
                        .collect(),
                ),
                sel_start: vc(sel_start as i32),
                sel_end: vc(sel_end as i32),
                finds: model(
                    tab.matches
                        .iter()
                        .enumerate()
                        .filter(|(_, m)| m.line == n)
                        .map(|(i, m)| MatchView { start: vc(m.start as i32), end: vc(m.end as i32), current: Some(i) == current })
                        .collect(),
                ),
                diags: model(on_line.clone()),
                severity: on_line.iter().map(|d| d.severity).min().unwrap_or(0),
            }
        })
        .collect();
    let (caret_line, caret_col) = buf.caret();
    // Cell column of a char column on a line.
    let cell = |line: usize, col: usize| visual_col(&buf.line(line), col) as i32;
    let hover = tab.hover.borrow();
    let hover = hover.as_ref().filter(|h| h.revision == revision);
    let completion = tab.completion.borrow();
    let completion = completion.as_ref().filter(|s| s.ready && !s.filtered.is_empty());
    let rows: Vec<CompletionRow> = completion
        .map(|s| {
            s.filtered[s.top..(s.top + COMPLETION_ROWS).min(s.filtered.len())]
                .iter()
                .map(|&i| CompletionRow { label: s.items[i].label.as_str().into(), detail: s.items[i].detail.as_str().into(), kind: s.items[i].kind as i32 })
                .collect()
        })
        .unwrap_or_default();
    let doc = completion
        .and_then(|s| s.filtered.get(s.selected).map(|&i| &s.items[i]))
        .map(|it| match (it.detail.is_empty(), it.doc.is_empty()) {
            (_, true) => String::new(),
            (true, false) => it.doc.clone(),
            (false, false) => format!("{}\n\n{}", it.detail, it.doc),
        })
        .unwrap_or_default();
    let signature = tab.signature.borrow();
    let (sig_before, sig_active, sig_after) = match signature.as_ref() {
        Some(s) => {
            let chars: Vec<char> = s.help.label.chars().collect();
            let (a, b) = s.help.active.map_or((chars.len(), chars.len()), |(a, b)| (a.min(chars.len()), b.min(chars.len()).max(a.min(chars.len()))));
            (chars[..a].iter().collect::<String>(), chars[a..b].iter().collect::<String>(), chars[b..].iter().collect::<String>())
        }
        None => Default::default(),
    };
    let sig_doc = signature
        .as_ref()
        .map(|s| if s.help.param_doc.is_empty() { s.help.doc.clone() } else { s.help.param_doc.clone() })
        .unwrap_or_default();
    let actions = tab.actions.borrow();
    let (action_rows, action_selected, action_line, action_col): (Vec<CompletionRow>, i32, i32, i32) = match actions.as_ref() {
        Some(menu) if menu.actions.is_empty() => (
            vec![CompletionRow { label: "No code actions available here".into(), detail: SharedString::new(), kind: 0 }],
            -1,
            menu.line as i32,
            cell(menu.line, menu.col),
        ),
        Some(menu) => {
            let top = if menu.selected >= ACTION_ROWS { menu.selected + 1 - ACTION_ROWS } else { 0 };
            let rows = menu.actions[top..(top + ACTION_ROWS).min(menu.actions.len())]
                .iter()
                .map(|a| CompletionRow { label: a.title.as_str().into(), detail: a.kind.as_str().into(), kind: a.kind_number() })
                .collect();
            (rows, (menu.selected - top) as i32, menu.line as i32, cell(menu.line, menu.col))
        }
        None => (Vec::new(), 0, 0, 0),
    };
    let bulb_line = tab.bulb.get().filter(|&l| l == buf.caret().0).map_or(-1, |l| l as i32);
    EditorView {
        actions: model(action_rows),
        actions_selected: action_selected,
        actions_line: action_line,
        actions_col: action_col,
        bulb_line,
        signature_open: signature.is_some(),
        signature_before: sig_before.into(),
        signature_active: sig_active.into(),
        signature_after: sig_after.into(),
        signature_doc: sig_doc.into(),
        signature_line: signature.as_ref().map_or(0, |s| s.line as i32),
        signature_col: signature.as_ref().map_or(0, |s| cell(s.line, s.col)),
        completion: model(rows),
        completion_selected: completion.map_or(0, |s| (s.selected - s.top) as i32),
        completion_line: completion.map_or(0, |s| s.line as i32),
        completion_col: completion.map_or(0, |s| cell(s.line, s.anchor_col)),
        completion_doc: doc.into(),
        hover_text: hover.map_or_else(SharedString::new, |h| h.text.as_str().into()),
        hover_line: hover.map_or(0, |h| h.line as i32),
        hover_start: hover.map_or(0, |h| cell(h.line, h.start)),
        hover_end: hover.map_or(0, |h| cell(h.line, h.end)),
        match_count: tab.matches.len() as i32,
        match_index: current.map_or(0, |i| i as i32 + 1),
        lines: model(lines),
        line_count: buf.len_lines() as i32,
        max_cols: buf.max_cols() as i32,
        caret_line: caret_line as i32,
        caret_col: cell(caret_line, caret_col),
    }
}

/// One line of a grouped list (Problems, References).
pub(crate) struct RowSource {
    pub(crate) path: PathBuf,
    pub(crate) label: String,
    pub(crate) sub: String,
    pub(crate) severity: i32,
}

/// Rows grouped under a header per file; `sources` must already be ordered by file.
pub(crate) fn grouped_rows(root: &Path, sources: &[RowSource]) -> Vec<ProblemRow> {
    let root = normalize_path(root);
    let mut rows = Vec::new();
    let mut i = 0;
    while i < sources.len() {
        let path = &sources[i].path;
        let count = sources[i..].iter().take_while(|s| &s.path == path).count();
        let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let dir = path.parent().map(normalize_path).map(|d| d.strip_prefix(&root).unwrap_or(&d).display().to_string()).unwrap_or_default();
        rows.push(ProblemRow { header: true, item: -1, label: name.into(), sub: format!("{dir}  ({count})").into(), severity: 0 });
        for (k, src) in sources[i..i + count].iter().enumerate() {
            rows.push(ProblemRow { header: false, item: (i + k) as i32, label: src.label.as_str().into(), sub: src.sub.as_str().into(), severity: src.severity });
        }
        i += count;
    }
    rows
}

/// The Problems panel rows for `items` (as `problem_items` returns them).
pub(crate) fn problem_rows(root: &Path, items: &[(PathBuf, Diag)]) -> Vec<ProblemRow> {
    let sources: Vec<RowSource> = items
        .iter()
        .map(|(path, d)| RowSource {
            path: path.clone(),
            label: d.message.lines().next().unwrap_or_default().to_string(),
            sub: format!("{}:{}", d.start.0 + 1, d.start.1 + 1),
            severity: d.severity as i32,
        })
        .collect();
    grouped_rows(root, &sources)
}

/// The References panel rows.
pub(crate) fn reference_rows(root: &Path, items: &[RefItem]) -> Vec<ProblemRow> {
    let sources: Vec<RowSource> = items
        .iter()
        .map(|r| RowSource { path: r.path.clone(), label: r.preview.clone(), sub: format!("{}:{}", r.line + 1, r.col + 1), severity: 0 })
        .collect();
    grouped_rows(root, &sources)
}

/// Moves the caret to the next (`dir > 0`) or previous problem, wrapping around.
pub(crate) fn jump_problem(buf: &mut Buffer, diag: &TabDiag, dir: i32) {
    let mut at: Vec<(usize, usize)> = diag.all().map(|d| (d.start.0, utf16_to_char_col(&buf.line(d.start.0), d.start.1))).collect();
    at.sort_unstable();
    at.dedup();
    let caret = buf.caret();
    let target = if dir > 0 {
        at.iter().find(|&&p| p > caret).or(at.first())
    } else {
        at.iter().rev().find(|&&p| p < caret).or(at.last())
    };
    if let Some(&(line, col)) = target {
        buf.set_caret(line, col, false);
    }
}

/// The diagnostics touching line `n`, as char columns on that line.
pub(crate) fn line_diags(buf: &Buffer, n: usize, diags: &TabDiag) -> Vec<DiagView> {
    // A range that ends at column 0 of a later line doesn't touch that line.
    let mut on_line = diags.all().filter(|d| d.start.0 <= n && n <= d.end.0 && !(n == d.end.0 && n > d.start.0 && d.end.1 == 0)).peekable();
    if on_line.peek().is_none() {
        return Vec::new();
    }
    let text = buf.line(n);
    on_line
        .map(|d| {
            let start = if n == d.start.0 { utf16_to_char_col(&text, d.start.1) } else { 0 };
            let mut end = if n == d.end.0 { utf16_to_char_col(&text, d.end.1) } else { text.chars().count() };
            if end <= start {
                end = start + 1;
            }
            DiagView { start: start as i32, end: end as i32, severity: d.severity as i32, message: d.text().into() }
        })
        .collect()
}

/// Pushes the whole shell state into the window.
pub(crate) fn sync(ui: &AppWindow, s: &Shell) {
    sync_tabs(ui, s);
    sync_workspace(ui, s);
}

/// Tabs, editors and the status bar: everything an edit or a scroll can change.
pub(crate) fn sync_tabs(ui: &AppWindow, s: &Shell) {
    s.lsp_sync_open();
    let idx = |a: Option<usize>| a.map_or(-1, |i| i as i32);
    let tab_of = |g: usize| s.active[g].and_then(|a| s.groups[g].get(a));

    ui.set_tabs(tab_infos(&s.groups[0]));
    ui.set_active_tab(idx(s.active[0]));
    ui.set_body(read_body(tab_of(0)).into());
    ui.set_split_tabs(tab_infos(&s.groups[1]));
    ui.set_split_active(idx(s.active[1]));
    ui.set_split_body(read_body(tab_of(1)).into());
    ui.set_active_group(s.group as i32);
    ui.set_has_editor(tab_of(0).is_some_and(|t| t.buffer.is_some()));
    let none = Tab::panel(String::new(), String::new(), String::new());
    ui.set_editor(editor_view(tab_of(0).unwrap_or(&none), &s.highlighter));
    ui.set_split_has_editor(tab_of(1).is_some_and(|t| t.buffer.is_some()));
    ui.set_split_editor(editor_view(tab_of(1).unwrap_or(&none), &s.highlighter));

    match tab_of(s.group) {
        Some(tab) => {
            ui.set_active_file(tab.title.as_str().into());
            let cursor = tab.buffer.as_ref().map(|b| b.caret()).map_or(String::new(), |(l, c)| format!("{}:{}", l + 1, c + 1));
            ui.set_cursor_text(cursor.into());
            ui.set_language(tab.path.as_deref().map_or("", Shell::language).into());
        }
        None => {
            ui.set_active_file(SharedString::new());
            ui.set_cursor_text(SharedString::new());
            ui.set_language(SharedString::new());
        }
    }
}

/// The Search panel rows: a header per file, then a row per hit with the match picked out.
pub(crate) fn search_rows(root: &Path, hits: &[search::Hit]) -> Vec<SearchRow> {
    let root = normalize_path(root);
    let mut rows = Vec::new();
    let mut i = 0;
    while i < hits.len() {
        let path = &hits[i].path;
        let count = hits[i..].iter().take_while(|h| &h.path == path).count();
        let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let dir = path.parent().map(normalize_path).map(|d| d.strip_prefix(&root).unwrap_or(&d).display().to_string()).unwrap_or_default();
        rows.push(SearchRow { header: true, item: -1, before: name.into(), matched: SharedString::new(), after: SharedString::new(), sub: format!("{dir}  ({count})").into() });
        for (k, h) in hits[i..i + count].iter().enumerate() {
            let chars: Vec<char> = h.preview.chars().collect();
            let (a, b) = (h.preview_start.min(chars.len()), h.preview_end.min(chars.len()));
            let part = |r: std::ops::Range<usize>| chars[r].iter().collect::<String>();
            rows.push(SearchRow { header: false, item: (i + k) as i32, before: part(0..a).into(), matched: part(a..b.max(a)).into(), after: part(b.max(a)..chars.len()).into(), sub: (h.line + 1).to_string().into() });
        }
        i += count;
    }
    rows
}

pub(crate) fn search_status(s: &SearchState, query: &str) -> String {
    let results = |n: usize, files: usize| format!("{n} result{} in {files} file{}", if n == 1 { "" } else { "s" }, if files == 1 { "" } else { "s" });
    if query.is_empty() {
        "Type to search the project".into()
    } else if s.searching && s.hits.is_empty() {
        "Searching…".into()
    } else if s.hits.is_empty() {
        "No results".into()
    } else if s.truncated {
        format!("{}+ results, showing the first {}", s.hits.len(), s.hits.len())
    } else if s.searching {
        format!("{}, searching…", results(s.hits.len(), s.files))
    } else {
        results(s.hits.len(), s.files)
    }
}

pub(crate) fn sync_search(ui: &AppWindow, s: &Shell) {
    ui.set_search_rows(model(search_rows(&s.root, &s.search.hits)));
    ui.set_search_status(search_status(&s.search, ui.get_search_query().as_str()).into());
}

pub(crate) fn sync_problems(ui: &AppWindow, s: &Shell) {
    ui.set_problems(model(problem_rows(&s.root, &s.problem_items())));
    ui.set_references(model(reference_rows(&s.root, &s.references)));
    ui.set_outline(model(outline_rows(&s.outline)));
}

/// The Language Servers menu, the status dot and the activity text.
pub(crate) fn sync_lsp(ui: &AppWindow, s: &Shell) {
    let Some(mgr) = &s.lsp else { return };
    let mut servers = mgr.servers();
    servers.sort_by(|a, b| (&a.root, a.id).cmp(&(&b.root, b.id)));
    let state = |st: Status| match st {
        Status::Starting => 0,
        Status::Running => 1,
        Status::Warning => 2,
        Status::Error => 3,
        Status::Stopped => 4,
        Status::NotInstalled => 5,
    };

    let mut rows = Vec::new();
    let mut height = 8.0;
    let mut last_root: Option<&Path> = None;
    for info in &servers {
        if last_root != Some(info.root.as_path()) {
            let project = info.root.file_name().map_or_else(|| info.root.display().to_string(), |n| n.to_string_lossy().into_owned());
            rows.push(LspInfo { header: true, id: 0, name: project.into(), state: 0, message: SharedString::new(), version: SharedString::new() });
            height += 22.0;
            last_root = Some(info.root.as_path());
        }
        rows.push(LspInfo {
            header: false,
            id: info.id as i32,
            name: info.name.as_str().into(),
            state: state(info.status),
            message: info.message.as_str().into(),
            version: info.version.clone().unwrap_or_default().into(),
        });
        height += 28.0;
    }
    height += if servers.is_empty() { 36.0 } else { 29.0 };
    let any = |wanted: &[i32]| servers.iter().any(|i| wanted.contains(&state(i.status)));
    let worst = match () {
        _ if servers.is_empty() => -1,
        _ if any(&[3, 5]) => 3,
        _ if any(&[2]) => 2,
        _ if any(&[0]) => 0,
        _ if any(&[1]) => 1,
        _ => 4,
    };
    let (errors, warnings) = mgr.diag_counts();
    ui.set_errors(errors as i32);
    ui.set_warnings(warnings as i32);
    ui.set_servers(model(rows));
    ui.set_lsp_state(worst);
    ui.set_lsp_base_h(height);
    ui.set_activity(servers.iter().find_map(|i| i.activity.as_ref().map(|a| format!("{}: {a}", i.name))).unwrap_or_default().into());
}

pub(crate) fn sync_workspace(ui: &AppWindow, s: &Shell) {
    sync_lsp(ui, s);
    sync_problems(ui, s);
    sync_search(ui, s);
    ui.set_project_name(s.root.file_name().map_or_else(|| s.root.display().to_string(), |n| n.to_string_lossy().into_owned()).into());
    ui.set_branch(s.branch().unwrap_or_default().into());
    ui.set_branches(model(s.branches().into_iter().map(SharedString::from).collect()));
    let filter = s.recent_filter.to_lowercase();
    ui.set_recent(model(
        s.recent_all.iter().filter(|p| p.to_lowercase().contains(&filter)).map(|p| SharedString::from(p.as_str())).collect(),
    ));
    ui.set_files(model(s.file_rows().into_iter().map(|(_, r)| r).collect()));

    ui.set_notices(model(
        s.notices
            .iter()
            .map(|n| Notice {
                kind: n.kind,
                title: n.title.as_str().into(),
                message: n.message.as_str().into(),
                time: n.time.as_str().into(),
                read: n.read,
            })
            .collect(),
    ));
    ui.set_unread_count(s.notices.iter().filter(|n| !n.read).count() as i32);
}

pub(crate) fn dock_panel(ui: &AppWindow, dock: i32) -> SharedString {
    match dock {
        0 => ui.get_left_panel(),
        1 => ui.get_right_panel(),
        _ => ui.get_bottom_panel(),
    }
}

pub(crate) fn set_dock_panel(ui: &AppWindow, dock: i32, id: SharedString) {
    match dock {
        0 => ui.set_left_panel(id),
        1 => ui.set_right_panel(id),
        _ => ui.set_bottom_panel(id),
    }
}

/// Short tag for an LSP `SymbolKind`.
fn kind_label(kind: u32) -> &'static str {
    match kind {
        2 | 3 => "mod",
        5 => "class",
        6 | 9 | 12 => "fn",
        7 | 8 => "field",
        10 => "enum",
        11 => "trait",
        13 => "let",
        14 => "const",
        22 => "variant",
        23 => "struct",
        26 => "type",
        _ => "",
    }
}

pub(crate) fn outline_rows(symbols: &[lsp_client::Symbol]) -> Vec<OutlineRow> {
    symbols
        .iter()
        .enumerate()
        .map(|(i, s)| OutlineRow { item: i as i32, depth: s.depth as i32, label: s.name.as_str().into(), kind: kind_label(s.kind).into() })
        .collect()
}

pub(crate) fn ext_rows(s: &Shell) -> Vec<ExtListRow> {
    let header = |label: &str| ExtListRow { kind: 0, id: SharedString::new(), label: label.into(), sub: SharedString::new(), state: 0, item: -1, active: false };
    let mut rows = vec![header("Installed themes")];
    rows.push(ExtListRow { kind: 1, label: "Forge Default".into(), item: -1, active: s.theme_key.is_none(), ..header("") });
    for (i, t) in s.ext_themes.iter().enumerate() {
        let look = match t.appearance {
            forge_extensions::Appearance::Dark => "Dark",
            forge_extensions::Appearance::Light => "Light",
        };
        rows.push(ExtListRow {
            kind: 1,
            label: t.name.as_str().into(),
            sub: format!("{look} · {}", t.extension_name).into(),
            item: i as i32,
            active: s.theme_key.as_deref() == Some(t.key().as_str()),
            ..header("")
        });
    }
    rows.push(header("Registry"));
    for e in &s.ext_entries {
        let state = if s.ext_busy.contains(&e.id) {
            1
        } else {
            match s.ext_installed.get(&e.id) {
                Some(v) if *v == e.version => 2,
                Some(_) => 3,
                None => 0,
            }
        };
        let sub = e.description.as_deref().map(str::trim).filter(|d| !d.is_empty()).map_or_else(|| e.provides.join(", "), |d| d.chars().take(120).collect());
        rows.push(ExtListRow { kind: 2, id: e.id.as_str().into(), label: format!("{} {}", e.name, e.version).into(), sub: sub.into(), state, ..header("") });
    }
    rows
}

pub(crate) fn sync_extensions(ui: &AppWindow, s: &Shell) {
    ui.set_ext_rows(model(ext_rows(s)));
    ui.set_ext_status(s.ext_status.as_str().into());
}
