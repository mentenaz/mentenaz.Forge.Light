//! Signature help: the tooltip shown while filling in a call.

use crate::client::{Client, Error};
use crate::completion::Trigger;
use crate::diag::utf16_to_char_col;
use crate::requests::plain_text;
use serde_json::{Value, json};

/// The signature in effect at the caret, with the parameter being typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureHelp {
    pub label: String,
    /// The active parameter as a char range inside `label`, if known.
    pub active: Option<(usize, usize)>,
    /// Documentation of the function.
    pub doc: String,
    /// Documentation of the active parameter.
    pub param_doc: String,
}

pub async fn signature_help(client: &Client, uri: &str, pos: (usize, usize), trigger: Trigger) -> Result<Option<SignatureHelp>, Error> {
    let context = match trigger {
        Trigger::Invoked => json!({"triggerKind": 1, "isRetrigger": false}),
        Trigger::Character(c) => json!({"triggerKind": 2, "triggerCharacter": c.to_string(), "isRetrigger": false}),
    };
    let params = json!({
        "textDocument": {"uri": uri},
        "position": {"line": pos.0, "character": pos.1},
        "context": context,
    });
    let reply = client.request_raw("textDocument/signatureHelp", params).await?;
    Ok(parse_signature_help(&reply))
}

fn doc_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => plain_text(s),
        Some(Value::Object(o)) => o.get("value").and_then(Value::as_str).map(plain_text).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Picks the active signature and parameter out of a `SignatureHelp` (or null).
pub fn parse_signature_help(v: &Value) -> Option<SignatureHelp> {
    let signatures = v.get("signatures")?.as_array()?;
    let index = v.get("activeSignature").and_then(Value::as_u64).unwrap_or(0) as usize;
    let sig = signatures.get(index).or_else(|| signatures.first())?;
    let label = sig.get("label")?.as_str()?.to_owned();
    // The signature's own activeParameter wins over the top-level one; absent means the first.
    let active_index = sig.get("activeParameter").or_else(|| v.get("activeParameter")).and_then(Value::as_u64).unwrap_or(0) as usize;
    let param = sig.get("parameters").and_then(Value::as_array).and_then(|p| p.get(active_index));

    let active = param.and_then(|p| match p.get("label")? {
        // A text label: find it inside the signature, after the opening parenthesis.
        Value::String(text) => {
            let from = label.find('(').map_or(0, |i| i + 1);
            let at = label[from..].find(text.as_str())? + from;
            let start = label[..at].chars().count();
            Some((start, start + text.chars().count()))
        }
        // [start, end) as UTF-16 offsets into the label.
        Value::Array(range) => {
            let (a, b) = (range.first()?.as_u64()? as usize, range.get(1)?.as_u64()? as usize);
            Some((utf16_to_char_col(&label, a), utf16_to_char_col(&label, b)))
        }
        _ => None,
    });
    Some(SignatureHelp { label, active, doc: doc_text(sig.get("documentation")), param_doc: doc_text(param.and_then(|p| p.get("documentation"))) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_labels_are_located_after_the_open_paren() {
        let v = json!({"signatures": [{
            "label": "fn x(x: u8, y: u8) -> u8",
            "documentation": {"kind": "markdown", "value": "Adds."},
            "parameters": [{"label": "x: u8", "documentation": "the first"}, {"label": "y: u8"}]
        }], "activeSignature": 0, "activeParameter": 1});
        let h = parse_signature_help(&v).unwrap();
        assert_eq!(h.label, "fn x(x: u8, y: u8) -> u8");
        let (a, b) = h.active.unwrap();
        assert_eq!(&h.label[a..b], "y: u8");
        assert_eq!(h.doc, "Adds.");
        assert_eq!(h.param_doc, "");
        // The first parameter is found after `(`, not at the `x` in the function name.
        let first = parse_signature_help(&json!({"signatures": [{
            "label": "fn x(x: u8)", "parameters": [{"label": "x: u8", "documentation": "the first"}]}]})).unwrap();
        let (a, b) = first.active.unwrap();
        assert_eq!((a, b), (5, 10));
        assert_eq!(first.param_doc, "the first");
    }

    #[test]
    fn offset_labels_use_utf16_units() {
        // 😀 is two UTF-16 units, so the parameter at offsets [9, 14) starts at char 8.
        let v = json!({"signatures": [{"label": "f(😀: u8, b: u8)", "parameters": [{"label": [2, 8]}, {"label": [10, 14]}]}], "activeParameter": 1});
        let h = parse_signature_help(&v).unwrap();
        let chars: Vec<char> = h.label.chars().collect();
        let (a, b) = h.active.unwrap();
        assert_eq!(chars[a..b].iter().collect::<String>(), "b: u");
        let first = parse_signature_help(&json!({"signatures": [{"label": "f(😀: u8)", "parameters": [{"label": [2, 8]}]}]})).unwrap();
        let (a, b) = first.active.unwrap();
        assert_eq!(first.label.chars().skip(a).take(b - a).collect::<String>(), "😀: u8");
    }

    #[test]
    fn no_signature_no_help_and_missing_parts_are_fine() {
        assert!(parse_signature_help(&Value::Null).is_none());
        assert!(parse_signature_help(&json!({"signatures": []})).is_none());
        let h = parse_signature_help(&json!({"signatures": [{"label": "f()"}]})).unwrap();
        assert_eq!(h.active, None);
        // Out of range index: nothing highlighted.
        let h = parse_signature_help(&json!({"signatures": [{"label": "f(a)", "parameters": [{"label": "a"}]}], "activeParameter": 5})).unwrap();
        assert_eq!(h.active, None);
        // Second signature chosen.
        let h = parse_signature_help(&json!({"signatures": [{"label": "one()"}, {"label": "two()"}], "activeSignature": 1})).unwrap();
        assert_eq!(h.label, "two()");
    }
}
