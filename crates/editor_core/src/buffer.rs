use std::cell::{Cell, RefCell};

use ropey::Rope;

use crate::columns;

/// One change to the text, in the forms its consumers want: byte offsets plus
/// (row, byte column) points for tree-sitter's `InputEdit`, and the replaced
/// range as (line, UTF-16 column) plus the inserted text for LSP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start: (usize, usize),
    pub old_end: (usize, usize),
    pub new_end: (usize, usize),
    /// The replaced range before the edit, as (line, UTF-16 column).
    pub lsp_start: (usize, usize),
    pub lsp_end: (usize, usize),
    pub text: String,
}

/// Oldest edits are dropped past this many; a consumer that fell behind gets
/// `None` from `edits_since` and must resynchronise from the full text.
const EDIT_LOG_CAP: usize = 4096;

/// A find result on one line; columns are chars.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Match {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone)]
struct Edit {
    at: usize,
    removed: String,
    inserted: String,
    before: (usize, usize),
    after: (usize, usize),
}

/// Text plus one selection (`anchor` to `head`, the caret is `head`) and undo history.
pub struct Buffer {
    rope: Rope,
    anchor: usize,
    head: usize,
    /// Column kept while moving vertically through shorter lines.
    want_col: Option<usize>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// Undo depth at the last save; `None` when that state is unreachable.
    saved_depth: Option<usize>,
    crlf: bool,
    /// Bumped by every change to the text; caches key on it.
    revision: u64,
    /// Log of edits for incremental consumers (parser, LSP); only recorded
    /// while `track_edits`. `edits_base` is the sequence number of `edits[0]`.
    edits: RefCell<Vec<TextEdit>>,
    edits_base: Cell<u64>,
    track_edits: Cell<bool>,
    /// The next `apply` starts a new undo step even if it would extend typing.
    seal: bool,
    /// What one level of indentation is: four spaces, or a tab in files that indent with tabs.
    indent_unit: &'static str,
}

/// Tabs if more lines start with a tab than with a space.
fn detect_indent(text: &str) -> &'static str {
    let (mut tabs, mut spaces) = (0usize, 0usize);
    for line in text.lines() {
        match line.as_bytes().first() {
            Some(b'\t') => tabs += 1,
            Some(b' ') => spaces += 1,
            _ => {}
        }
    }
    if tabs > spaces { "\t" } else { "    " }
}

impl Buffer {
    /// Line endings are normalised to `\n`; `to_file_text` restores CRLF.
    pub fn new(text: &str) -> Self {
        let crlf = text.contains("\r\n");
        let text = if crlf { text.replace("\r\n", "\n") } else { text.to_string() };
        Self {
            indent_unit: detect_indent(&text),
            rope: Rope::from_str(&text),
            anchor: 0,
            head: 0,
            want_col: None,
            undo: Vec::new(),
            redo: Vec::new(),
            saved_depth: Some(0),
            crlf,
            revision: 0,
            edits: RefCell::new(Vec::new()),
            edits_base: Cell::new(0),
            track_edits: Cell::new(false),
            seal: false,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The text with LF line endings, as edited.
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn to_file_text(&self) -> String {
        let text = self.rope.to_string();
        if self.crlf { text.replace('\n', "\r\n") } else { text }
    }

    pub fn mark_saved(&mut self) {
        self.saved_depth = Some(self.undo.len());
    }

    pub fn is_dirty(&self) -> bool {
        self.saved_depth != Some(self.undo.len())
    }

    // ---- reading ---------------------------------------------------------

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn line(&self, i: usize) -> String {
        if i >= self.len_lines() {
            return String::new();
        }
        let mut s = self.rope.line(i).to_string();
        while s.ends_with('\n') || s.ends_with('\r') {
            s.pop();
        }
        s
    }

    pub fn line_len(&self, i: usize) -> usize {
        self.line(i).chars().count()
    }

    pub fn max_cols(&self) -> usize {
        // Counts the newline too, which is only a column of slack. A tab is at most TAB_WIDTH cells.
        self.rope
            .lines()
            .map(|l| {
                let tabs = l.chars().filter(|&c| c == '\t').count();
                if tabs == 0 { l.len_chars() } else { columns::width(&l.to_string()) + 1 }
            })
            .max()
            .unwrap_or(0)
    }

    /// The text of one indentation level.
    pub fn indent_unit(&self) -> &'static str {
        self.indent_unit
    }

    /// Caret position as (line, column).
    pub fn caret(&self) -> (usize, usize) {
        self.pos(self.head)
    }

    /// Where the selection started as (line, column); the caret itself when nothing is selected.
    pub fn anchor_pos(&self) -> (usize, usize) {
        self.pos(self.anchor)
    }

    /// The selection on line `i` as a column range, including the newline cell
    /// when the selection runs past the line's end.
    pub fn selection_on_line(&self, i: usize) -> Option<(usize, usize)> {
        let (a, b) = self.selection();
        if a == b || i >= self.len_lines() {
            return None;
        }
        let start = self.rope.line_to_char(i);
        let len = self.line_len(i);
        let end = start + len;
        if b <= start || a > end {
            return None;
        }
        let from = a.saturating_sub(start);
        let has_newline = i + 1 < self.len_lines();
        let to = if b > end && has_newline { len + 1 } else { (b - start).min(len) };
        (from < to).then_some((from, to))
    }

    /// The selection's start and end as (line, column).
    pub fn selection_bounds(&self) -> ((usize, usize), (usize, usize)) {
        let (a, b) = self.selection();
        (self.pos(a), self.pos(b))
    }

    pub fn selection(&self) -> (usize, usize) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }

