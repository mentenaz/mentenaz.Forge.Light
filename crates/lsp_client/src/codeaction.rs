//! Code actions (quick fixes and refactors): the request and a parser that
//! keeps only what we can apply, the actions that carry a plain edit.

use crate::client::{Client, Error};
use crate::requests::{FileEdit, parse_workspace_edit};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeAction {
    pub title: String,
    /// `quickfix`, `refactor.extract`, `source.organizeImports`, ... ("" if the server gave none).
    pub kind: String,
    pub preferred: bool,
    pub edits: Vec<FileEdit>,
}

impl CodeAction {
    /// 101 quickfix, 102 refactor, 103 source, 100 anything else (what the editor draws a badge for).
    pub fn kind_number(&self) -> i32 {
        match self.kind.split('.').next().unwrap_or_default() {
            "quickfix" => 101,
            "refactor" => 102,
            "source" => 103,
            _ => 100,
        }
    }
}

/// The actions available for `range` ((line, UTF-16 col) pairs). `diagnostics`
/// are the diagnostics overlapping it, in the shape the server published them.
pub async fn code_actions(client: &Client, uri: &str, range: ((usize, usize), (usize, usize)), diagnostics: Vec<Value>) -> Result<Vec<CodeAction>, Error> {
    let params = json!({
        "textDocument": {"uri": uri},
        "range": {
            "start": {"line": range.0.0, "character": range.0.1},
            "end": {"line": range.1.0, "character": range.1.1},
        },
        "context": {"diagnostics": diagnostics, "triggerKind": 1},
    });
    let reply = client.request_raw("textDocument/codeAction", params).await?;
    Ok(parse_code_actions(&reply))
}

/// Actions with an edit, best first. Bare commands, disabled actions and
/// edits that need file operations are left out.
pub fn parse_code_actions(v: &Value) -> Vec<CodeAction> {
    let Some(items) = v.as_array() else { return Vec::new() };
    let mut actions: Vec<CodeAction> = items
        .iter()
        .filter_map(|item| {
            if item.get("disabled").is_some() {
                return None;
            }
            let edits = parse_workspace_edit(item.get("edit")?).ok()?;
            if edits.is_empty() {
                return None;
            }
            Some(CodeAction {
                title: item.get("title")?.as_str()?.to_owned(),
                kind: item.get("kind").and_then(Value::as_str).unwrap_or_default().to_owned(),
                preferred: item.get("isPreferred").and_then(Value::as_bool).unwrap_or(false),
                edits,
            })
        })
        .collect();
    let rank = |a: &CodeAction| match a.kind_number() {
        101 => 0,
        102 => 1,
        103 => 2,
        _ => 3,
    };
    actions.sort_by_key(|a| (!a.preferred, rank(a)));
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(text: &str) -> Value {
        json!({"changes": {"file:///C:/p/a.rs": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}, "newText": text}]}})
    }

    #[test]
    fn keeps_editing_actions_best_first() {
        let v = json!([
            {"title": "Extract into function", "kind": "refactor.extract", "edit": edit("f")},
            {"title": "Run test", "command": {"title": "Run", "command": "x"}},
            {"title": "Import `HashMap`", "kind": "quickfix", "edit": edit("use"), "isPreferred": false},
            {"title": "Locked", "kind": "refactor", "edit": edit("z"), "disabled": {"reason": "no"}},
            {"title": "Make file", "kind": "refactor", "edit": {"documentChanges": [{"kind": "create", "uri": "file:///C:/p/b.rs"}]}},
            {"title": "Best fix", "kind": "quickfix", "edit": edit("fix"), "isPreferred": true},
            {"title": "Empty", "kind": "quickfix", "edit": {"changes": {}}}
        ]);
        let titles: Vec<_> = parse_code_actions(&v).into_iter().map(|a| a.title).collect();
        assert_eq!(titles, ["Best fix", "Import `HashMap`", "Extract into function"]);
        assert!(parse_code_actions(&Value::Null).is_empty());
    }

    #[test]
    fn kinds_map_to_badges() {
        let a = |k: &str| CodeAction { title: String::new(), kind: k.into(), preferred: false, edits: vec![] }.kind_number();
        assert_eq!((a("quickfix"), a("refactor.extract"), a("source.organizeImports"), a("")), (101, 102, 103, 100));
    }
}
