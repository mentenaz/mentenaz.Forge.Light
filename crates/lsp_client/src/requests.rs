//! Position-based requests (hover, go to definition) with their replies
//! reduced to what the editor shows. Replies are parsed from raw JSON because
//! servers answer in several equivalent shapes.

use crate::client::{Client, Error};
use crate::diag::uri_to_path;
use crate::docs::ContentChange;
use serde_json::{Value, json};
use std::path::PathBuf;

/// Text for a hover popup and, if the server said, the range it describes
/// as ((line, UTF-16 col), (line, UTF-16 col)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoverInfo {
    pub text: String,
    pub range: Option<((usize, usize), (usize, usize))>,
}

/// Where a symbol is defined: a file and an LSP position (UTF-16 column).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
}

fn at(uri: &str, pos: (usize, usize)) -> Value {
    json!({"textDocument": {"uri": uri}, "position": {"line": pos.0, "character": pos.1}})
}

pub async fn hover(client: &Client, uri: &str, pos: (usize, usize)) -> Result<Option<HoverInfo>, Error> {
    let reply = client.request_raw("textDocument/hover", at(uri, pos)).await?;
    Ok(parse_hover(&reply))
}

pub async fn definition(client: &Client, uri: &str, pos: (usize, usize)) -> Result<Vec<Target>, Error> {
    let reply = client.request_raw("textDocument/definition", at(uri, pos)).await?;
    Ok(parse_locations(&reply))
}

/// Every place that uses the symbol at `pos`, its definition included.
pub async fn references(client: &Client, uri: &str, pos: (usize, usize)) -> Result<Vec<Target>, Error> {
    let mut params = at(uri, pos);
    params["context"] = json!({"includeDeclaration": true});
    let reply = client.request_raw("textDocument/references", params).await?;
    Ok(parse_locations(&reply))
}

/// The edits that rename the symbol at `pos` to `new_name`, by file.
pub async fn rename(client: &Client, uri: &str, pos: (usize, usize), new_name: &str) -> Result<Vec<FileEdit>, Error> {
    let mut params = at(uri, pos);
    params["newName"] = json!(new_name);
    let reply = client.request_raw("textDocument/rename", params).await?;
    parse_workspace_edit(&reply).map_err(|message| Error::Server { code: 0, message })
}

/// The edits that format the whole document.
pub async fn format(client: &Client, uri: &str) -> Result<Vec<ContentChange>, Error> {
    let params = json!({"textDocument": {"uri": uri}, "options": {"tabSize": 4, "insertSpaces": true}});
    let reply = client.request_raw("textDocument/formatting", params).await?;
    Ok(text_edits(&reply))
}

/// One file's share of a workspace edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEdit {
    pub path: PathBuf,
    pub edits: Vec<ContentChange>,
}

fn text_edits(list: &Value) -> Vec<ContentChange> {
    let Some(items) = list.as_array() else { return Vec::new() };
    items
        .iter()
        .filter_map(|e| {
            let (start, end) = range(e.get("range")?)?;
            Some(ContentChange { start, end, text: e.get("newText")?.as_str()?.to_owned() })
        })
        .collect()
}

/// A `WorkspaceEdit` as per-file edits. File operations (create, rename,
/// delete) are refused rather than half applied.
pub fn parse_workspace_edit(v: &Value) -> Result<Vec<FileEdit>, String> {
    let mut files: Vec<FileEdit> = Vec::new();
    let mut add = |uri: &str, edits: Vec<ContentChange>| {
        let path = uri_to_path(uri);
        match files.iter_mut().find(|f| crate::diag::path_key(&f.path) == crate::diag::path_key(&path)) {
            Some(f) => f.edits.extend(edits),
            None => files.push(FileEdit { path, edits }),
        }
    };
    if let Some(changes) = v.get("documentChanges").and_then(Value::as_array) {
        for change in changes {
            if change.get("kind").is_some() {
                return Err("This change needs file operations (create, rename or delete), which aren't supported yet.".into());
            }
            let uri = change.get("textDocument").and_then(|d| d.get("uri")).and_then(Value::as_str);
            if let (Some(uri), Some(edits)) = (uri, change.get("edits")) {
                add(uri, text_edits(edits));
            }
        }
    } else if let Some(changes) = v.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes {
            add(uri, text_edits(edits));
        }
    }
    files.retain(|f| !f.edits.is_empty());
    Ok(files)
}

