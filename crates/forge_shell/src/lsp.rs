use super::*;

/// A file tab's connection to its language server. Dropping it (when the tab
/// closes) sends `didClose`.
pub(crate) struct TabLsp {
    pub(crate) mgr: Arc<Manager>,
    pub(crate) id: ServerId,
    pub(crate) doc: DocSync,
    /// How far into the buffer's edit log the server has been told.
    pub(crate) cursor: u64,
}

impl Drop for TabLsp {
    fn drop(&mut self) {
        self.doc.close(&self.mgr);
    }
}

/// Answers from the language servers, produced on their threads and applied
/// on the UI thread.
pub(crate) enum Reply {
    Hover { seq: u64, key: String, revision: u64, line: usize, word: (usize, usize), info: Option<lsp_client::HoverInfo> },
    Definition { seq: u64, targets: Vec<lsp_client::Target> },
    References { seq: u64, targets: Vec<lsp_client::Target> },
    Symbols { seq: u64, symbols: Vec<lsp_client::Symbol> },
    Rename { seq: u64, result: Result<Vec<lsp_client::FileEdit>, String> },
    Completion { seq: u64, key: String, line: usize, anchor_col: usize, result: Result<lsp_client::CompletionList, String> },
    Signature { seq: u64, key: String, line: usize, col: usize, help: Option<lsp_client::SignatureHelp> },
    Format { seq: u64, key: String, revision: u64, then_save: bool, result: Result<Vec<ContentChange>, String> },
    CodeActions { seq: u64, key: String, line: usize, col: usize, bulb: bool, result: Result<Vec<lsp_client::CodeAction>, String> },
}

pub(crate) const COMPLETION_ROWS: usize = 8;

/// An open completion list for a tab: what the server offered and what is
/// left after filtering by the word typed since `anchor_col`.
pub(crate) struct CompletionSession {
    pub(crate) line: usize,
    pub(crate) anchor_col: usize,
    pub(crate) items: Vec<lsp_client::CompletionItem>,
    /// False while the first answer is still on its way.
    pub(crate) ready: bool,
    pub(crate) incomplete: bool,
    pub(crate) filtered: Vec<usize>,
    pub(crate) selected: usize,
    pub(crate) top: usize,
}

impl CompletionSession {
    pub(crate) fn refilter(&mut self, query: &str) {
        self.filtered = lsp_client::filter_and_rank(&self.items, query);
        self.selected = 0;
        self.top = 0;
    }

    pub(crate) fn select(&mut self, delta: isize) {
        let n = self.filtered.len() as isize;
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + COMPLETION_ROWS {
            self.top = self.selected + 1 - COMPLETION_ROWS;
        }
    }
}

/// The code actions menu of a tab, hanging from the caret cell it was opened at.
pub(crate) struct ActionMenu {
    pub(crate) line: usize,
    pub(crate) col: usize,
    pub(crate) actions: Vec<lsp_client::CodeAction>,
    pub(crate) selected: usize,
}

pub(crate) const ACTION_ROWS: usize = 10;

/// The signature tooltip of a tab and the cell (the caret when it was asked for) it hangs from.
pub(crate) struct SignatureState {
    pub(crate) line: usize,
    pub(crate) col: usize,
    pub(crate) help: lsp_client::SignatureHelp,
}

/// One place a symbol is used, with the text of its line.
#[derive(Clone)]
pub(crate) struct RefItem {
    pub(crate) path: PathBuf,
    pub(crate) line: usize,
    pub(crate) col: usize,
    pub(crate) preview: String,
}

/// The identifier at or just before column `col`, or "".
pub(crate) fn word_at(line: &str, col: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let at = match (chars.get(col).copied().is_some_and(is_word), col.checked_sub(1).and_then(|c| chars.get(c)).copied().is_some_and(is_word)) {
        (true, _) => col,
        (false, true) => col - 1,
        _ => return String::new(),
    };
    let (start, end) = word_bounds(line, at);
    chars[start..end].iter().collect()
}

/// The hover text showing for a tab, valid for one revision of its text.
pub(crate) struct HoverState {
    pub(crate) revision: u64,
    pub(crate) line: usize,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) text: String,
}

/// The identifier around column `col`, or just that cell if there is none.
pub(crate) fn word_bounds(line: &str, col: usize) -> (usize, usize) {
    let chars: Vec<char> = line.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    if !chars.get(col).copied().is_some_and(is_word) {
        return (col, col + 1);
    }
    let start = (0..col).rev().take_while(|&i| is_word(chars[i])).last().unwrap_or(col);
    let end = (col..chars.len()).take_while(|&i| is_word(chars[i])).last().map_or(col, |i| i + 1);
    (start, end)
}

/// A tab's diagnostics, kept in the server's coordinates and moved along with
/// the edits made since the server computed them.
#[derive(Default)]
pub(crate) struct DiagGroup {
    pub(crate) list: Vec<Diag>,
    /// How far into the buffer's edit log `list` has been shifted.
    pub(crate) cursor: u64,
}

