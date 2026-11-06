//! Diagnostics as the editor wants them, and how they follow edits made after
//! the server computed them.

use crate::docs::ContentChange;
use std::path::Path;

pub const ERROR: u8 = 1;
pub const WARNING: u8 = 2;
pub const INFO: u8 = 3;
pub const HINT: u8 = 4;

/// One diagnostic. Positions are LSP's: (line, UTF-16 column).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diag {
    pub start: (usize, usize),
    pub end: (usize, usize),
    /// `ERROR`, `WARNING`, `INFO` or `HINT`.
    pub severity: u8,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
    /// Computed from the last saved text rather than the live document (a
    /// background `cargo check`): its positions only follow edits made after the save.
    pub saved_text: bool,
    /// The diagnostic as the server sent it, to hand back with code action requests.
    pub raw: Option<serde_json::Value>,
}

/// What a server last published for one file.
#[derive(Clone, Debug, Default)]
pub struct FileDiags {
    /// The file, as the server named it (case and separators as published).
    pub path: std::path::PathBuf,
    /// The document version the diagnostics were computed for, if the server said.
    pub version: Option<i64>,
    pub diags: Vec<Diag>,
}

impl From<lsp_types::Diagnostic> for Diag {
    fn from(d: lsp_types::Diagnostic) -> Self {
        use lsp_types::{DiagnosticSeverity as S, NumberOrString};
        let raw = serde_json::to_value(&d).ok();
        let severity = match d.severity {
            Some(s) if s == S::WARNING => WARNING,
            Some(s) if s == S::INFORMATION => INFO,
            Some(s) if s == S::HINT => HINT,
            _ => ERROR,
        };
        Diag {
            start: (d.range.start.line as usize, d.range.start.character as usize),
            end: (d.range.end.line as usize, d.range.end.character as usize),
            severity,
            message: d.message,
            source: d.source,
            code: d.code.map(|c| match c {
                NumberOrString::Number(n) => n.to_string(),
                NumberOrString::String(s) => s,
            }),
            saved_text: false,
            raw,
        }
    }
}

impl Diag {
    /// Moves the range to follow `change`, which replaced text in the document
    /// the range was valid for. Returns `false` if the edit removed the range.
    pub fn apply(&mut self, change: &ContentChange) -> bool {
        let was_empty = self.start == self.end;
        self.start = shift(self.start, change, false);
        self.end = shift(self.end, change, true);
        was_empty || self.start < self.end
    }

    /// The diagnostic in the form a code action request carries. Diagnostics of
    /// the live text use their current (edit-shifted) range; saved-text ones keep
    /// the range the server knows them by.
    pub fn context_json(&self) -> serde_json::Value {
        let mut v = self.raw.clone().unwrap_or_else(|| serde_json::json!({"message": self.message, "severity": self.severity}));
        if !self.saved_text || v.get("range").is_none() {
            v["range"] = serde_json::json!({
                "start": {"line": self.start.0, "character": self.start.1},
                "end": {"line": self.end.0, "character": self.end.1},
            });
        }
        v
    }

    /// `message [source code]` for display.
    pub fn text(&self) -> String {
        match (&self.source, &self.code) {
            (Some(s), Some(c)) => format!("{} [{s} {c}]", self.message),
            (Some(s), None) => format!("{} [{s}]", self.message),
            (None, Some(c)) => format!("{} [{c}]", self.message),
            (None, None) => self.message.clone(),
        }
    }
}