    pub fn selected_text(&self) -> String {
        let (a, b) = self.selection();
        self.rope.slice(a..b).to_string()
    }

    fn pos(&self, idx: usize) -> (usize, usize) {
        let line = self.rope.char_to_line(idx);
        (line, idx - self.rope.line_to_char(line))
    }

    fn idx(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.len_lines() - 1);
        self.rope.line_to_char(line) + col.min(self.line_len(line))
    }

    // ---- syntax-tree support ----------------------------------------------

    /// Starts or stops recording edits. Consumers take an `edit_cursor` right
    /// after enabling and read with `edits_since`; the log is shared.
    pub fn set_track_edits(&self, on: bool) {
        self.track_edits.set(on);
    }

    /// Position in the edit log; everything recorded from here on is `>= ` it.
    pub fn edit_cursor(&self) -> u64 {
        self.edits_base.get() + self.edits.borrow().len() as u64
    }

    /// The edits recorded since `cursor`, oldest first. `None` if the log no
    /// longer reaches back that far.
    pub fn edits_since(&self, cursor: u64) -> Option<Vec<TextEdit>> {
        let base = self.edits_base.get();
        let edits = self.edits.borrow();
        let from = usize::try_from(cursor.checked_sub(base)?).ok()?;
        edits.get(from..).map(<[TextEdit]>::to_vec)
    }

    /// (line, UTF-16 column) of a char index.
    fn utf16_point(&self, idx: usize) -> (usize, usize) {
        let line = self.rope.char_to_line(idx);
        let col = self.rope.char_to_utf16_cu(idx) - self.rope.char_to_utf16_cu(self.rope.line_to_char(line));
        (line, col)
    }

    /// The document's bytes from `byte` to the end of the chunk it falls in
    /// (empty past the end), as a tree-sitter parse callback wants them.
    pub fn bytes_from(&self, byte: usize) -> &[u8] {
        if byte >= self.rope.len_bytes() {
            return &[];
        }
        let (chunk, start, _, _) = self.rope.chunk_at_byte(byte);
        &chunk.as_bytes()[byte - start..]
    }

    /// The document's bytes in `range`, as chunks.
    pub fn byte_chunks(&self, range: std::ops::Range<usize>) -> impl Iterator<Item = &[u8]> {
        self.rope.byte_slice(range).chunks().map(str::as_bytes)
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn line_to_byte(&self, line: usize) -> usize {
        if line >= self.len_lines() { self.rope.len_bytes() } else { self.rope.line_to_byte(line) }
    }

    /// (line, byte column) of a char index.
    fn point_at(&self, idx: usize) -> (usize, usize) {
        let line = self.rope.char_to_line(idx);
        (line, self.rope.char_to_byte(idx) - self.rope.line_to_byte(line))
    }

    /// The one place the text changes: replaces `remove` chars at `at` with
    /// `insert`, and records the edit when a parser is following along.
    fn mutate(&mut self, at: usize, remove: usize, insert: &str) {
        if self.track_edits.get() {
            let start_byte = self.rope.char_to_byte(at);
            let start = self.point_at(at);
            let newlines = insert.matches('\n').count();
            let new_end = match insert.rfind('\n') {
                Some(last) => (start.0 + newlines, insert.len() - last - 1),
                None => (start.0, start.1 + insert.len()),
            };
            let mut log = self.edits.borrow_mut();
            if log.len() >= EDIT_LOG_CAP {
                let drop = log.len() / 2;
                log.drain(..drop);
                self.edits_base.set(self.edits_base.get() + drop as u64);
            }
            log.push(TextEdit {
                start_byte,
                old_end_byte: self.rope.char_to_byte(at + remove),
                new_end_byte: start_byte + insert.len(),
                start,
                old_end: self.point_at(at + remove),
                new_end,
                lsp_start: self.utf16_point(at),
                lsp_end: self.utf16_point(at + remove),
                text: insert.to_string(),
            });
        }
        if remove > 0 {
            self.rope.remove(at..at + remove);
        }
        if !insert.is_empty() {
            self.rope.insert(at, insert);
        }
    }

    // ---- editing ---------------------------------------------------------

    fn apply(&mut self, at: usize, remove: usize, insert: &str) {
        self.revision += 1;
        let before = (self.anchor, self.head);
        let removed = self.rope.slice(at..at + remove).to_string();
        self.mutate(at, remove, insert);
        let caret = at + insert.chars().count();
        self.anchor = caret;
        self.head = caret;
        self.want_col = None;
        self.redo.clear();
        // A saved state past the undo depth can no longer be reached.
        if self.saved_depth.is_some_and(|d| d > self.undo.len()) {
            self.saved_depth = None;
        }
        // Typing a run of characters is one undo step (but never across a save).
        let at_saved_state = self.saved_depth == Some(self.undo.len());
        let sealed = std::mem::take(&mut self.seal);
        if let Some(last) = self.undo.last_mut() {
            let typed = !sealed
                && last.removed.is_empty()
                && remove == 0
                && last.at + last.inserted.chars().count() == at
                && !insert.contains('\n')
                && !last.inserted.contains('\n')
                && !insert.chars().all(char::is_whitespace)
                && !at_saved_state;
            if typed {
                last.inserted.push_str(insert);
                last.after = (caret, caret);
                return;
            }
        }
        self.undo.push(Edit { at, removed, inserted: insert.to_string(), before, after: (caret, caret) });
    }

    /// Replaces the whole text with `new_text` as a single undo step, touching
    /// only the span that differs, and keeps the caret where it was in the text.
    pub fn replace_text(&mut self, new_text: &str) {
        let new_text = new_text.replace("\r\n", "\n");
        let new: Vec<char> = new_text.chars().collect();
        let old_len = self.rope.len_chars();
        let common = old_len.min(new.len());
        let mut prefix = 0;
        while prefix < common && self.rope.char(prefix) == new[prefix] {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < common - prefix && self.rope.char(old_len - 1 - suffix) == new[new.len() - 1 - suffix] {
            suffix += 1;
        }
        let remove = old_len - prefix - suffix;
        let insert: String = new[prefix..new.len() - suffix].iter().collect();
        if remove == 0 && insert.is_empty() {
            return;
        }
        let (anchor, head) = (self.anchor, self.head);
        let inserted = insert.chars().count();
        self.seal = true;
        self.apply(prefix, remove, &insert);
        let map = |p: usize| {
            if p <= prefix {
                p
            } else if p >= old_len - suffix {
                p + inserted + suffix - (old_len - prefix)
            } else {
                prefix + inserted
            }
        };
        self.anchor = map(anchor);
        self.head = map(head);
    }

    /// Replaces the selection (if any) with `text`.
    pub fn insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n");
        let (a, b) = self.selection();
        self.apply(a, b - a, &text);
    }

    pub fn newline(&mut self) {
        let (line, _) = self.caret();
        let indent: String = self.line(line).chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let (a, b) = self.selection();
        let prev = (a > 0).then(|| self.rope.char(a - 1));
        let next = (b < self.rope.len_chars()).then(|| self.rope.char(b));
        let unit = self.indent_unit;
        // After an opener, indent one level more; between a pair, open a middle line.
        if let Some(open) = prev.filter(|c| matches!(c, '{' | '(' | '[')) {
            if next == closer_for(open) {
                self.insert(&format!("\n{indent}{unit}\n{indent}"));
                let middle = a + 1 + indent.chars().count() + unit.chars().count();
                self.anchor = middle;
                self.head = middle;
            } else {
                self.insert(&format!("\n{indent}{unit}"));
            }
            return;
        }
        self.insert(&format!("\n{indent}"));
    }

    pub fn backspace(&mut self) {
        let (a, b) = self.selection();
        if a != b {
            self.apply(a, b - a, "");
        } else if a > 0 {
            // Between an auto-closed pair, delete both halves.
            if a < self.rope.len_chars() && closer_for(self.rope.char(a - 1)) == Some(self.rope.char(a)) {
                return self.apply(a - 1, 2, "");
            }
            // In leading spaces, step back to the previous 4-column stop.
            let (line, col) = self.caret();
            let in_indent = col > 0 && self.line(line).chars().take(col).all(|c| c == ' ');
            let n = if in_indent { (col - 1) % 4 + 1 } else { 1 };
            self.apply(a - n, n, "");
        }
    }

    pub fn delete(&mut self) {
        let (a, b) = self.selection();
        if a != b {
            self.apply(a, b - a, "");
        } else if a < self.rope.len_chars() {
            self.apply(a, 1, "");
        }
    }

    pub fn undo(&mut self) {
        if let Some(e) = self.undo.pop() {
            self.revision += 1;
            let n = e.inserted.chars().count();
            self.mutate(e.at, n, &e.removed);
            (self.anchor, self.head) = e.before;
            self.want_col = None;
            self.redo.push(e);
        }
    }

    pub fn redo(&mut self) {
        if let Some(e) = self.redo.pop() {
            self.revision += 1;
            let n = e.removed.chars().count();
            self.mutate(e.at, n, &e.inserted);
            (self.anchor, self.head) = e.after;
            self.want_col = None;
            self.undo.push(e);
        }
    }

    // ---- movement --------------------------------------------------------

    fn move_to(&mut self, idx: usize, extend: bool) {
        self.head = idx.min(self.rope.len_chars());
        if !extend {
            self.anchor = self.head;
        }
    }

    pub fn left(&mut self, extend: bool, word: bool) {
        let (a, b) = self.selection();
        if !extend && a != b {
            self.want_col = None;
            return self.move_to(a, false);
        }
        self.want_col = None;
        let mut i = self.head;
        if word {
            while i > 0 && !is_word(self.rope.char(i - 1)) {
                i -= 1;
            }
            while i > 0 && is_word(self.rope.char(i - 1)) {
                i -= 1;
            }
        } else {
            i = i.saturating_sub(1);
        }
        self.move_to(i, extend);
    }

    pub fn right(&mut self, extend: bool, word: bool) {
        let (a, b) = self.selection();
        if !extend && a != b {
            self.want_col = None;
            return self.move_to(b, false);
        }
        self.want_col = None;
        let n = self.rope.len_chars();
        let mut i = self.head;
        if word {
            while i < n && !is_word(self.rope.char(i)) {
                i += 1;
            }
            while i < n && is_word(self.rope.char(i)) {
                i += 1;
            }
        } else {
            i = (i + 1).min(n);
        }
        self.move_to(i, extend);
    }

    pub fn vertical(&mut self, lines: isize, extend: bool) {
        let (line, col) = self.caret();
        // The remembered column is a cell, so a tab doesn't throw the caret off on the next line.
        let here = columns::visual_col(&self.line(line), col);
        let want = *self.want_col.get_or_insert(here);
        let target = (line as isize + lines).clamp(0, self.len_lines() as isize - 1) as usize;
        let idx = self.idx(target, columns::char_col(&self.line(target), want));
        self.move_to(idx, extend);
    }

    pub fn home(&mut self, extend: bool) {
        let (line, col) = self.caret();
        let first = self.line(line).chars().take_while(|c| c.is_whitespace()).count();
        // Smart home: first non-blank, then column 0.
        let idx = self.idx(line, if col == first { 0 } else { first });
        self.want_col = None;
        self.move_to(idx, extend);
    }

    pub fn end(&mut self, extend: bool) {
        let (line, _) = self.caret();
        let idx = self.idx(line, usize::MAX);
        self.want_col = None;
        self.move_to(idx, extend);
    }

    pub fn doc_start(&mut self, extend: bool) {
        self.want_col = None;
        self.move_to(0, extend);
    }

    pub fn doc_end(&mut self, extend: bool) {
        self.want_col = None;
        self.move_to(self.rope.len_chars(), extend);
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.head = self.rope.len_chars();
    }

    pub fn set_caret(&mut self, line: usize, col: usize, extend: bool) {
        self.want_col = None;
        let idx = self.idx(line, col);
        self.move_to(idx, extend);
    }

    // ---- selection helpers -----------------------------------------------

    /// Selects the word (or run of punctuation or spaces) at a position.
    pub fn select_word(&mut self, line: usize, col: usize) {
        let idx = self.idx(line, col);
        let n = self.rope.len_chars();
        let class = |c: char| if is_word(c) { 1 } else if c.is_whitespace() { 2 } else { 3 };
        let probe = if idx < n && self.rope.char(idx) != '\n' {
            idx
        } else if idx > 0 && self.rope.char(idx - 1) != '\n' {
            idx - 1
        } else {
            return;
        };
        let kind = class(self.rope.char(probe));
        let mut a = probe;
        while a > 0 && self.rope.char(a - 1) != '\n' && class(self.rope.char(a - 1)) == kind {
            a -= 1;
        }
        let mut b = probe + 1;
        while b < n && self.rope.char(b) != '\n' && class(self.rope.char(b)) == kind {
            b += 1;
        }
        self.want_col = None;
        self.anchor = a;
        self.head = b;
    }

    /// Selects a whole line including its newline.
    pub fn select_line(&mut self, line: usize) {
        let l = line.min(self.len_lines() - 1);
        self.want_col = None;
        self.anchor = self.rope.line_to_char(l);
        self.head = if l + 1 < self.len_lines() { self.rope.line_to_char(l + 1) } else { self.rope.len_chars() };
    }

    /// First and last line touched by the selection (a selection that ends at
    /// column 0 of a later line doesn't include that line).
    fn selected_lines(&self) -> (usize, usize) {
        let (a, b) = self.selection();
        let first = self.rope.char_to_line(a);
        let mut last = self.rope.char_to_line(b);
        if b > a && last > first && b == self.rope.line_to_char(last) {
            last -= 1;
        }
        (first, last)
    }

    // ---- line operations -------------------------------------------------

    /// Replaces lines `first..=last` with `f` applied to each, as one undo step.
    /// A lone caret stays where it was (shifted by the change); otherwise the
    /// transformed lines end up selected.
    fn rewrite_lines(&mut self, first: usize, last: usize, f: impl Fn(&str) -> String) {
        let start = self.rope.line_to_char(first);
        let end = self.rope.line_to_char(last) + self.line_len(last);
        let old: Vec<String> = (first..=last).map(|i| self.line(i)).collect();
        let new: Vec<String> = old.iter().map(|l| f(l)).collect();
        if old == new {
            return;
        }
        let (old_text, new_text) = (old.join("\n"), new.join("\n"));
        let had_selection = self.anchor != self.head;
        let caret = self.head;
        self.apply(start, end - start, &new_text);
        let new_len = new_text.chars().count();
        if had_selection || first != last {
            self.anchor = start;
            self.head = start + new_len;
        } else {
            let delta = new_len as isize - old_text.chars().count() as isize;
            let at = (caret as isize + delta).clamp(start as isize, (start + new_len) as isize) as usize;
            self.anchor = at;
            self.head = at;
        }
    }

    pub fn indent(&mut self) {
        let (first, last) = self.selected_lines();
        let unit = self.indent_unit;
        self.rewrite_lines(first, last, |l| if l.is_empty() { String::new() } else { format!("{unit}{l}") });
    }

    pub fn outdent(&mut self) {
        let (first, last) = self.selected_lines();
        self.rewrite_lines(first, last, |l| {
            if let Some(rest) = l.strip_prefix('\t') {
                return rest.to_string();
            }
            let n = l.chars().take(columns::TAB_WIDTH).take_while(|c| *c == ' ').count();
            l.chars().skip(n).collect()
        });
    }

    /// The Tab key: indent a multi-line selection, else a tab in tab files or
    /// spaces up to the next tab stop.
    pub fn tab(&mut self) {
        let (first, last) = self.selected_lines();
        if first != last {
            return self.indent();
        }
        if self.indent_unit == "\t" {
            return self.insert("\t");
        }
        let (a, _) = self.selection();
        let col = columns::visual_col(&self.line(first), a - self.rope.line_to_char(first));
        self.insert(&" ".repeat(columns::TAB_WIDTH - col % columns::TAB_WIDTH));
    }

    /// Toggles `prefix` line comments on the selected lines.
    pub fn toggle_comment(&mut self, prefix: &str) {
        let (first, last) = self.selected_lines();
        let lines: Vec<String> = (first..=last).map(|i| self.line(i)).collect();
        let nonblank: Vec<&String> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
        if nonblank.is_empty() {
            return;
        }
        let indent_of = |l: &str| l.chars().take_while(|c| c.is_whitespace()).count();
        let all_commented = nonblank.iter().all(|l| l.trim_start().starts_with(prefix));
        let min_indent = nonblank.iter().map(|l| indent_of(l)).min().unwrap_or(0);
        self.rewrite_lines(first, last, |l| {
            if l.trim().is_empty() {
                return l.to_string();
            }
            let chars: Vec<char> = l.chars().collect();
            if all_commented {
                let at = indent_of(l);
                let mut skip = prefix.chars().count();
                if chars.get(at + skip) == Some(&' ') {
                    skip += 1;
                }
                chars[..at].iter().chain(&chars[at + skip..]).collect()
            } else {
                let head: String = chars[..min_indent].iter().collect();
                let tail: String = chars[min_indent..].iter().collect();
                format!("{head}{prefix} {tail}")
            }
        });
    }

    /// Moves the selected lines up (`-1`) or down (`1`) past their neighbour.
    pub fn move_lines(&mut self, dir: isize) {
        let (first, last) = self.selected_lines();
        let neighbour = if dir < 0 {
            if first == 0 {
                return;
            }
            first - 1
        } else {
            if last + 1 >= self.len_lines() {
                return;
            }
            last + 1
        };
        let (lo, hi) = if dir < 0 { (neighbour, last) } else { (first, neighbour) };
        let block: Vec<String> = (first..=last).map(|i| self.line(i)).collect();
        let other = self.line(neighbour);
        let mut new: Vec<String> = Vec::new();
        if dir < 0 {
            new.extend(block);
            new.push(other.clone());
        } else {
            new.push(other.clone());
            new.extend(block);
        }
        let start = self.rope.line_to_char(lo);
        let end = self.rope.line_to_char(hi) + self.line_len(hi);
        let (anchor, head) = (self.anchor, self.head);
        self.apply(start, end - start, &new.join("\n"));
        // The text length is unchanged, so the selection just shifts by the neighbour's size.
        let shift = (other.chars().count() + 1) as isize;
        let shift = if dir < 0 { -shift } else { shift };
        self.anchor = (anchor as isize + shift) as usize;
        self.head = (head as isize + shift) as usize;
    }

    // ---- typing with pairs -----------------------------------------------

    /// Types one character: closes brackets and double quotes, types over a
    /// closer that is already there, and wraps a selection in a pair.
    pub fn type_char(&mut self, c: char) {
        let (a, b) = self.selection();
        let n = self.rope.len_chars();
        let next = (b < n).then(|| self.rope.char(b));
        let prev = (a > 0).then(|| self.rope.char(a - 1));

        if let Some(close) = closer_for(c) {
            if a != b {
                // Wrap the selection and keep it selected.
                let inner = self.selected_text();
                self.apply(a, b - a, &format!("{c}{inner}{close}"));
                self.anchor = a + 1;
                self.head = a + 1 + inner.chars().count();
                return;
            }
            let word = |x: Option<char>| x.is_some_and(is_word);
            let ok = if c == '"' || c == '`' {
                !word(prev) && !word(next) && prev != Some('\\') && next != Some(c)
            } else {
                next.is_none_or(|x| x.is_whitespace() || ")]}>,;".contains(x))
            };
            if ok {
                self.apply(a, 0, &format!("{c}{close}"));
                self.anchor = a + 1;
                self.head = a + 1;
                return;
            }
        } else if a == b && next == Some(c) && ")]}\"`".contains(c) {
            // Typing a closer that is already there just steps over it.
            self.want_col = None;
            return self.move_to(a + 1, false);
        }
        self.insert(&c.to_string());
    }

    // ---- find and replace ------------------------------------------------

    /// All occurrences of `query`, within single lines.
    pub fn find_matches(&self, query: &str, case_sensitive: bool) -> Vec<Match> {
        if query.is_empty() {
            return Vec::new();
        }
        let fold = |c: char| if case_sensitive { c } else { c.to_lowercase().next().unwrap_or(c) };
        let q: Vec<char> = query.chars().map(fold).collect();
        let mut out = Vec::new();
        for line in 0..self.len_lines() {
            let chars: Vec<char> = self.line(line).chars().map(fold).collect();
            let mut i = 0;
            while i + q.len() <= chars.len() {
                if chars[i..i + q.len()] == q[..] {
                    out.push(Match { line, start: i, end: i + q.len() });
                    i += q.len();
                } else {
                    i += 1;
                }
            }
        }
        out
    }

    pub fn select_match(&mut self, m: Match) {
        self.want_col = None;
        self.anchor = self.idx(m.line, m.start);
        self.head = self.idx(m.line, m.end);
    }

    /// The match the selection currently covers, if any.
    pub fn match_at_selection(&self, matches: &[Match]) -> Option<usize> {
        let (a, b) = self.selection();
        matches.iter().position(|m| self.idx(m.line, m.start) == a && self.idx(m.line, m.end) == b)
    }

    pub fn replace_match(&mut self, m: Match, with: &str) {
        let a = self.idx(m.line, m.start);
        let b = self.idx(m.line, m.end);
        self.apply(a, b - a, with);
    }

    /// Replaces every match in one undo step. Returns how many were replaced.
    pub fn replace_all(&mut self, matches: &[Match], with: &str) -> usize {
        if matches.is_empty() {
            return 0;
        }
        let lines: Vec<String> = (0..self.len_lines())
            .map(|i| {
                let mut chars: Vec<char> = self.line(i).chars().collect();
                for m in matches.iter().rev().filter(|m| m.line == i) {
                    chars.splice(m.start..m.end, with.chars());
                }
                chars.into_iter().collect()
            })
            .collect();
        let n = self.rope.len_chars();
        self.apply(0, n, &lines.join("\n"));
        self.anchor = 0;
        self.head = 0;
        matches.len()
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn closer_for(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        '`' => Some('`'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_is_one_undo_step() {
        let mut b = Buffer::new("");
        for c in "hello".chars() {
            b.insert(&c.to_string());
        }
        assert_eq!(b.to_file_text(), "hello");
        b.undo();
        assert_eq!(b.to_file_text(), "");
        b.redo();
        assert_eq!(b.to_file_text(), "hello");
    }

    #[test]
    fn newline_keeps_indent_and_backspace_dedents() {
        let mut b = Buffer::new("    foo");
        b.doc_end(false);
        b.newline();
        assert_eq!(b.caret(), (1, 4));
        b.backspace();
        assert_eq!(b.caret(), (1, 0));
    }

    #[test]
    fn vertical_keeps_wanted_column() {
        let mut b = Buffer::new("abcdef\nab\nabcdef");
        b.set_caret(0, 5, false);
        b.vertical(1, false);
        assert_eq!(b.caret(), (1, 2));
        b.vertical(1, false);
        assert_eq!(b.caret(), (2, 5));
    }

    #[test]
    fn selection_replace_and_dirty() {
        let mut b = Buffer::new("one two");
        assert!(!b.is_dirty());
        b.set_caret(0, 0, false);
        b.right(true, true);
        assert_eq!(b.selected_text(), "one");
        b.insert("1");
        assert_eq!(b.to_file_text(), "1 two");
        assert!(b.is_dirty());
        b.undo();
        assert!(!b.is_dirty());
    }

    #[test]
    fn word_and_line_selection() {
        let mut b = Buffer::new("let foo_bar = 1;\nnext");
        b.select_word(0, 6);
        assert_eq!(b.selected_text(), "foo_bar");
        b.select_word(0, 3);
        assert_eq!(b.selected_text(), " ");
        b.select_line(0);
        assert_eq!(b.selected_text(), "let foo_bar = 1;\n");
    }

    #[test]
    fn indent_outdent_and_comment() {
        let mut b = Buffer::new("a\n\nb");
        b.select_all();
        b.indent();
        assert_eq!(b.to_file_text(), "    a\n\n    b");
        b.undo();
        assert_eq!(b.to_file_text(), "a\n\nb");
        b.select_all();
        b.toggle_comment("//");
        assert_eq!(b.to_file_text(), "// a\n\n// b");
        b.toggle_comment("//");
        assert_eq!(b.to_file_text(), "a\n\nb");

        let mut b = Buffer::new("    x");
        b.set_caret(0, 6, false);
        b.outdent();
        assert_eq!(b.to_file_text(), "x");
        assert_eq!(b.caret(), (0, 1));
    }

    #[test]
    fn enter_after_opener_indents_and_splits_pairs() {
        let mut b = Buffer::new("fn f() {");
        b.doc_end(false);
        b.newline();
        assert_eq!(b.to_file_text(), "fn f() {\n    ");

        let mut b = Buffer::new("  x({})");
        b.set_caret(0, 5, false);
        b.newline();
        assert_eq!(b.to_file_text(), "  x({\n      \n  })");
        assert_eq!(b.caret(), (1, 6));
    }

    #[test]
    fn tab_goes_to_next_stop() {
        let mut b = Buffer::new("ab");
        b.set_caret(0, 2, false);
        b.tab();
        assert_eq!(b.to_file_text(), "ab  ");
    }

    #[test]
    fn move_lines_keeps_selection() {
        let mut b = Buffer::new("one\ntwo\nthree");
        b.set_caret(1, 0, false);
        b.set_caret(1, 3, true);
        b.move_lines(-1);
        assert_eq!(b.to_file_text(), "two\none\nthree");
        assert_eq!(b.selected_text(), "two");
        b.move_lines(1);
        b.move_lines(1);
        assert_eq!(b.to_file_text(), "one\nthree\ntwo");
        assert_eq!(b.selected_text(), "two");
    }

    #[test]
    fn pairs_close_skip_wrap_and_delete() {
        let mut b = Buffer::new("");
        b.type_char('(');
        assert_eq!(b.to_file_text(), "()");
        assert_eq!(b.caret(), (0, 1));
        b.type_char(')');
        assert_eq!(b.to_file_text(), "()");
        assert_eq!(b.caret(), (0, 2));
        b.left(false, false);
        b.backspace();
        assert_eq!(b.to_file_text(), "");

        let mut b = Buffer::new("word");
        b.select_all();
        b.type_char('"');
        assert_eq!(b.to_file_text(), "\"word\"");
        assert_eq!(b.selected_text(), "word");

        // No auto-close of a quote right after a word character.
        let mut b = Buffer::new("it");
        b.doc_end(false);
        b.type_char('"');
        assert_eq!(b.to_file_text(), "it\"");
    }

    #[test]
    fn find_and_replace() {
        let mut b = Buffer::new("Foo foo\nfoofoo");
        assert_eq!(b.find_matches("foo", true).len(), 3);
        let all = b.find_matches("FOO", false);
        assert_eq!(all.len(), 4);
        b.select_match(all[1]);
        assert_eq!(b.selected_text(), "foo");
        assert_eq!(b.match_at_selection(&all), Some(1));
        assert_eq!(b.replace_all(&all, "x"), 4);
        assert_eq!(b.to_file_text(), "x x\nxx");
        b.undo();
        assert_eq!(b.to_file_text(), "Foo foo\nfoofoo");
    }

    #[test]
    fn crlf_round_trips() {
        let b = Buffer::new("a\r\nb\r\n");
        assert_eq!(b.len_lines(), 3);
        assert_eq!(b.to_file_text(), "a\r\nb\r\n");
    }

    #[test]
    fn selection_on_line_spans_newline() {
        let mut b = Buffer::new("ab\ncd");
        b.set_caret(0, 1, false);
        b.set_caret(1, 1, true);
        assert_eq!(b.selection_on_line(0), Some((1, 3)));
        assert_eq!(b.selection_on_line(1), Some((0, 1)));
    }

    #[test]
    fn replace_text_is_one_undo_step_and_keeps_the_caret() {
        let mut b = Buffer::new("let foo = foo + 1;\nfoo();\n");
        b.set_caret(1, 2, false);
        let before = b.text();
        b.replace_text("let barbaz = barbaz + 1;\nbarbaz();\n");
        assert_eq!(b.text(), "let barbaz = barbaz + 1;\nbarbaz();\n");
        assert!(b.is_dirty());
        // The caret was inside the replaced span: it lands after the new text there.
        let (line, _) = b.caret();
        assert_eq!(line, 1);
        b.undo();
        assert_eq!(b.text(), before, "one undo restores everything");
        b.redo();
        assert_eq!(b.text(), "let barbaz = barbaz + 1;\nbarbaz();\n");
        // Identical text changes nothing.
        let rev = b.revision();
        b.replace_text("let barbaz = barbaz + 1;\nbarbaz();\n");
        assert_eq!(b.revision(), rev);
    }

    #[test]
    fn replace_text_does_not_merge_into_typing() {
        let mut b = Buffer::new("");
        for c in "ab".chars() {
            b.insert(&c.to_string());
        }
        b.set_caret(0, 2, false);
        b.replace_text("abcd");
        b.undo();
        assert_eq!(b.text(), "ab", "the replacement was its own step");
    }

    #[test]
    fn edit_log_has_utf16_ranges_and_text() {
        // 😀 is 2 UTF-16 units, é is 1 (2 bytes).
        let mut b = Buffer::new("a😀é
xy");
        b.set_track_edits(true);
        let start = b.edit_cursor();
        b.set_caret(0, 3, false); // after 😀é? columns are chars: a,😀,é -> 3
        b.insert("Z");
        b.set_caret(1, 1, false);
        b.backspace();
        let edits = b.edits_since(start).unwrap();
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].lsp_start, (0, 4));
        assert_eq!(edits[0].lsp_end, (0, 4));
        assert_eq!(edits[0].text, "Z");
        assert_eq!(edits[0].start, (0, 7)); // byte column: a(1)+😀(4)+é(2)
        assert_eq!((edits[1].lsp_start, edits[1].lsp_end), ((1, 0), (1, 1)));
        assert_eq!(edits[1].text, "");
    }

    #[test]
    fn edit_log_serves_independent_consumers() {
        let mut b = Buffer::new("");
        b.set_track_edits(true);
        let (a, c) = (b.edit_cursor(), b.edit_cursor());
        b.insert("x");
        let mid = b.edit_cursor();
        b.insert("y");
        assert_eq!(b.edits_since(a).unwrap().len(), 2);
        assert_eq!(b.edits_since(mid).unwrap().len(), 1);
        assert_eq!(b.edits_since(c).unwrap().len(), 2);
        assert!(b.edits_since(b.edit_cursor()).unwrap().is_empty());
    }

    #[test]
    fn edit_log_overflow_tells_slow_consumers() {
        let mut b = Buffer::new("");
        b.set_track_edits(true);
        let slow = b.edit_cursor();
        for _ in 0..EDIT_LOG_CAP + 10 {
            b.insert("x");
        }
        assert!(b.edits_since(slow).is_none());
        let fresh = b.edit_cursor();
        b.insert("y");
        assert_eq!(b.edits_since(fresh).unwrap().len(), 1);
    }

    #[test]
    fn tabs_survive_a_round_trip() {
        let text = "all:\n\tgcc a.c\n\t\tx\r\n  mixed\t \n";
        // Mixed line endings aside, a tabbed LF file comes back byte for byte.
        let lf = "all:\n\tgcc a.c\n\t\tx\n  mixed\t \n";
        assert_eq!(Buffer::new(lf).to_file_text(), lf);
        let crlf = text.replace("\r\n", "\n").replace('\n', "\r\n");
        assert_eq!(Buffer::new(&crlf).to_file_text(), crlf);
    }

    #[test]
    fn indent_style_follows_the_file() {
        assert_eq!(Buffer::new("fn a() {\n\tx;\n\ty;\n}\n").indent_unit(), "\t");
        assert_eq!(Buffer::new("fn a() {\n    x;\n}\n").indent_unit(), "    ");
        assert_eq!(Buffer::new("").indent_unit(), "    ");

        let mut b = Buffer::new("fn a() {\n\tx;\n}\n");
        b.set_caret(0, 8, false);
        b.newline();
        assert_eq!(b.text(), "fn a() {\n\t\n\tx;\n}\n");
        // Tab key inserts a tab, indent adds one to each line, outdent takes one off.
        let mut b = Buffer::new("\ta\n\tb\n");
        b.set_caret(0, 1, false);
        b.tab();
        assert_eq!(b.line(0), "\t\ta");
        b.select_all();
        b.indent();
        assert_eq!(b.line(1), "\t\tb");
        b.outdent();
        assert_eq!(b.line(1), "\tb");
    }

    #[test]
    fn spaces_files_still_tab_to_the_next_stop() {
        let mut b = Buffer::new("ab\n");
        b.set_caret(0, 2, false);
        b.tab();
        assert_eq!(b.line(0), "ab  ");
        // A tab character earlier on the line counts as its full width.
        let mut b = Buffer::new("\tab\n    c\n");
        b.set_caret(0, 3, false);
        b.tab();
        assert_eq!(b.line(0), "\tab  ");
    }

    #[test]
    fn up_and_down_keep_the_cell_column() {
        // Caret after "ab" on a space line is cell 2; the tab line has cells 0..4 for its tab.
        let mut b = Buffer::new("abcdefgh\n\tx\nabcdefgh\n");
        b.set_caret(0, 6, false);
        b.vertical(1, false);
        // Cell 6 of "\tx" is past the end, so the caret clamps to the line end...
        assert_eq!(b.caret(), (1, 2));
        b.vertical(1, false);
        // ...and comes back to cell 6 on the next line, not to char column 2.
        assert_eq!(b.caret(), (2, 6));
        // A caret after a tab sits at cell 4 and lands on the same cell below.
        let mut b = Buffer::new("\tx\nabcdefgh\n");
        b.set_caret(0, 1, false);
        b.vertical(1, false);
        assert_eq!(b.caret(), (1, 4));
        assert_eq!(b.max_cols(), 9);
    }
}
