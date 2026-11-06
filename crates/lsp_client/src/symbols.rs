//! `textDocument/documentSymbol`, flattened into outline rows.

use crate::client::{Client, Error};
use serde_json::{Value, json};

/// One outline entry. `depth` is the nesting level, `line`/`col` the LSP
/// position (UTF-16 column) of the symbol's name, and `kind` the LSP `SymbolKind` number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub detail: String,
    pub kind: u32,
    pub depth: usize,
    pub line: usize,
    pub col: usize,
}

pub async fn document_symbols(client: &Client, uri: &str) -> Result<Vec<Symbol>, Error> {
    let reply = client.request_raw("textDocument/documentSymbol", json!({"textDocument": {"uri": uri}})).await?;
    Ok(parse_document_symbols(&reply))
}

/// Accepts both reply shapes: nested `DocumentSymbol[]` and flat `SymbolInformation[]`.
pub fn parse_document_symbols(v: &Value) -> Vec<Symbol> {
    let mut out = Vec::new();
    if let Some(items) = v.as_array() {
        for item in items {
            push(item, 0, &mut out);
        }
    }
    out
}

fn start(v: &Value) -> Option<(usize, usize)> {
    let s = v.get("start")?;
    Some((s.get("line")?.as_u64()? as usize, s.get("character")?.as_u64()? as usize))
}

fn push(item: &Value, depth: usize, out: &mut Vec<Symbol>) {
    let pos = item
        .get("selectionRange")
        .or_else(|| item.get("range"))
        .or_else(|| item.get("location").and_then(|l| l.get("range")))
        .and_then(start);
    if let (Some(name), Some((line, col))) = (item.get("name").and_then(Value::as_str), pos) {
        out.push(Symbol {
            name: name.to_owned(),
            detail: item.get("detail").and_then(Value::as_str).unwrap_or_default().to_owned(),
            kind: item.get("kind").and_then(Value::as_u64).unwrap_or(0) as u32,
            depth,
            line,
            col,
        });
    }
    if let Some(children) = item.get("children").and_then(Value::as_array) {
        for c in children {
            push(c, depth + 1, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_symbols_are_flattened_with_their_depth() {
        let v = json!([{
            "name": "Shape", "kind": 23, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 4, "character": 1}},
            "selectionRange": {"start": {"line": 0, "character": 7}, "end": {"line": 0, "character": 12}},
            "children": [{
                "name": "area", "detail": "fn(&self) -> f64", "kind": 6,
                "range": {"start": {"line": 2, "character": 4}, "end": {"line": 3, "character": 5}},
                "selectionRange": {"start": {"line": 2, "character": 7}, "end": {"line": 2, "character": 11}}
            }]
        }]);
        let s = parse_document_symbols(&v);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].name.as_str(), s[0].depth, s[0].line, s[0].col), ("Shape", 0, 0, 7));
        assert_eq!((s[1].name.as_str(), s[1].depth, s[1].line, s[1].col, s[1].kind), ("area", 1, 2, 7, 6));
        assert_eq!(s[1].detail, "fn(&self) -> f64");
    }

    #[test]
    fn flat_symbol_information_uses_the_location_and_a_null_reply_is_empty() {
        let v = json!([{"name": "main", "kind": 12, "location": {"uri": "file:///a.rs", "range": {"start": {"line": 3, "character": 3}, "end": {"line": 5, "character": 1}}}}]);
        let s = parse_document_symbols(&v);
        assert_eq!((s[0].name.as_str(), s[0].depth, s[0].line, s[0].col), ("main", 0, 3, 3));
        assert!(parse_document_symbols(&Value::Null).is_empty());
    }
}