/// Drops a diagnostic from a saved-text check (`cargo check`) when a live one
/// already covers the same range at the same severity, so one problem isn't
/// counted twice.
pub fn dedupe(diags: Vec<Diag>) -> Vec<Diag> {
    let live: std::collections::HashSet<_> =
        diags.iter().filter(|d| !d.saved_text).map(|d| (d.start, d.end, d.severity)).collect();
    diags.into_iter().filter(|d| !(d.saved_text && live.contains(&(d.start, d.end, d.severity)))).collect()
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// A range's start or end after `c`. A start sitting where text is inserted
/// moves right with it; an end sitting there stays, so typing at either edge
/// lands outside the range.
fn shift(p: (usize, usize), c: &ContentChange, is_end: bool) -> (usize, usize) {
    let (s, e) = (c.start, c.end);
    let new_end = match c.text.rfind('\n') {
        Some(i) => (s.0 + c.text.matches('\n').count(), utf16_len(&c.text[i + 1..])),
        None => (s.0, s.1 + utf16_len(&c.text)),
    };
    if is_end {
        if p <= s {
            return p;
        }
        if p < e {
            return new_end;
        }
    } else {
        if p < s {
            return p;
        }
        if p < e {
            return s;
        }
    }
    // At or after the end of the replaced text: slide by the size change.
    if p.0 == e.0 { (new_end.0, new_end.1 + (p.1 - e.1)) } else { (p.0 + new_end.0 - e.0, p.1) }
}

/// Converts a UTF-16 column to a char column on `line`, clamped to its length.
pub fn utf16_to_char_col(line: &str, col: usize) -> usize {
    let mut units = 0;
    for (i, ch) in line.chars().enumerate() {
        if units >= col {
            return i;
        }
        units += ch.len_utf16();
    }
    line.chars().count()
}

/// Converts a char column on `line` to a UTF-16 column (clamped to the line).
pub fn char_to_utf16_col(line: &str, col: usize) -> usize {
    line.chars().take(col).map(char::len_utf16).sum()
}

/// Parses a `file://` URI into a comparable key (see `path_key`).
pub fn uri_key(uri: &str) -> String {
    normalize_key(uri_path(uri))
}

/// The file path a `file://` URI names, with its case kept.
pub fn uri_to_path(uri: &str) -> std::path::PathBuf {
    let path = uri_path(uri);
    std::path::PathBuf::from(if cfg!(windows) { path.replace('/', "\\") } else { path })
}

fn uri_path(uri: &str) -> String {
    let rest = uri.strip_prefix("file://").unwrap_or(uri);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(v) = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let mut s = String::from_utf8_lossy(&out).into_owned();
    // `/C:/dir` -> `C:/dir`
    if s.len() >= 3 && s.as_bytes()[0] == b'/' && s.as_bytes()[2] == b':' {
        s.remove(0);
    }
    s
}

/// A key for a file path that matches `uri_key` of its URI (separators and,
/// on Windows, case are normalised).
pub fn path_key(path: &Path) -> String {
    normalize_key(crate::manager::normalize_path(path).to_string_lossy().into_owned())
}

fn normalize_key(s: String) -> String {
    let s = s.replace('\\', "/");
    if cfg!(windows) { s.to_lowercase() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(start: (usize, usize), end: (usize, usize), text: &str) -> ContentChange {
        ContentChange { start, end, text: text.into() }
    }

    fn diag(start: (usize, usize), end: (usize, usize)) -> Diag {
        Diag { start, end, severity: ERROR, message: "m".into(), source: None, code: None, saved_text: false, raw: None }
    }

    fn after(d: Diag, c: ContentChange) -> Option<((usize, usize), (usize, usize))> {
        let mut d = d;
        d.apply(&c).then_some((d.start, d.end))
    }

    #[test]
    fn insertion_before_moves_range() {
        let d = diag((2, 10), (2, 15));
        assert_eq!(after(d.clone(), change((2, 4), (2, 4), "xyz")), Some(((2, 13), (2, 18))));
        // A new line above pushes it down; columns stay.
        assert_eq!(after(d.clone(), change((0, 0), (0, 0), "a\nb\n")), Some(((4, 10), (4, 15))));
        // Joining lines above pulls it up.
        assert_eq!(after(d, change((0, 3), (1, 0), "")), Some(((1, 10), (1, 15))));
    }

    #[test]
    fn edits_on_the_same_line_after_the_range_do_nothing() {
        let d = diag((2, 10), (2, 15));
        assert_eq!(after(d.clone(), change((2, 20), (2, 20), "zzz")), Some(((2, 10), (2, 15))));
        assert_eq!(after(d, change((5, 0), (5, 0), "q")), Some(((2, 10), (2, 15))));
    }

    #[test]
    fn typing_at_the_edges_stays_outside() {
        let d = diag((1, 4), (1, 8));
        assert_eq!(after(d.clone(), change((1, 4), (1, 4), "ab")), Some(((1, 6), (1, 10))));
        assert_eq!(after(d, change((1, 8), (1, 8), "ab")), Some(((1, 4), (1, 8))));
    }

    #[test]
    fn typing_inside_grows_the_range() {
        let d = diag((1, 4), (1, 8));
        assert_eq!(after(d, change((1, 6), (1, 6), "ab")), Some(((1, 4), (1, 10))));
    }

    #[test]
    fn newline_inside_splits_the_end_onto_the_next_line() {
        let d = diag((1, 4), (1, 8));
        // "\n  " inserted at col 6: the end moves to line 2, column 2 + (8 - 6).
        assert_eq!(after(d, change((1, 6), (1, 6), "\n  ")), Some(((1, 4), (2, 4))));
    }

    #[test]
    fn deleting_the_range_removes_it_and_partial_overlap_trims() {
        let d = diag((1, 4), (1, 8));
        assert_eq!(after(d.clone(), change((1, 2), (1, 10), "")), None);
        assert_eq!(after(d.clone(), change((1, 6), (1, 12), "")), Some(((1, 4), (1, 6))));
        assert_eq!(after(d, change((1, 0), (1, 6), "")), Some(((1, 0), (1, 2))));
    }

    #[test]
    fn utf16_units_are_counted() {
        // 😀 is two UTF-16 units.
        let d = diag((0, 10), (0, 12));
        assert_eq!(after(d, change((0, 0), (0, 0), "😀")), Some(((0, 12), (0, 14))));
        assert_eq!(utf16_to_char_col("a😀b", 3), 2);
        assert_eq!(utf16_to_char_col("a😀b", 1), 1);
        assert_eq!(utf16_to_char_col("ab", 99), 2);
        assert_eq!(char_to_utf16_col("a😀b", 2), 3);
        assert_eq!(char_to_utf16_col("ab", 99), 2);
    }

    #[test]
    fn uri_and_path_keys_agree() {
        let k = uri_key("file:///c%3A/Users/Me/My%20Dir/a.rs");
        assert_eq!(k, normalize_key("C:/Users/Me/My Dir/a.rs".into()));
        assert_eq!(uri_key("file:///C:/Users/Me/My%20Dir/a.rs"), k);
        assert_eq!(uri_key("file:///usr/x/a.rs"), "/usr/x/a.rs");
    }

    #[test]
    fn saved_text_duplicates_of_live_diagnostics_are_dropped() {
        let mut saved = diag((1, 1), (1, 4));
        saved.saved_text = true;
        saved.message = "from cargo check".into();
        let live = diag((1, 1), (1, 4));
        let mut elsewhere = diag((2, 0), (2, 3));
        elsewhere.saved_text = true;
        let kept = dedupe(vec![saved, live, elsewhere]);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|d| d.message != "from cargo check"));
    }

    #[test]
    fn uri_to_path_keeps_case() {
        let p = uri_to_path("file:///C:/Users/Me/My%20Dir/Main.rs");
        let shown = p.to_string_lossy().replace('\\', "/");
        assert_eq!(shown, "C:/Users/Me/My Dir/Main.rs");
    }

    #[test]
    fn context_json_uses_the_right_range() {
        let raw = serde_json::json!({"range": {"start": {"line": 1, "character": 1}, "end": {"line": 1, "character": 4}}, "message": "m", "code": "E1"});
        let mut live = diag((3, 1), (3, 4));
        live.raw = Some(raw.clone());
        assert_eq!(live.context_json()["range"]["start"]["line"], 3, "live diagnostics follow edits");
        assert_eq!(live.context_json()["code"], "E1");
        let mut saved = live.clone();
        saved.saved_text = true;
        assert_eq!(saved.context_json()["range"]["start"]["line"], 1, "saved-text ones keep the server's range");
        assert_eq!(diag((0, 0), (0, 1)).context_json()["range"]["end"]["character"], 1);
    }

    #[test]
    fn text_includes_source_and_code() {
        let mut d = diag((0, 0), (0, 1));
        d.source = Some("rustc".into());
        d.code = Some("E0308".into());
        assert_eq!(d.text(), "m [rustc E0308]");
    }
}