impl DiagGroup {
    /// Moves the diagnostics along with the edits made since `cursor`.
    pub(crate) fn follow(&mut self, buf: &Buffer) {
        if self.cursor == buf.edit_cursor() {
            return;
        }
        match buf.edits_since(self.cursor) {
            Some(edits) => {
                for e in edits {
                    let change = ContentChange { start: e.lsp_start, end: e.lsp_end, text: e.text };
                    self.list.retain_mut(|diag| diag.apply(&change));
                }
            }
            None => self.list.clear(),
        }
        self.cursor = buf.edit_cursor();
    }
}

#[derive(Default)]
pub(crate) struct TabDiag {
    /// Diagnostics of the live text: valid for the document version they carry.
    pub(crate) live: DiagGroup,
    /// Diagnostics of the last saved text (`cargo check`): valid for the edit-log position of the save.
    pub(crate) saved: DiagGroup,
    pub(crate) started: bool,
    /// Edit-log position when the file was opened or last saved.
    pub(crate) saved_cursor: u64,
    /// The manager's `diag_gen` the lists were fetched at.
    pub(crate) seen: u64,
    /// (document version sent, edit-log position at that moment), oldest first.
    pub(crate) versions: VecDeque<(i64, u64)>,
}

impl TabDiag {
    pub(crate) fn all(&self) -> impl Iterator<Item = &Diag> {
        self.live.list.iter().chain(&self.saved.list)
    }

    pub(crate) fn note_version(&mut self, version: i64, cursor: u64) {
        match self.versions.back() {
            Some(&(last, _)) if last == version => return,
            // The document was re-opened (server restart): old versions mean nothing.
            Some(&(last, _)) if version < last => self.versions.clear(),
            _ => {}
        }
        self.versions.push_back((version, cursor));
        if self.versions.len() > 64 {
            self.versions.pop_front();
        }
    }
}

/// Feeds a buffer's text and edit log to `DocSync`.
pub(crate) struct BufSource<'a> {
    pub(crate) buf: &'a Buffer,
    pub(crate) cursor: &'a mut u64,
}

impl DocSource for BufSource<'_> {
    fn full_text(&mut self) -> String {
        *self.cursor = self.buf.edit_cursor();
        self.buf.text()
    }

    fn take_changes(&mut self) -> Option<Vec<ContentChange>> {
        let edits = self.buf.edits_since(*self.cursor)?;
        *self.cursor = self.buf.edit_cursor();
        Some(edits.into_iter().map(|e| ContentChange { start: e.lsp_start, end: e.lsp_end, text: e.text }).collect())
    }
}

impl Shell {
    /// What a position-based request needs for the tab shown in pane `g`:
    /// the connection, the document URI and the position in LSP terms.
    pub(crate) fn lsp_request_target(&self, g: usize, line: usize, col: usize) -> Option<(lsp_client::Client, String, (usize, usize), &Tab)> {
        let mgr = self.lsp.as_ref()?;
        let tab = self.active[g].and_then(|a| self.groups[g].get(a))?;
        // Edits typed a moment ago must reach the server before the question does.
        tab.lsp_sync(false);
        let (id, uri) = {
            let l = tab.lsp.borrow();
            let l = l.as_ref()?;
            (l.id, l.doc.uri().to_string())
        };
        let (client, _) = mgr.ready(id)?;
        let buf = tab.buffer.as_ref()?;
        let line = line.min(buf.len_lines() - 1);
        Some((client, uri, (line, char_to_utf16_col(&buf.line(line), col)), tab))
    }

    /// The char column under cell `vcol` of a line in pane `g`.
    pub(crate) fn char_col_at(&self, g: usize, line: usize, vcol: usize) -> usize {
        let buf = self.active[g].and_then(|a| self.groups[g].get(a)).and_then(|t| t.buffer.as_ref());
        buf.map_or(vcol, |b| char_col(&b.line(line.min(b.len_lines() - 1)), vcol))
    }

    pub(crate) fn request_hover(&mut self, g: usize, line: usize, vcol: usize) {
        let col = self.char_col_at(g, line, vcol);
        self.group = g;
        let Some(mgr) = self.lsp.clone() else { return };
        let Some((client, uri, pos, tab)) = self.lsp_request_target(g, line, col) else { return };
        let Some(buf) = &tab.buffer else { return };
        // A diagnostic under the mouse shows its own message.
        if !line_diags(buf, line, &tab.diag.borrow()).iter().all(|d| !((d.start as usize) <= col && col < d.end as usize)) {
            return;
        }
        let (key, revision, word) = (tab.key.clone(), buf.revision(), word_bounds(&buf.line(line), col));
        self.hover_seq += 1;
        let (seq, replies, wake) = (self.hover_seq, self.replies.clone(), mgr.waker());
        mgr.handle().spawn(async move {
            let info = lsp_client::hover(&client, &uri, pos).await.ok().flatten();
            replies.lock().unwrap().push_back(Reply::Hover { seq, key, revision, line: pos.0, word, info });
            wake();
        });
    }