/// `text` (LF line endings) with `edits` applied. The edits are in LSP
/// positions (line, UTF-16 column), all relative to the original text.
pub fn apply_changes(text: &str, edits: &[ContentChange]) -> String {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    let offset = |(line, col16): (usize, usize)| -> usize {
        let Some(&start) = starts.get(line) else { return text.len() };
        let end = starts.get(line + 1).map_or(text.len(), |&n| n - 1);
        let mut units = 0;
        for (i, ch) in text[start..end].char_indices() {
            if units >= col16 {
                return start + i;
            }
            units += ch.len_utf16();
        }
        end
    };
    let mut sorted: Vec<(usize, usize, &str)> = edits.iter().map(|e| (offset(e.start), offset(e.end), e.text.as_str())).collect();
    sorted.sort_by_key(|&(start, end, _)| (start, end));
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end, replacement) in sorted {
        if start < at {
            continue; // overlapping edits are invalid; keep the first
        }
        out.push_str(&text[at..start]);
        out.push_str(replacement);
        at = end.max(start);
    }
    out.push_str(&text[at..]);
    out
}

fn position(v: &Value) -> Option<(usize, usize)> {
    Some((v.get("line")?.as_u64()? as usize, v.get("character")?.as_u64()? as usize))
}

fn range(v: &Value) -> Option<((usize, usize), (usize, usize))> {
    Some((position(v.get("start")?)?, position(v.get("end")?)?))
}

pub fn parse_hover(v: &Value) -> Option<HoverInfo> {
    fn part(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => o.get("value")?.as_str().map(str::to_owned),
            Value::Array(items) => {
                let parts: Vec<String> = items.iter().filter_map(part).collect();
                (!parts.is_empty()).then(|| parts.join("\n\n"))
            }
            _ => None,
        }
    }
    let text = plain_text(&part(v.get("contents")?)?);
    (!text.is_empty()).then(|| HoverInfo { text, range: v.get("range").and_then(range) })
}