    pub(crate) fn request_definition(&mut self, g: usize, line: usize, vcol: usize) {
        let col = self.char_col_at(g, line, vcol);
        self.group = g;
        let Some(mgr) = self.lsp.clone() else { return };
        let Some((client, uri, pos, _)) = self.lsp_request_target(g, line, col) else { return };
        self.definition_seq += 1;
        let (seq, replies, wake) = (self.definition_seq, self.replies.clone(), mgr.waker());
        mgr.handle().spawn(async move {
            let targets = lsp_client::definition(&client, &uri, pos).await.unwrap_or_default();
            replies.lock().unwrap().push_back(Reply::Definition { seq, targets });
            wake();
        });
    }

    /// Called after the text or caret of pane `g` changed: keeps an open
    /// completion list in step with the word being typed, or opens one.
    pub(crate) fn completion_after_edit(&mut self, g: usize, typed: Option<char>) {
        enum Status {
            None,
            Drop,
            Keep,
            Refetch(usize),
        }
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let (line, col, anchor, trigger) = {
            let Some(tab) = self.active[g].and_then(|a| self.groups[g].get(a)) else { return };
            let Some(buf) = &tab.buffer else { return };
            let (line, col) = buf.caret();
            let chars: Vec<char> = buf.line(line).chars().collect();
            let col = col.min(chars.len());
            let mut start = col;
            while start > 0 && is_word(chars[start - 1]) {
                start -= 1;
            }
            let status = {
                let mut slot = tab.completion.borrow_mut();
                match slot.as_mut() {
                    None => Status::None,
                    Some(s) if s.line != line || col < s.anchor_col || !chars[s.anchor_col.min(col)..col].iter().all(|&c| is_word(c)) => Status::Drop,
                    Some(s) if !s.ready => Status::Keep,
                    Some(s) => {
                        let query: String = chars[s.anchor_col..col].iter().collect();
                        s.refilter(&query);
                        if s.filtered.is_empty() {
                            Status::Drop
                        } else if s.incomplete && typed.is_some() {
                            Status::Refetch(s.anchor_col)
                        } else {
                            Status::Keep
                        }
                    }
                }
            };
            if matches!(status, Status::Drop) {
                *tab.completion.borrow_mut() = None;
            }
            let trigger = match status {
                Status::Refetch(anchor) => Some((anchor, lsp_client::Trigger::Invoked)),
                Status::Keep => None,
                Status::None | Status::Drop => match typed {
                    Some(c) if is_word(c) && start < col => Some((start, lsp_client::Trigger::Invoked)),
                    Some('.') => Some((col, lsp_client::Trigger::Character('.'))),
                    Some(':') if col >= 2 && chars[col - 2] == ':' => Some((col, lsp_client::Trigger::Character(':'))),
                    _ => None,
                },
            };
            let anchor = trigger.as_ref().map_or(0, |t| t.0);
            (line, col, anchor, trigger.map(|t| t.1))
        };
        if let Some(trigger) = trigger {
            self.request_completion(g, line, col, anchor, trigger);
        }
    }

    pub(crate) fn request_completion(&mut self, g: usize, line: usize, col: usize, anchor_col: usize, trigger: lsp_client::Trigger) {
        let Some(mgr) = self.lsp.clone() else { return };
        let Some((client, uri, pos, tab)) = self.lsp_request_target(g, line, col) else { return };
        {
            let mut slot = tab.completion.borrow_mut();
            // A refetch keeps the visible list until the new one arrives.
            if !slot.as_ref().is_some_and(|s| s.line == line && s.anchor_col == anchor_col) {
                *slot = Some(CompletionSession { line, anchor_col, items: Vec::new(), ready: false, incomplete: false, filtered: Vec::new(), selected: 0, top: 0 });
            }
        }
        let key = tab.key.clone();
        self.completion_seq += 1;
        let (seq, replies, wake) = (self.completion_seq, self.replies.clone(), mgr.waker());
        mgr.handle().spawn(async move {
            let result = lsp_client::completion(&client, &uri, pos, trigger).await.map_err(|e| e.to_string());
            replies.lock().unwrap().push_back(Reply::Completion { seq, key, line, anchor_col, result });
            wake();
        });
    }

    /// Keeps the signature tooltip in step with the call being typed: opens it
    /// on `(` and `,`, refreshes it while it is open, closes it on `)`.
    pub(crate) fn signature_after_edit(&mut self, g: usize, typed: Option<char>) {
        let Some(tab) = self.active[g].and_then(|a| self.groups[g].get(a)) else { return };
        let Some(buf) = &tab.buffer else { return };
        let open = tab.signature.borrow().is_some();
        let (line, col) = buf.caret();
        let trigger = match typed {
            Some(')') => {
                *tab.signature.borrow_mut() = None;
                self.signature_seq += 1;
                return;
            }
            Some(c @ ('(' | ',')) => lsp_client::Trigger::Character(c),
            _ if open => lsp_client::Trigger::Invoked,
            _ => return,
        };
        self.request_signature(g, line, col, trigger);
    }

    pub(crate) fn request_signature(&mut self, g: usize, line: usize, col: usize, trigger: lsp_client::Trigger) {
        let Some(mgr) = self.lsp.clone() else { return };
        let Some((client, uri, pos, tab)) = self.lsp_request_target(g, line, col) else { return };
        let key = tab.key.clone();
        self.signature_seq += 1;
        let (seq, replies, wake) = (self.signature_seq, self.replies.clone(), mgr.waker());
        mgr.handle().spawn(async move {
            let help = lsp_client::signature_help(&client, &uri, pos, trigger).await.ok().flatten();
            replies.lock().unwrap().push_back(Reply::Signature { seq, key, line, col, help });
            wake();
        });
    }

    /// Asks the server to format the file in pane `g`; with `then_save` the
    /// file is saved when the answer arrives. Returns false if nothing was sent.
    pub(crate) fn request_format(&mut self, g: usize, then_save: bool) -> bool {
        let Some(mgr) = self.lsp.clone() else { return false };
        let Some((client, uri, _, tab)) = self.lsp_request_target(g, 0, 0) else { return false };
        let Some(revision) = tab.buffer.as_ref().map(|b| b.revision()) else { return false };
        let key = tab.key.clone();
        self.format_seq += 1;
        let (seq, replies, wake) = (self.format_seq, self.replies.clone(), mgr.waker());
        mgr.handle().spawn(async move {
            let result = lsp_client::format(&client, &uri).await.map_err(|e| match e {
                lsp_client::Error::Server { message, .. } => message,
                other => other.to_string(),
            });
            replies.lock().unwrap().push_back(Reply::Format { seq, key, revision, then_save, result });
            wake();
        });
        true
    }

    /// Asks for the code actions at the caret or selection of pane `g`: for the
    /// menu, or (`bulb`) just to know whether the lightbulb should show.
    pub(crate) fn request_code_actions(&mut self, g: usize, bulb: bool) {
        let Some(mgr) = self.lsp.clone() else { return };
        let Some(tab) = self.active[g].and_then(|a| self.groups[g].get(a)) else { return };
        let Some(buf) = &tab.buffer else { return };
        let (start, end) = if bulb { (buf.caret(), buf.caret()) } else { buf.selection_bounds() };
        let (line, col) = buf.caret();
        let to16 = |(l, c): (usize, usize)| (l, char_to_utf16_col(&buf.line(l), c));
        let range = (to16(start), to16(end));
        // The diagnostics under the range, as the server knows them.
        let diagnostics: Vec<_> = tab.diag.borrow().all().filter(|d| d.start <= range.1 && d.end >= range.0).map(|d| d.context_json()).collect();
        let Some((client, uri, _, tab)) = self.lsp_request_target(g, line, col) else { return };
        let key = tab.key.clone();
        let seq = if bulb {
            self.bulb_seq += 1;
            self.bulb_seq
        } else {
            self.actions_seq += 1;
            self.actions_seq
        };
        let (replies, wake) = (self.replies.clone(), mgr.waker());
        mgr.handle().spawn(async move {
            let result = lsp_client::code_actions(&client, &uri, range, diagnostics).await.map_err(|e| match e {
                lsp_client::Error::Server { message, .. } => message,
                other => other.to_string(),
            });
            replies.lock().unwrap().push_back(Reply::CodeActions { seq, key, line, col, bulb, result });
            wake();
        });
    }

    pub(crate) fn close_actions(&mut self, g: usize) {
        if let Some(tab) = self.active[g].and_then(|a| self.groups[g].get(a)) {
            *tab.actions.borrow_mut() = None;
        }
    }

    /// Applies the chosen code action of pane `g` and closes the menu.
    pub(crate) fn accept_action(&mut self, g: usize) {
        let edits = {
            let Some(tab) = self.active[g].and_then(|a| self.groups[g].get(a)) else { return };
            let mut slot = tab.actions.borrow_mut();
            let Some(menu) = slot.take() else { return };
            tab.bulb.set(None);
            match menu.actions.get(menu.selected) {
                Some(action) => action.edits.clone(),
                None => return,
            }
        };
        let (files, count, problems) = self.apply_file_edits(edits);
        if files > 1 {
            self.notify(1, "Code action applied", format!("{count} change(s) in {files} file(s). Open files are modified but not saved."));
        }
        for p in problems {
            self.notify(3, "Couldn't update a file", p);
        }
    }

    /// Saves the file in pane `g` (and tells its server).
    pub(crate) fn save_active(&mut self, g: usize) {
        let error = match self.active[g].and_then(|a| self.groups[g].get_mut(a)) {
            Some(tab) => match save_tab(tab) {
                Ok(true) => {
                    tab.lsp_sync(true);
                    None
                }
                Ok(false) => None,
                Err(m) => Some(m),
            },
            None => None,
        };
        if let Some(m) = error {
            self.notify(3, "Couldn't save", m);
        }
    }