/// `Location`, `Location[]` or `LocationLink[]` (or null) as targets.
pub fn parse_locations(v: &Value) -> Vec<Target> {
    fn one(v: &Value) -> Option<Target> {
        // A link names the file in `targetUri` and the identifier in `targetSelectionRange`.
        let (uri, r) = match v.get("targetUri") {
            Some(uri) => (uri, v.get("targetSelectionRange").or_else(|| v.get("targetRange"))?),
            None => (v.get("uri")?, v.get("range")?),
        };
        let (line, col) = range(r)?.0;
        Some(Target { path: uri_to_path(uri.as_str()?), line, col })
    }
    match v {
        Value::Array(items) => items.iter().filter_map(one).collect(),
        Value::Object(_) => one(v).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Markdown reduced to readable plain text: fences, rules and link syntax
/// removed, long answers cut.
pub fn plain_text(md: &str) -> String {
    const MAX_LINES: usize = 18;
    let mut lines: Vec<String> = Vec::new();
    for raw in md.lines() {
        let t = raw.trim_end();
        if t.trim_start().starts_with("```") {
            continue;
        }
        let line = if matches!(t.trim(), "---" | "***" | "___") { String::new() } else { strip_links(t).replace('`', "").replace("**", "") };
        if line.is_empty() && lines.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        lines.push(line);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    if lines.len() > MAX_LINES {
        lines.truncate(MAX_LINES);
        lines.push("…".into());
    }
    lines.join("\n")
}

/// `[text](url)` becomes `text`.
fn strip_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        match after.find("](").and_then(|mid| after[mid + 2..].find(')').map(|end| (mid, end))) {
            Some((mid, end)) => {
                out.push_str(&rest[..open]);
                out.push_str(&after[..mid]);
                rest = &after[mid + 2 + end + 1..];
            }
            None => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_markup_content_and_range() {
        let v = json!({
            "contents": {"kind": "markdown", "value": "```rust\nproj\n```\n\n```rust\nfn main()\n```\n\n---\n\nsize = 0, align = 1\n\n[docs](http://x)"},
            "range": {"start": {"line": 2, "character": 3}, "end": {"line": 2, "character": 7}}
        });
        let h = parse_hover(&v).unwrap();
        assert_eq!(h.text, "proj\n\nfn main()\n\nsize = 0, align = 1\n\ndocs");
        assert_eq!(h.range, Some(((2, 3), (2, 7))));
    }

    #[test]
    fn hover_other_shapes() {
        assert_eq!(parse_hover(&json!({"contents": "plain"})).unwrap().text, "plain");
        assert_eq!(
            parse_hover(&json!({"contents": [{"language": "rust", "value": "let x"}, "more"]})).unwrap().text,
            "let x\n\nmore"
        );
        assert!(parse_hover(&Value::Null).is_none());
        assert!(parse_hover(&json!({"contents": "  "})).is_none());
    }

    #[test]
    fn locations_in_every_shape() {
        let loc = json!({"uri": "file:///C:/p/a.rs", "range": {"start": {"line": 4, "character": 7}, "end": {"line": 4, "character": 9}}});
        let link = json!({
            "targetUri": "file:///C:/p/b.rs",
            "targetRange": {"start": {"line": 1, "character": 0}, "end": {"line": 9, "character": 1}},
            "targetSelectionRange": {"start": {"line": 2, "character": 4}, "end": {"line": 2, "character": 8}}
        });
        let one = parse_locations(&loc);
        assert_eq!((one[0].line, one[0].col), (4, 7));
        let many = parse_locations(&json!([loc, link]));
        assert_eq!(many.len(), 2);
        assert_eq!((many[1].line, many[1].col), (2, 4), "a link points at the identifier, not the whole item");
        assert!(many[1].path.to_string_lossy().ends_with("b.rs"));
        assert!(parse_locations(&Value::Null).is_empty());
        assert!(parse_locations(&json!([])).is_empty());
    }

    fn change(start: (usize, usize), end: (usize, usize), text: &str) -> ContentChange {
        ContentChange { start, end, text: text.into() }
    }

    #[test]
    fn apply_changes_handles_many_edits_lines_and_utf16() {
        let text = "let foo = 1;\nfoo(\"😀\", foo);\n";
        let edits = [
            change((1, 0), (1, 3), "bar"),
            change((0, 4), (0, 7), "bar"),
            // After the emoji (2 UTF-16 units): `foo(` is 4, quote 1, emoji 2, quote 1, `, ` 2 = col 10.
            change((1, 10), (1, 13), "bar"),
        ];
        assert_eq!(apply_changes(text, &edits), "let bar = 1;\nbar(\"😀\", bar);\n");
        assert_eq!(apply_changes("a\nb", &[change((1, 1), (1, 1), "!")]), "a\nb!");
        assert_eq!(apply_changes("ab", &[change((0, 1), (3, 0), "")]), "a", "positions past the end clamp");
        assert_eq!(apply_changes("x\ny\n", &[change((0, 0), (1, 1), "z")]), "z\n");
    }

    #[test]
    fn workspace_edit_changes_and_document_changes() {
        let edit = |s: &str| json!({"range": {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 3}}, "newText": s});
        let a = parse_workspace_edit(&json!({"changes": {"file:///C:/p/a.rs": [edit("x")], "file:///C:/p/b.rs": [edit("y")]}})).unwrap();
        assert_eq!(a.len(), 2);
        let b = parse_workspace_edit(&json!({"documentChanges": [
            {"textDocument": {"uri": "file:///C:/p/a.rs", "version": 3}, "edits": [edit("x")]},
            {"textDocument": {"uri": "file:///c%3A/p/a.rs", "version": 3}, "edits": [edit("z")]}
        ]})).unwrap();
        assert_eq!(b.len(), 1, "the same file under two spellings is merged");
        assert_eq!(b[0].edits.len(), 2);
        assert!(parse_workspace_edit(&json!({"documentChanges": [{"kind": "rename", "oldUri": "a", "newUri": "b"}]})).is_err());
        assert!(parse_workspace_edit(&Value::Null).unwrap().is_empty());
    }

    #[test]
    fn plain_text_cuts_long_answers_and_keeps_brackets() {
        let long = (0..40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let out = plain_text(&long);
        assert_eq!(out.lines().count(), 19);
        assert!(out.ends_with('…'));
        assert_eq!(plain_text("arr[0] and [x] here"), "arr[0] and [x] here");
    }
}