    /// Up / down / accept / close / Ctrl+Space for the list of pane `g`.
    pub(crate) fn completion_command(&mut self, g: usize, command: &str) {
        match command {
            "completion-up" | "completion-down" => {
                if let Some(tab) = self.active[g].and_then(|a| self.groups[g].get(a))
                    && let Some(s) = tab.completion.borrow_mut().as_mut() {
                        s.select(if command == "completion-up" { -1 } else { 1 });
                    }
            }
            "completion-close" => {
                if let Some(tab) = self.active[g].and_then(|a| self.groups[g].get(a)) {
                    *tab.completion.borrow_mut() = None;
                }
            }
            "completion-accept" => {
                if self.accept_completion(g)
                    && let Some((line, col)) = self.active[g].and_then(|a| self.groups[g].get(a)).and_then(|t| t.buffer.as_ref()).map(|b| b.caret()) {
                        self.request_signature(g, line, col, lsp_client::Trigger::Character('('));
                    }
            }
            "code-actions" => self.request_code_actions(g, false),
            "actions-up" | "actions-down" => {
                if let Some(tab) = self.active[g].and_then(|a| self.groups[g].get(a))
                    && let Some(menu) = tab.actions.borrow_mut().as_mut() {
                        let n = menu.actions.len().max(1) as isize;
                        let delta = if command == "actions-up" { -1 } else { 1 };
                        menu.selected = (menu.selected as isize + delta).rem_euclid(n) as usize;
                    }
            }
            "actions-close" => self.close_actions(g),
            "actions-accept" => self.accept_action(g),
            "signature-close" => {
                if let Some(tab) = self.active[g].and_then(|a| self.groups[g].get(a)) {
                    *tab.signature.borrow_mut() = None;
                }
                self.signature_seq += 1;
            }
            "completion-trigger" => {
                let Some(buf) = self.active[g].and_then(|a| self.groups[g].get(a)).and_then(|t| t.buffer.as_ref()) else { return };
                let (line, col) = buf.caret();
                let chars: Vec<char> = buf.line(line).chars().collect();
                let mut start = col.min(chars.len());
                while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
                    start -= 1;
                }
                self.request_completion(g, line, col, start, lsp_client::Trigger::Invoked);
            }
            _ => {}
        }
    }

    /// Inserts the selected suggestion (and any extra edits it brings) as one undo step.
    /// Returns whether the insertion opened a call (so signature help should follow).
    pub(crate) fn accept_completion(&mut self, g: usize) -> bool {
        let Some(tab) = self.active[g].and_then(|a| self.groups[g].get_mut(a)) else { return false };
        let Some(buf) = tab.buffer.as_mut() else { return false };
        let (line, col) = buf.caret();
        let (item, anchor_col) = {
            let slot = tab.completion.borrow();
            let Some(s) = slot.as_ref().filter(|s| s.ready) else { return false };
            let Some(&i) = s.filtered.get(s.selected) else { return false };
            (s.items[i].clone(), s.anchor_col)
        };
        let text = buf.line(line);
        // From the start the server named (or where the word began) up to the caret,
        // which has moved on since the request if more was typed.
        let start_col = match item.range {
            Some(((l0, c0), (l1, _))) if l0 == line && l1 == line => utf16_to_char_col(&text, c0).min(col),
            _ => anchor_col,
        };
        let mut edits = vec![ContentChange { start: (line, char_to_utf16_col(&text, start_col)), end: (line, char_to_utf16_col(&text, col)), text: item.text.clone() }];
        edits.extend(item.extra.iter().cloned());
        let new = lsp_client::apply_changes(&buf.text(), &edits);
        buf.replace_text(&new);
        // Lines inserted above (a `use` line) push the caret down.
        let shift: isize = item.extra.iter().filter(|e| e.start.0 <= line).map(|e| e.text.matches('\n').count() as isize - (e.end.0 - e.start.0) as isize).sum();
        // (line, column) of an offset into the inserted text.
        let at = |offset: usize| {
            let before: String = item.text.chars().take(offset).collect();
            match before.rfind('\n') {
                Some(i) => ((line as isize + shift) as usize + before.matches('\n').count(), before[i + 1..].chars().count()),
                None => ((line as isize + shift) as usize, start_col + before.chars().count()),
            }
        };
        let (l, c) = at(item.cursor);
        buf.set_caret(l, c, false);
        if item.select_end > item.cursor {
            let (l, c) = at(item.select_end);
            buf.set_caret(l, c, true);
        }
        *tab.completion.borrow_mut() = None;
        tab.refresh_matches();
        item.text.contains('(')
    }

    pub(crate) fn request_references(&mut self, g: usize, line: usize, col: usize) {
        self.group = g;
        let Some(mgr) = self.lsp.clone() else { return };
        let Some((client, uri, pos, _)) = self.lsp_request_target(g, line, col) else { return };
        self.references_seq += 1;
        let (seq, replies, wake) = (self.references_seq, self.replies.clone(), mgr.waker());
        mgr.handle().spawn(async move {
            let targets = lsp_client::references(&client, &uri, pos).await.unwrap_or_default();
            replies.lock().unwrap().push_back(Reply::References { seq, targets });
            wake();
        });
    }

    /// Renames the symbol at the caret of pane `g`.
    /// Asks for the outline of pane `g`'s file when it differs from the one on show.
    /// Returns whether a request went out.
    pub(crate) fn refresh_outline(&mut self, g: usize) -> bool {
        let Some(mgr) = self.lsp.clone() else { return false };
        let Some((client, uri, _, tab)) = self.lsp_request_target(g, 0, 0) else { return false };
        let (Some(path), Some(buf)) = (tab.path.clone(), tab.buffer.as_ref()) else { return false };
        let wanted = (tab.key.clone(), buf.revision());
        if self.outline_for.as_ref() == Some(&wanted) {
            return false;
        }
        self.outline_for = Some(wanted);
        self.outline_path = Some(path);
        self.outline_seq += 1;
        let (seq, replies, wake) = (self.outline_seq, self.replies.clone(), mgr.waker());
        mgr.handle().spawn(async move {
            let symbols = lsp_client::document_symbols(&client, &uri).await.unwrap_or_default();
            replies.lock().unwrap().push_back(Reply::Symbols { seq, symbols });
            wake();
        });
        true
    }

    pub(crate) fn request_rename(&mut self, g: usize, new_name: String) {
        self.group = g;
        let Some(mgr) = self.lsp.clone() else { return };
        let Some((line, col)) = self.active[g].and_then(|a| self.groups[g].get(a)).and_then(|t| t.buffer.as_ref()).map(|b| b.caret()) else { return };
        let Some((client, uri, pos, _)) = self.lsp_request_target(g, line, col) else { return };
        self.rename_seq += 1;
        let (seq, replies, wake) = (self.rename_seq, self.replies.clone(), mgr.waker());
        mgr.handle().spawn(async move {
            let result = lsp_client::rename(&client, &uri, pos, &new_name).await.map_err(|e| match e {
                lsp_client::Error::Server { message, .. } => message,
                other => other.to_string(),
            });
            replies.lock().unwrap().push_back(Reply::Rename { seq, result });
            wake();
        });
    }

    /// The lines of `path` for previews: the open tab's text, else the file.
    pub(crate) fn text_of(&self, path: &Path) -> Option<String> {
        let key = path_key(path);
        let open = self.groups.iter().flatten().find(|t| t.path.as_deref().is_some_and(|p| path_key(p) == key));
        match open.and_then(|t| t.buffer.as_ref()) {
            Some(buf) => Some(buf.text()),
            None => fs::metadata(path).ok().filter(|m| m.len() <= MAX_FILE_BYTES).and_then(|_| fs::read_to_string(path).ok()),
        }
    }

    /// Applies a rename's edits: open files change in their buffers (one undo
    /// step each, left unsaved), the rest are rewritten on disk. Returns
    /// (files, edits, problems).
    pub(crate) fn apply_file_edits(&mut self, edits: Vec<lsp_client::FileEdit>) -> (usize, usize, Vec<String>) {
        let (mut files, mut count, mut problems) = (0, 0, Vec::new());
        for file in edits {
            files += 1;
            count += file.edits.len();
            let key = path_key(&file.path);
            let mut in_buffer = false;
            for tab in self.groups.iter_mut().flatten() {
                if tab.path.as_deref().is_none_or(|p| path_key(p) != key) {
                    continue;
                }
                if let Some(buf) = tab.buffer.as_mut() {
                    let new = lsp_client::apply_changes(&buf.text(), &file.edits);
                    buf.replace_text(&new);
                    in_buffer = true;
                }
                tab.refresh_matches();
            }
            if in_buffer {
                continue;
            }
            match fs::read_to_string(&file.path) {
                Ok(text) => {
                    let crlf = text.contains("\r\n");
                    let new = lsp_client::apply_changes(&text.replace("\r\n", "\n"), &file.edits);
                    let out = if crlf { new.replace('\n', "\r\n") } else { new };
                    if let Err(e) = fs::write(&file.path, out) {
                        problems.push(format!("{}: {e}", file.path.display()));
                    }
                }
                Err(e) => problems.push(format!("{}: {e}", file.path.display())),
            }
        }
        (files, count, problems)
    }

    /// Applies the answers that arrived since the last call. Returns whether the view changed.
    pub(crate) fn apply_replies(&mut self) -> bool {
        let replies: Vec<Reply> = self.replies.lock().unwrap().drain(..).collect();
        let mut changed = false;
        for reply in replies {
            match reply {
                Reply::Hover { seq, key, revision, line, word, info } if seq == self.hover_seq => {
                    let Some(tab) = self.groups.iter().flatten().find(|t| t.key == key) else { continue };
                    let Some(buf) = tab.buffer.as_ref().filter(|b| b.revision() == revision) else { continue };
                    *tab.hover.borrow_mut() = info.map(|info| {
                        let text = buf.line(line);
                        // The server's range if it stays on the line, else the word under the mouse.
                        let (start, end) = match info.range {
                            Some(((l0, c0), (l1, c1))) if l0 == line && l1 == line => (utf16_to_char_col(&text, c0), utf16_to_char_col(&text, c1)),
                            _ => word,
                        };
                        HoverState { revision, line, start, end: end.max(start + 1), text: info.text }
                    });
                    changed = true;
                }
                Reply::Definition { seq, targets } if seq == self.definition_seq => {
                    if let Some(t) = targets.into_iter().next() {
                        self.goto(t.path, t.line, t.col);
                        changed = true;
                    }
                }
                Reply::Symbols { seq, symbols } if seq == self.outline_seq => {
                    self.outline = symbols;
                    changed = true;
                }
                Reply::References { seq, targets } if seq == self.references_seq => {
                    let mut cache: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
                    let mut items = Vec::new();
                    for t in targets {
                        let lines = cache
                            .entry(path_key(&t.path))
                            .or_insert_with(|| self.text_of(&t.path).map(|s| s.lines().map(str::to_owned).collect()).unwrap_or_default());
                        let preview: String = lines.get(t.line).map(|l| l.trim().chars().take(140).collect()).unwrap_or_default();
                        items.push(RefItem { path: t.path, line: t.line, col: t.col, preview });
                    }
                    items.sort_by_key(|a| (path_key(&a.path), a.line, a.col));
                    self.references = items;
                    self.show_references = true;
                    changed = true;
                }
                Reply::Completion { seq, key, line, anchor_col, result } if seq == self.completion_seq => {
                    let Some(tab) = self.groups.iter().flatten().find(|t| t.key == key) else { continue };
                    let Some(buf) = &tab.buffer else { continue };
                    let (caret_line, col) = buf.caret();
                    let chars: Vec<char> = buf.line(line).chars().collect();
                    let mut slot = tab.completion.borrow_mut();
                    let valid = caret_line == line
                        && col >= anchor_col
                        && col <= chars.len()
                        && chars[anchor_col..col].iter().all(|&c| c.is_alphanumeric() || c == '_')
                        && slot.as_ref().is_some_and(|s| s.line == line && s.anchor_col == anchor_col);
                    match (valid, result) {
                        (true, Ok(list)) => {
                            let s = slot.as_mut().unwrap();
                            s.items = list.items;
                            s.incomplete = list.incomplete;
                            s.ready = true;
                            let query: String = chars[anchor_col..col].iter().collect();
                            s.refilter(&query);
                            if s.filtered.is_empty() {
                                *slot = None;
                            }
                        }
                        (true, Err(_)) => *slot = None,
                        (false, _) => {}
                    }
                    changed = true;
                }
                Reply::Signature { seq, key, line, col, help } if seq == self.signature_seq => {
                    let Some(tab) = self.groups.iter().flatten().find(|t| t.key == key) else { continue };
                    let on_line = tab.buffer.as_ref().is_some_and(|b| b.caret().0 == line);
                    *tab.signature.borrow_mut() = match help {
                        // A bare `(...)` is a tuple or a parenthesised expression, not a call.
                        Some(help) if on_line && !help.label.trim_start().starts_with('(') => Some(SignatureState { line, col, help }),
                        _ => None,
                    };
                    changed = true;
                }
                Reply::CodeActions { seq, key, line, col, bulb, result } if seq == if bulb { self.bulb_seq } else { self.actions_seq } => {
                    let mut problem = None;
                    if let Some(tab) = self.groups.iter().flatten().find(|t| t.key == key) {
                        let on_line = tab.buffer.as_ref().is_some_and(|b| b.caret().0 == line);
                        match (result, bulb) {
                            (Ok(actions), true) => tab.bulb.set((on_line && !actions.is_empty()).then_some(line)),
                            (Ok(actions), false) if on_line => *tab.actions.borrow_mut() = Some(ActionMenu { line, col, actions, selected: 0 }),
                            (Err(m), false) => problem = Some(m),
                            _ => {}
                        }
                    }
                    if let Some(m) = problem {
                        self.notify(2, "Code actions failed", m);
                    }
                    changed = true;
                }
                Reply::Format { seq, key, revision, then_save, result } if seq == self.format_seq => {
                    let (mut problem, mut save_problem) = (None, None);
                    if let Some(tab) = self.groups.iter_mut().flatten().find(|t| t.key == key) {
                        if let Some(buf) = tab.buffer.as_mut() {
                            match result {
                                Ok(edits) if buf.revision() == revision => {
                                    if !edits.is_empty() {
                                        let new = lsp_client::apply_changes(&buf.text(), &edits);
                                        buf.replace_text(&new);
                                        tab.refresh_matches();
                                    }
                                }
                                // Typed since: applying old edits would garble the text. A save still goes ahead.
                                Ok(_) if then_save => {}
                                Ok(_) => problem = Some("The text changed while formatting. Run Format again.".to_string()),
                                Err(m) => problem = Some(m),
                            }
                        }
                        if then_save {
                            match save_tab(tab) {
                                Ok(_) => tab.lsp_sync(true),
                                Err(m) => save_problem = Some(m),
                            }
                        }
                    }
                    if let Some(m) = problem {
                        self.notify(2, "Format failed", m);
                    }
                    if let Some(m) = save_problem {
                        self.notify(3, "Couldn't save", m);
                    }
                    changed = true;
                }
                Reply::Rename { seq, result } if seq == self.rename_seq => {
                    match result {
                        Ok(edits) if edits.is_empty() => self.notify(2, "Nothing to rename", "The server returned no changes.".into()),
                        Ok(edits) => {
                            let (files, count, problems) = self.apply_file_edits(edits);
                            self.notify(1, "Renamed", format!("{count} change(s) in {files} file(s). Open files are modified but not saved."));
                            for p in problems {
                                self.notify(3, "Couldn't update a file", p);
                            }
                        }
                        Err(message) => self.notify(2, "Rename failed", message),
                    }
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }

    /// Every diagnostic the servers know of, by file then position. Files shown
    /// in a pane use that tab's copy, which follows unsaved edits.
    pub(crate) fn problem_items(&self) -> Vec<(PathBuf, Diag)> {
        let Some(mgr) = &self.lsp else { return Vec::new() };
        let mut files: BTreeMap<String, (PathBuf, Vec<Diag>)> = BTreeMap::new();
        for f in mgr.all_diagnostics() {
            files.insert(path_key(&f.path), (f.path, f.diags));
        }
        for g in 0..2 {
            let Some(tab) = self.active[g].and_then(|a| self.groups[g].get(a)) else { continue };
            let Some(path) = &tab.path else { continue };
            let diag = tab.diag.borrow();
            if diag.started {
                let list: Vec<Diag> = diag.all().cloned().collect();
                let key = path_key(path);
                match files.get_mut(&key) {
                    Some(entry) => entry.1 = list,
                    None if !list.is_empty() => {
                        files.insert(key, (path.clone(), list));
                    }
                    None => {}
                }
            }
        }
        let mut out = Vec::new();
        for (_, (path, mut diags)) in files {
            diags.sort_by_key(|d| (d.start, d.severity));
            out.extend(diags.into_iter().map(|d| (path.clone(), d)));
        }
        out
    }

    /// Opens (or switches to) the tab showing a server's log.
    pub(crate) fn open_log_tab(&mut self, id: ServerId) {
        let Some(mgr) = self.lsp.clone() else { return };
        let Some(info) = mgr.servers().into_iter().find(|s| s.id == id) else { return };
        let mut tab = Tab::panel(format!("{} log", info.name), "file".into(), format!("lsp-log:{id}"));
        tab.buffer = Some(log_buffer(&mgr.logs(id)));
        self.open_tab(tab);
    }

    /// Reloads open log tabs whose server logged since. Returns whether any changed.
    pub(crate) fn refresh_log_tabs(&mut self) -> bool {
        let Some(mgr) = self.lsp.clone() else { return false };
        let mut changed = false;
        for tab in self.groups.iter_mut().flatten() {
            let Some(id) = tab.key.strip_prefix("lsp-log:").and_then(|i| i.parse().ok()) else { continue };
            let lines = mgr.logs(id);
            let up_to_date = tab.buffer.as_ref().is_some_and(|b| {
                b.len_lines() == lines.len().max(1) && b.line(b.len_lines() - 1) == lines.last().map_or("", String::as_str)
            });
            if !up_to_date {
                tab.buffer = Some(log_buffer(&lines));
                changed = true;
            }
        }
        changed
    }

    /// Connects a file tab to the server for its language, if there is one.
    /// A file shown twice (split pane) is only synced from its first tab.
    pub(crate) fn attach_lsp(&self, tab: &Tab) {
        let (Some(mgr), Some(path), Some(buf)) = (&self.lsp, &tab.path, &tab.buffer) else { return };
        if self.groups.iter().flatten().any(|t| t.key == tab.key && t.lsp.borrow().is_some()) {
            return;
        }
        let Some(id) = mgr.ensure_for_file(path) else { return };
        buf.set_track_edits(true);
        *tab.lsp.borrow_mut() = Some(TabLsp { mgr: mgr.clone(), id, doc: DocSync::new(path, language_id(path)), cursor: 0 });
    }

    /// Syncs the visible tabs with their servers.
    pub(crate) fn lsp_sync_open(&self) {
        let Some(mgr) = &self.lsp else { return };
        for g in 0..2 {
            if let Some(tab) = self.active[g].and_then(|a| self.groups[g].get(a)) {
                tab.lsp_sync(false);
                tab.refresh_diags(mgr);
            }
        }
    }
}
